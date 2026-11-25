//! Scenario-replay fake provider for adapter tests.
//!
//! Usage: `dgc-fake-provider <scenario.jsonl> [provider args...]`. The format is
//! documented in `crates/dgc-daemon/tests/fixtures/fake/README.md`.
//!
//! Exit codes: 0 scenario finished, 2 scenario/usage error, 3 mismatch (stdin
//! line, argv, or unusable capture), 4 timeout, 5 stdio failure.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::process::exit;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

const ANY: &str = "${ANY}";
const DEFAULT_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug)]
enum Step {
    ExpectArgs(Vec<String>),
    Expect(Value),
    Send(Value),
    Capture { name: String, path: String },
    Stderr(String),
    SleepMs(u64),
    Exit(i32),
    ExpectEof,
}

/// Terminal failure: message for stderr plus the process exit code.
struct Fail {
    code: i32,
    message: String,
}

fn fail(code: i32, message: impl Into<String>) -> Fail {
    Fail {
        code,
        message: message.into(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(f) = run(&args) {
        eprintln!("dgc-fake-provider: {}", f.message);
        exit(f.code);
    }
}

fn run(args: &[String]) -> Result<(), Fail> {
    let Some((scenario_path, provider_args)) = args.split_first() else {
        return Err(fail(
            2,
            "usage: dgc-fake-provider <scenario.jsonl> [args...]",
        ));
    };
    let text = std::fs::read_to_string(scenario_path)
        .map_err(|e| fail(2, format!("cannot read scenario {scenario_path}: {e}")))?;
    let steps = parse_scenario(&text)?;
    let timeout = timeout_from_env()?;
    let stdin = spawn_stdin_reader();
    let mut stdout = std::io::stdout();
    let mut captures: HashMap<String, Value> = HashMap::new();
    let mut last_expected: Option<Value> = None;

    for step in steps {
        match step {
            Step::ExpectArgs(pattern) => check_args(&pattern, provider_args)?,
            Step::Expect(pattern) => {
                let line = next_line(&stdin, timeout, "expect")?
                    .ok_or_else(|| fail(3, "stdin closed while waiting for an expected line"))?;
                let received: Value = serde_json::from_str(&line)
                    .map_err(|e| fail(3, format!("received line is not JSON ({e}): {line}")))?;
                if let Err(at) = matches(&pattern, &received, &captures, "") {
                    return Err(fail(
                        3,
                        format!(
                            "mismatch at {at}\n  expected (subset): {pattern}\n  received: {received}"
                        ),
                    ));
                }
                last_expected = Some(received);
            }
            Step::Send(value) => {
                let value = substitute(&value, &captures)?;
                let io = writeln!(stdout, "{value}").and_then(|()| stdout.flush());
                io.map_err(|e| fail(5, format!("cannot write stdout: {e}")))?;
            }
            Step::Capture { name, path } => {
                let source = last_expected
                    .as_ref()
                    .ok_or_else(|| fail(2, "capture before any expect"))?;
                let value = source
                    .pointer(&path)
                    .ok_or_else(|| fail(3, format!("capture path {path} not found in {source}")))?;
                captures.insert(name, value.clone());
            }
            Step::Stderr(text) => eprintln!("{text}"),
            Step::SleepMs(ms) => std::thread::sleep(Duration::from_millis(ms)),
            Step::Exit(code) => {
                let _ = stdout.flush();
                exit(code);
            }
            Step::ExpectEof => wait_for_eof(&stdin, timeout)?,
        }
    }
    // End of scenario without `exit`: wait for stdin to close, ignoring input.
    wait_for_eof(&stdin, timeout)
}

fn timeout_from_env() -> Result<Duration, Fail> {
    match std::env::var("DGC_FAKE_TIMEOUT_MS") {
        Ok(v) => v
            .parse::<u64>()
            .map(Duration::from_millis)
            .map_err(|_| fail(2, format!("invalid DGC_FAKE_TIMEOUT_MS: {v}"))),
        Err(_) => Ok(Duration::from_millis(DEFAULT_TIMEOUT_MS)),
    }
}

fn parse_scenario(text: &str) -> Result<Vec<Step>, Fail> {
    let mut steps = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let err = |m: &str| fail(2, format!("scenario line {no}: {m}"));
        let value: Value = serde_json::from_str(line).map_err(|e| err(&e.to_string()))?;
        let obj = value.as_object().ok_or_else(|| err("not a JSON object"))?;
        let step = if let Some(name) = obj.get("capture") {
            let name = name
                .as_str()
                .ok_or_else(|| err("capture must be a string"))?;
            let path = obj
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| err("capture needs a string \"path\""))?;
            Step::Capture {
                name: name.to_owned(),
                path: path.to_owned(),
            }
        } else if obj.len() != 1 {
            return Err(err("expected exactly one action key"));
        } else if let Some(v) = obj.get("expect_args") {
            if !steps.is_empty() {
                return Err(err("expect_args must be the first scenario line"));
            }
            let list = v
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .map(|s| s.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| err("expect_args must be an array of strings"))?;
            Step::ExpectArgs(list)
        } else if let Some(v) = obj.get("expect") {
            Step::Expect(v.clone())
        } else if let Some(v) = obj.get("send") {
            Step::Send(v.clone())
        } else if let Some(v) = obj.get("stderr") {
            Step::Stderr(
                v.as_str()
                    .ok_or_else(|| err("stderr must be a string"))?
                    .to_owned(),
            )
        } else if let Some(v) = obj.get("sleep_ms") {
            Step::SleepMs(
                v.as_u64()
                    .ok_or_else(|| err("sleep_ms must be an unsigned integer"))?,
            )
        } else if let Some(v) = obj.get("exit") {
            let code = v
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .ok_or_else(|| err("exit must be an integer"))?;
            Step::Exit(code)
        } else if obj.get("expect_eof") == Some(&Value::Bool(true)) {
            Step::ExpectEof
        } else {
            return Err(err("unknown action"));
        };
        steps.push(step);
    }
    Ok(steps)
}

