use super::*;

#[test]
fn current_rust_multiple_failures_keep_their_own_file_and_message() {
    let raw = include_str!("fixtures/rust-current-multiple-failures.txt");
    let failures = parse_rust_test_output(raw, "");
    assert_eq!(failures.len(), 2);
    let alpha = &failures[0];
    let beta = &failures[1];
    assert_eq!(alpha.name, "a::alpha");
    assert_eq!(beta.name, "b::beta");
    assert_eq!(alpha.file_path.as_deref(), Some("tests/a.rs"));
    assert_eq!(beta.file_path.as_deref(), Some("tests/b.rs"));
    assert_eq!(alpha.line_number, Some(2));
    assert_eq!(beta.line_number, Some(2));
    assert!(
        alpha.message.contains("ALPHA_PRIVATE_DETAIL")
            && !alpha.message.contains("BETA_PRIVATE_DETAIL")
    );
    assert!(
        beta.message.contains("BETA_PRIVATE_DETAIL")
            && !beta.message.contains("ALPHA_PRIVATE_DETAIL")
    );
}

#[test]
fn reversed_legacy_sections_keep_values_and_frames_local() {
    let raw = "running 2 tests\ntest alpha ... FAILED\ntest beta ... FAILED\nfailures:\n---- beta stdout ----\nthread 'beta' panicked at tests/b.rs:20:3:\nassertion `left == right` failed\n left: `B-left`\n right: `B-right`\nstack backtrace:\n 0: app::beta\n at src/b.rs:9:2\n---- alpha stdout ----\nthread 'alpha' panicked at tests/a.rs:10:4:\nassertion `left == right` failed\n left: `A-left`\n right: `A-right`\nstack backtrace:\n 0: app::alpha\n at src/a.rs:8:1\nfailures:\n alpha\n beta\ntest result: FAILED.\n";
    let tests = parse_rust_test_output(raw, "");
    assert_eq!(tests.len(), 2);
    assert_eq!(tests[0].file_path.as_deref(), Some("tests/a.rs"));
    assert_eq!(tests[1].file_path.as_deref(), Some("tests/b.rs"));
    for (test, own, other) in [
        (&tests[0], "A-left", "B-left"),
        (&tests[1], "B-left", "A-left"),
    ] {
        assert!(test.message.contains(own) && !test.message.contains(other));
        assert!(test.expected.is_none() && test.actual.is_none());
    }
    assert!(tests[0].related_files.contains(&"src/a.rs".into()));
    assert!(!tests[0].related_files.iter().any(|p| p.ends_with("b.rs")));
    assert!(tests[1].related_files.contains(&"src/b.rs".into()));
    assert!(!tests[1].related_files.iter().any(|p| p.ends_with("a.rs")));
}

#[test]
fn repeated_names_across_harnesses_do_not_collapse_or_share_locations() {
    let run = |file: &str| {
        format!(
            "running 1 test\ntest same ... FAILED\nfailures:\n---- same stdout ----\nthread 'same' (12) panicked at {file}:7:8:\nunique {file}\nfailures:\n same\ntest result: FAILED.\n"
        )
    };
    let tests = parse_rust_test_output(&(run("tests/one.rs") + &run("tests/two.rs")), "");
    assert_eq!(tests.len(), 2);
    assert_eq!(tests[0].file_path.as_deref(), Some("tests/one.rs"));
    assert_eq!(tests[1].file_path.as_deref(), Some("tests/two.rs"));
}

#[test]
fn duplicate_missing_or_unmatched_sections_never_supply_another_failure_location() {
    let raw = "running 2 tests\ntest ambiguous ... FAILED\ntest missing ... FAILED\nfailures:\n---- ambiguous stdout ----\nthread 'ambiguous' panicked at first.rs:1:2:\nfirst\n---- ambiguous stdout ----\nthread 'ambiguous' panicked at second.rs:3:4:\nsecond\n---- not_failed stdout ----\nthread 'not_failed' panicked at other.rs:5:6:\nother\nfailures:\n ambiguous\n missing\ntest result: FAILED.\n";
    let tests = parse_rust_test_output(raw, "");
    assert_eq!(tests.len(), 2);
    assert!(tests.iter().all(|t| t.file_path.is_none()
        && t.line_number.is_none()
        && t.message.is_empty()
        && t.stack_trace.is_none()));
}

#[test]
fn doctest_identity_and_spaces_in_reported_paths_remain_intact() {
    let raw = "running 1 test\ntest src/lib.rs - thing (line 12) ... FAILED\nfailures:\n---- src/lib.rs - thing (line 12) stdout ----\nthread 'main' panicked at C:\\project space\\日本語.rs:14:7:\nassertion failed: useful message\nfailures:\n src/lib.rs - thing (line 12)\ntest result: FAILED.\n";
    let tests = parse_rust_test_output(raw, "");
    assert_eq!(tests[0].name, "src/lib.rs - thing (line 12)");
    assert_eq!(
        tests[0].file_path.as_deref(),
        Some("C:\\project space\\日本語.rs")
    );
    assert_eq!(tests[0].line_number, Some(14));
}

#[test]
fn parsing_preserves_raw_process_outcomes_and_skips_std_source_frames() {
    let raw = include_str!("fixtures/rust-current-multiple-failures.txt");
    let result: TestResult = serde_json::from_value(serde_json::json!({"command":"cargo test","stdout":raw,"stderr":"compiler diagnostic","success":false,"exit_code":101,"failed_tests":[]})).unwrap();
    let tests = parse_test_output(&result, "rust");
    assert_eq!(result.stdout, raw);
    assert_eq!(result.stderr, "compiler diagnostic");
    assert!(!result.success);
    assert_eq!(result.exit_code, Some(101));
    assert!(
        tests
            .iter()
            .flat_map(|t| &t.related_files)
            .all(|p| !p.contains("rustc/") && !p.contains("rustlib/"))
    );
    assert!(!tests[0].related_files.iter().any(|p| p.ends_with("b.rs")));
}

#[test]
fn std_source_locations_and_stale_function_names_are_not_assigned_to_user_frames() {
    let trace = "0: app::pending\n1: core::ops::FnOnce::call_once\n at /toolchain/lib/rustlib/src/rust/library/core/src/ops/function.rs:250:5\n2: app::pending_again\n3: std::future::poll\n at src/reported.rs:4:2\n";
    let frames = parse_rust_stack_trace(trace);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].file_path.as_deref(), Some("src/reported.rs"));
    assert!(frames[0].function_name.is_none());
}
