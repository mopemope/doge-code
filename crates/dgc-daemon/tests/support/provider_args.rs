use std::path::{Path, PathBuf};

/// Capture the real argv before handing off to the existing fake provider.
pub fn capture_args(capture: &Path, scenario: &Path) -> (PathBuf, Vec<String>) {
    (
        PathBuf::from("/bin/sh"),
        vec![
            "-c".to_owned(),
            r#"capture=$1; shift; printf '%s\0' "$@" > "$capture" && exec "$0" "$@""#.to_owned(),
            env!("CARGO_BIN_EXE_dgc-fake-provider").to_owned(),
            capture.to_str().expect("capture path").to_owned(),
            scenario.to_str().expect("scenario path").to_owned(),
        ],
    )
}

pub fn read_args(capture: &Path) -> Vec<String> {
    let captured = std::fs::read_to_string(capture).expect("captured argv");
    // The first argument is the scenario, followed by the provider's argv.
    captured
        .split_terminator('\0')
        .skip(1)
        .map(str::to_owned)
        .collect()
}
