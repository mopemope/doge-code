//! Literal path arguments for `/verify refresh`; no shell expansion or escaping.
pub fn parse(input: &str) -> anyhow::Result<Vec<String>> {
    let mut chars = input.chars().peekable();
    let mut paths = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        let mut path = String::new();
        if c == '\'' || c == '"' {
            let mut closed = false;
            for next in chars.by_ref() {
                if next == c {
                    closed = true;
                    break;
                }
                path.push(next);
            }
            anyhow::ensure!(closed, "Unclosed path quote.");
            anyhow::ensure!(
                chars.peek().is_none_or(|c| c.is_whitespace()),
                "A quoted path must be followed by whitespace or the end of input."
            );
        } else {
            path.push(c);
            while let Some(c) = chars.peek().copied().filter(|c| !c.is_whitespace()) {
                chars.next();
                path.push(c);
            }
        }
        anyhow::ensure!(!path.is_empty(), "Empty path arguments are not supported.");
        paths.push(path);
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verify_path_literals_do_not_expand_or_unescape() {
        for (input, expected) in [
            (r#""日本語 の.rs""#, "日本語 の.rs"),
            (
                r#"" $HOME/*.rs;$(touch marker) ""#,
                " $HOME/*.rs;$(touch marker) ",
            ),
            (r#"'a"b.rs'"#, "a\"b.rs"),
            (r#""a'b.rs""#, "a'b.rs"),
            ("a'b.rs", "a'b.rs"),
            (r#""src\a b.rs""#, r"src\a b.rs"),
        ] {
            assert_eq!(parse(input).expect("literal"), vec![expected]);
        }
        assert!(parse(" \t\n").expect("empty list").is_empty());
    }
}
