//! Best-effort human-facing projection. Never apply to canonical checkpoints,
//! provider state, provenance, machine tool results or authorization decisions.
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

const MASK: &str = "[REDACTED]";
static CONTROLS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?s)(?:\x1b\]|\x{009d}).*?(?:\x07|\x1b\\|\x{009c}|$)",
        r"|\x1b[P^_X].*?(?:\x1b\\|$)",
        r"|(?:\x1b\[|\x{009b})[0-?]*[ -/]*[@-~]",
    ))
    .expect("terminal controls regex")
});
static HEADERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)(\b(?:authorization|proxy-authorization|cookie|set-cookie)\s*:\s*)[^\r\n]+")
        .expect("credential header regex")
});
static USERINFO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(https?://)[^/@\s]+@").expect("URL credentials regex"));
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(\b[\w.-]*(?:api[_-]?key|(?:access|auth)?[_-]?token|refresh[_-]?token|id[_-]?token|password|passwd|secret|authorization|cookie|private[_-]?key)["']?\s*[:=]\s*)("[^"\r\n]*"|'[^'\r\n]*'|[^\s,;]+)"#).expect("credential assignment regex")
});
static FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(--(?:api-key|token|access-token|password|secret)(?:=|\s+))(?:"[^"\r\n]*"|'[^'\r\n]*'|[^\s,;]+)"#).expect("credential flag regex")
});
static AUTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:Bearer|Basic)\s+[a-z0-9_./+~=-]+").expect("authorization regex")
});
static KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:sk-[A-Za-z0-9_-]{8,}|gh[pousr]_[A-Za-z0-9_]{8,}|github_pat_[A-Za-z0-9_]{8,})")
        .expect("key regex")
});
static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----")
        .expect("private key regex")
});

pub(crate) fn terminal_text(input: &str) -> String {
    CONTROLS
        .replace_all(input, "")
        .chars()
        .filter(|c| {
            (!c.is_control() || matches!(c, '\n' | '\t'))
                && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect()
}

fn sensitive(name: &str) -> bool {
    let name: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    name == "token"
        || [
            "apikey",
            "accesstoken",
            "refreshtoken",
            "idtoken",
            "authtoken",
            "password",
            "passwd",
            "secret",
            "authorization",
            "cookie",
            "privatekey",
            "encryptedcontent",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

fn plain(input: &str, known_secret: Option<&str>) -> String {
    let mut output = terminal_text(input);
    if let Some(secret) = known_secret.filter(|secret| !secret.is_empty()) {
        output = output.replace(secret, MASK);
    }
    output = HEADERS.replace_all(&output, "${1}[REDACTED]").into_owned();
    output = USERINFO
        .replace_all(&output, "${1}[REDACTED]@")
        .into_owned();
    output = PRIVATE_KEY.replace_all(&output, MASK).into_owned();
    output = AUTH.replace_all(&output, MASK).into_owned();
    output = KEY.replace_all(&output, MASK).into_owned();
    output = ASSIGNMENT
        .replace_all(&output, |captures: &regex::Captures<'_>| {
            let value = captures[2].trim_matches(['\"', '\'']);
            if crate::features::human_review::valid_token(value) {
                captures[0].to_string()
            } else {
                format!("{}{}", &captures[1], MASK)
            }
        })
        .into_owned();
    FLAG.replace_all(&output, "${1}[REDACTED]").into_owned()
}

pub(crate) fn text(input: &str, known_secret: Option<&str>) -> String {
    project_text(input, known_secret, 0)
}

fn project_text(input: &str, secret: Option<&str>, depth: usize) -> String {
    if depth < 8
        && input.trim_start().starts_with(['{', '['])
        && let Ok(mut json) = serde_json::from_str::<Value>(input)
    {
        visit(&mut json, secret, depth + 1, "");
        return json.to_string();
    }
    plain(input, secret)
}

pub(crate) fn json(mut value: Value) -> Value {
    visit(&mut value, None, 0, "");
    value
}

fn visit(value: &mut Value, secret: Option<&str>, depth: usize, parent: &str) {
    match value {
        Value::String(s) => *s = project_text(s, secret, depth),
        Value::Array(values) => {
            let mut mask_next = false;
            for value in values {
                let flag = value
                    .as_str()
                    .is_some_and(|s| s.starts_with("--") && sensitive(s.trim_start_matches('-')));
                if mask_next && value.is_string() {
                    *value = MASK.into();
                } else {
                    visit(value, secret, depth, parent);
                }
                mask_next = flag;
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                // The local human-review operation token is an integrity identifier,
                // not a provider credential. Keep it usable after export.
                if sensitive(key)
                    && !(parent == "review_target"
                        && key == "token"
                        && value
                            .as_str()
                            .is_some_and(crate::features::human_review::valid_token))
                    && !value.is_null()
                    && !(key == "token" && value.is_number())
                {
                    *value = MASK.into();
                } else {
                    visit(value, secret, depth, key);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_masks_credentials_controls_and_keeps_operation_identifiers() {
        let input = serde_json::json!({"tool_call_id":"call-1", "token_count":42, "review_target":{"token":"review-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}, "arguments":r#"{"args":["--token","FLAG_SECRET"],"env":{"API_KEY":"ENV_SECRET"}}"#, "result":{"access_token":"RESULT_SECRET", "stdout":"Authorization: Bearer HEADER_SECRET\nAPI_KEY=TEXT_SECRET\ntoken=PLAIN_TOKEN\nauth_token=AUTH_TOKEN\nCookie: session=COOKIE_SECRET; other=SECOND_COOKIE\nhttps://user:URL_SECRET@example.invalid/\nknown-exact-value\u{1b}]0;INJECT\u{7}\u{1b}[31mvisible\r"}});
        let mut output = input.clone();
        visit(&mut output, Some("known-exact-value"), 0, "");
        let rendered = output.to_string();
        for secret in [
            "FLAG_SECRET",
            "ENV_SECRET",
            "RESULT_SECRET",
            "HEADER_SECRET",
            "TEXT_SECRET",
            "PLAIN_TOKEN",
            "AUTH_TOKEN",
            "COOKIE_SECRET",
            "SECOND_COOKIE",
            "URL_SECRET",
            "known-exact-value",
            "INJECT",
            "\\u001b",
            "\r",
        ] {
            assert!(!rendered.contains(secret), "{rendered}");
        }
        assert_eq!(output["tool_call_id"], "call-1");
        assert_eq!(output["token_count"], 42);
        assert_eq!(
            output["review_target"]["token"],
            "review-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(rendered.contains("visible"));
        assert!(input.to_string().contains("FLAG_SECRET"));
    }
}