/// Asserts `pattern` appears in `args` as an ordered subsequence.
fn check_args(pattern: &[String], args: &[String]) -> Result<(), Fail> {
    let mut rest = args.iter();
    for want in pattern {
        if !rest.any(|got| want == ANY || got == want) {
            return Err(fail(
                3,
                format!("argv mismatch: expected subsequence {pattern:?}, got {args:?}"),
            ));
        }
    }
    Ok(())
}

fn spawn_stdin_reader() -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(std::io::stdin().lock());
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf)
                        .trim_end_matches(['\n', '\r'])
                        .to_owned();
                    if tx.send(line).is_err() {
                        return;
                    }
                }
            }
        }
    });
    rx
}

/// `Ok(None)` means stdin reached EOF.
fn next_line(rx: &Receiver<String>, timeout: Duration, what: &str) -> Result<Option<String>, Fail> {
    match rx.recv_timeout(timeout) {
        Ok(line) => Ok(Some(line)),
        Err(RecvTimeoutError::Disconnected) => Ok(None),
        Err(RecvTimeoutError::Timeout) => Err(fail(
            4,
            format!(
                "timed out after {}ms waiting for {what}",
                timeout.as_millis()
            ),
        )),
    }
}

fn wait_for_eof(rx: &Receiver<String>, timeout: Duration) -> Result<(), Fail> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if next_line(rx, left, "stdin EOF")?.is_none() {
            return Ok(());
        }
    }
}

/// Subset match; `Err` carries the JSON-pointer-like location of the first mismatch.
fn matches(
    pattern: &Value,
    got: &Value,
    captures: &HashMap<String, Value>,
    at: &str,
) -> Result<(), String> {
    let here = || {
        if at.is_empty() {
            "/".to_owned()
        } else {
            at.to_owned()
        }
    };
    match (pattern, got) {
        (Value::String(s), _) if s == ANY => Ok(()),
        (Value::String(s), _) if capture_name(s).is_some_and(|n| captures.contains_key(n)) => {
            let name = capture_name(s).unwrap_or_default();
            if captures.get(name) == Some(got) {
                Ok(())
            } else {
                Err(here())
            }
        }
        (Value::Object(p), Value::Object(g)) => {
            for (k, pv) in p {
                let child = format!("{at}/{}", k.replace('~', "~0").replace('/', "~1"));
                let gv = g.get(k).ok_or_else(|| child.clone())?;
                matches(pv, gv, captures, &child)?;
            }
            Ok(())
        }
        (Value::Array(p), Value::Array(g)) if p.len() == g.len() => {
            for (i, (pv, gv)) in p.iter().zip(g).enumerate() {
                matches(pv, gv, captures, &format!("{at}/{i}"))?;
            }
            Ok(())
        }
        (p, g) if p == g && !p.is_object() && !p.is_array() => Ok(()),
        _ => Err(here()),
    }
}

fn capture_name(s: &str) -> Option<&str> {
    s.strip_prefix("${")?.strip_suffix('}')
}

/// Replaces JSON strings that are exactly `"${name}"` with the captured value.
fn substitute(value: &Value, captures: &HashMap<String, Value>) -> Result<Value, Fail> {
    Ok(match value {
        Value::String(s) => match capture_name(s) {
            Some(name) => captures
                .get(name)
                .cloned()
                .ok_or_else(|| fail(2, format!("send references unknown capture {s}")))?,
            None => value.clone(),
        },
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|v| substitute(v, captures))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), substitute(v, captures)?)))
                .collect::<Result<_, Fail>>()?,
        ),
        _ => value.clone(),
    })
}
