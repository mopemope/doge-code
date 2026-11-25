use std::process::Command;

#[test]
fn dgc_next_reports_its_name_and_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_dgc-next"))
        .arg("--version")
        .output()
        .expect("dgc-next should start");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("version output is UTF-8");
    assert_eq!(stdout.trim(), "dgc-next 0.1.0");
}
