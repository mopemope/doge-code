use std::process::Command;

#[test]
fn dgc_reports_its_name_and_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_dgc"))
        .arg("--version")
        .output()
        .expect("dgc should start");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("version output is UTF-8");
    assert_eq!(stdout.trim(), "dgc 0.1.0");
}
