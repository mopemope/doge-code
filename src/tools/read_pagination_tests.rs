use super::read::{FsReadOptions, fs_read};
use super::read_many::{FsReadManyOptions, fs_read_many_files};
use crate::config::AppConfig;

fn fixture(content: &str) -> (tempfile::TempDir, AppConfig, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.txt");
    std::fs::write(&path, content).unwrap();
    let config = AppConfig {
        project_root: dir.path().to_path_buf(),
        ..Default::default()
    };
    (dir, config, path.to_str().unwrap().to_owned())
}

#[test]
fn read_pagination_complete_lines_without_holes() {
    let expected: Vec<_> = (0..500)
        .map(|i| format!("{i:04}{}", "x".repeat(96)))
        .collect();
    let (_dir, config, path) = fixture(&expected.join("\n"));
    let mut cursor = None;
    let mut returned = Vec::new();
    for _ in 0..500 {
        let page = fs_read(
            &path,
            FsReadOptions {
                cursor,
                ..Default::default()
            },
            &config,
        )
        .unwrap();
        let lines: Vec<_> = page.content.lines().map(str::to_owned).collect();
        assert_eq!(page.end_line, page.start_line + lines.len() - 1);
        assert!(page.content.chars().count() <= 6000);
        returned.extend(lines);
        match page.next_cursor {
            Some(next) => {
                assert!(next > cursor.unwrap_or(1));
                cursor = Some(next);
            }
            None => break,
        }
    }
    assert_eq!(returned, expected);
}

#[test]
fn read_pagination_unicode_and_invalid_budgets() {
    let (_dir, config, path) = fixture("あ\n😀\ne\u{301}");
    for budget in [1, 2] {
        let page = fs_read(
            &path,
            FsReadOptions {
                response_budget_chars: Some(budget),
                ..Default::default()
            },
            &config,
        )
        .unwrap();
        assert_eq!(page.content, "あ");
        assert_eq!(page.next_cursor, Some(2));
        let many = fs_read_many_files(
            vec![path.clone()],
            None,
            None,
            &config,
            FsReadManyOptions {
                snippet_max_chars: Some(budget),
                response_budget_chars: Some(budget),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(many.files[0].snippet.chars().count() <= budget);
    }
    for opts in [
        FsReadOptions {
            response_budget_chars: Some(0),
            ..Default::default()
        },
        FsReadOptions {
            limit: Some(0),
            ..Default::default()
        },
        FsReadOptions {
            page_size: Some(0),
            ..Default::default()
        },
    ] {
        assert!(fs_read(&path, opts, &config).is_err());
    }
    for opts in [
        FsReadManyOptions {
            response_budget_chars: Some(0),
            ..Default::default()
        },
        FsReadManyOptions {
            snippet_max_chars: Some(0),
            ..Default::default()
        },
        FsReadManyOptions {
            page_size: Some(0),
            ..Default::default()
        },
    ] {
        assert!(fs_read_many_files(vec![path.clone()], None, None, &config, opts).is_err());
    }
}

#[test]
fn read_pagination_many_budget_last_page_never_skips_files() {
    let (dir, config, _) = fixture("");
    let paths: Vec<_> = (0..3)
        .map(|i| {
            let p = dir.path().join(format!("{i}.txt"));
            std::fs::write(&p, "x".repeat(100)).unwrap();
            p.to_str().unwrap().to_owned()
        })
        .collect();
    let mut cursor = None;
    let mut returned = Vec::new();
    for _ in 0..4 {
        let page = fs_read_many_files(
            paths.clone(),
            None,
            None,
            &config,
            FsReadManyOptions {
                page_size: Some(3),
                response_budget_chars: Some(150),
                cursor,
                ..Default::default()
            },
        )
        .unwrap();
        returned.extend(page.files.iter().map(|file| file.path.clone()));
        match page.next_cursor {
            Some(next) => {
                assert!(next > cursor.unwrap_or(0));
                cursor = Some(next);
            }
            None => break,
        }
    }
    assert_eq!(returned, paths);
}

#[test]
fn read_pagination_eof_empty_crlf_and_extreme_sizes() {
    let (_dir, config, path) = fixture("first\r\n\r\nlast\r\n");
    let full = fs_read(
        &path,
        FsReadOptions {
            page_size: Some(usize::MAX),
            ..Default::default()
        },
        &config,
    )
    .unwrap();
    assert_eq!(full.content, "first\n\nlast");
    assert_eq!(full.end_line, 3);
    let eof = fs_read(
        &path,
        FsReadOptions {
            cursor: Some(usize::MAX),
            page_size: Some(usize::MAX),
            ..Default::default()
        },
        &config,
    )
    .unwrap();
    assert!(eof.content.is_empty());
    assert_eq!(eof.next_cursor, None);
    assert!(!eof.truncated);
    std::fs::write(&path, "").unwrap();
    let empty = fs_read(&path, FsReadOptions::default(), &config).unwrap();
    assert_eq!(
        (empty.start_line, empty.end_line, empty.total_lines),
        (0, 0, 0)
    );
    std::fs::write(&path, format!("{}\nsmall", "x".repeat(45_000))).unwrap();
    let error = fs_read(
        &path,
        FsReadOptions {
            response_budget_chars: Some(usize::MAX),
            ..Default::default()
        },
        &config,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("45000") && error.contains("same start_line=1"));
    std::fs::write(&path, "abc\ndef").unwrap();
    let error = fs_read(
        &path,
        FsReadOptions {
            response_budget_chars: Some(2),
            ..Default::default()
        },
        &config,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("at least 3"));
}

#[test]
fn read_pagination_many_skips_directory_but_not_next_file() {
    let (dir, config, empty) = fixture("");
    let skip = dir.path().join("directory");
    std::fs::create_dir(&skip).unwrap();
    let one = dir.path().join("one.txt");
    let two = dir.path().join("two.txt");
    std::fs::write(&one, "あ😀e\u{301}").unwrap();
    std::fs::write(&two, "tail").unwrap();
    let paths = vec![
        empty.clone(),
        skip.to_str().unwrap().into(),
        one.to_str().unwrap().into(),
        two.to_str().unwrap().into(),
    ];
    let first = fs_read_many_files(
        paths.clone(),
        None,
        None,
        &config,
        FsReadManyOptions {
            page_size: Some(usize::MAX),
            max_entries: Some(usize::MAX),
            response_budget_chars: Some(4),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(first.files.len(), 2);
    assert_eq!(first.files[1].snippet, "あ😀e\u{301}");
    assert_eq!(first.next_cursor, Some(3));
    let second = fs_read_many_files(
        paths.clone(),
        None,
        None,
        &config,
        FsReadManyOptions {
            cursor: first.next_cursor,
            page_size: Some(usize::MAX),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(second.files[0].path, two.to_str().unwrap());
    assert_eq!(second.next_cursor, None);
    let summary = fs_read_many_files(
        vec![one.to_str().unwrap().into()],
        None,
        None,
        &config,
        FsReadManyOptions {
            response_budget_chars: Some(1),
            snippet_max_chars: Some(2),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(summary.files[0].snippet, "あ");
    assert!(summary.files[0].truncated);
    assert!(summary.warnings.iter().any(|w| w.contains("fs_read")));
    let page = fs_read_many_files(
        paths.clone(),
        None,
        None,
        &config,
        FsReadManyOptions {
            page_size: Some(3),
            max_entries: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(page.next_cursor, Some(1));
    let eof = fs_read_many_files(
        paths,
        None,
        None,
        &config,
        FsReadManyOptions {
            cursor: Some(usize::MAX),
            page_size: Some(usize::MAX),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(eof.files.is_empty());
    assert_eq!(eof.next_cursor, None);
}

#[test]
fn read_pagination_serialized_escape_cap_keeps_cursor_and_lines() {
    let line = "\"\\\u{0001}".repeat(1000);
    let expected = vec![line.clone(); 10];
    let (_dir, config, path) = fixture(&expected.join("\n"));
    let mut cursor = None;
    let mut returned = Vec::new();
    for _ in 0..11 {
        let page = fs_read(
            &path,
            FsReadOptions {
                cursor,
                response_budget_chars: Some(usize::MAX),
                page_size: Some(usize::MAX),
                ..Default::default()
            },
            &config,
        )
        .unwrap();
        let encoded = serde_json::json!({"ok":true,"result":page}).to_string();
        assert!(encoded.chars().count() <= super::budget::READ_TOOL_OUTPUT_MAX_CHARS);
        assert_eq!(
            crate::llm::truncate_tool_output(encoded.clone(), "fs_read"),
            encoded
        );
        returned.extend(page.content.lines().map(str::to_owned));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(returned, expected);
    std::fs::write(&path, "\u{0001}".repeat(7000)).unwrap();
    assert!(
        fs_read(
            &path,
            FsReadOptions {
                response_budget_chars: Some(40_000),
                ..Default::default()
            },
            &config
        )
        .unwrap_err()
        .to_string()
        .contains("serialized JSON limit")
    );
    let many = fs_read_many_files(
        vec![path.clone(), path.clone()],
        None,
        None,
        &config,
        FsReadManyOptions {
            response_budget_chars: Some(usize::MAX),
            snippet_max_chars: Some(usize::MAX),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(many.files[0].truncated);
    assert_eq!(many.next_cursor, Some(1));
    let encoded = serde_json::json!({"ok":true,"result":many}).to_string();
    assert!(encoded.chars().count() <= 40_000);
    assert_eq!(
        crate::llm::truncate_tool_output(encoded.clone(), "fs_read_many_files"),
        encoded
    );
}

#[test]
fn read_pagination_many_short_control_lines_preserve_complete_prefixes() {
    let (_dir, config, path) = fixture(&vec!["\u{0001}"; 20_000].join("\n"));
    let mut cursor = None;
    let mut returned = 0;
    for _ in 0..10 {
        let page = fs_read(
            &path,
            FsReadOptions {
                cursor,
                page_size: Some(usize::MAX),
                response_budget_chars: Some(40_000),
                ..Default::default()
            },
            &config,
        )
        .unwrap();
        assert!(page.content.lines().all(|line| line == "\u{0001}"));
        returned += page.content.lines().count();
        assert!(super::budget::read_result_fits(&page).unwrap());
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(returned, 20_000);
}

#[test]
fn read_pagination_long_paths_scope_and_metadata_cap() {
    let (dir, mut config, _) = fixture("");
    let mut nested = dir.path().to_path_buf();
    for _ in 0..10 {
        nested = nested.join(format!("{}\"", "a".repeat(190)));
    }
    std::fs::create_dir_all(&nested).unwrap();
    let path = nested.join("quoted\".txt");
    std::fs::write(&path, vec!["\"\\\u{0001}".repeat(1000); 10].join("\n")).unwrap();
    let page = fs_read(
        path.to_str().unwrap(),
        FsReadOptions {
            response_budget_chars: Some(40_000),
            ..Default::default()
        },
        &config,
    )
    .unwrap();
    assert!(super::budget::read_result_fits(&page).unwrap());
    assert!(page.next_cursor.is_some());
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("secret.txt");
    std::fs::write(&target, "private").unwrap();
    assert!(fs_read(target.to_str().unwrap(), FsReadOptions::default(), &config).is_err());
    assert!(fs_read("relative.txt", FsReadOptions::default(), &config).is_err());
    #[cfg(unix)]
    {
        let alias = dir.path().join("outside-link.txt");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert!(fs_read(alias.to_str().unwrap(), FsReadOptions::default(), &config).is_err());
        let batch = fs_read_many_files(
            vec![alias.to_str().unwrap().into()],
            None,
            None,
            &config,
            FsReadManyOptions::default(),
        )
        .unwrap();
        assert!(batch.files.is_empty());
    }
    config.allowed_paths.push(outside.path().to_path_buf());
    assert_eq!(
        fs_read(target.to_str().unwrap(), FsReadOptions::default(), &config)
            .unwrap()
            .content,
        "private"
    );
    let oversized = super::read::FsReadResult {
        path: "\u{0001}".repeat(7000),
        content: String::new(),
        start_line: 0,
        end_line: 0,
        total_lines: 0,
        truncated: false,
        next_cursor: None,
        warnings: Vec::new(),
    };
    assert!(!super::budget::read_result_fits(&oversized).unwrap());
}

#[tokio::test]
async fn read_streaming_dispatch_and_mcp_keep_results_and_error_semantics() -> anyhow::Result<()> {
    use crate::llm::tool_execution::dispatch_tool_call;
    use crate::llm::tool_runtime::ToolRuntime;
    use crate::llm::types::{ToolCall, ToolCallFunction};
    use crate::mcp::service::{DogeMcpService, McpServiceState};
    use rmcp::handler::server::wrapper::Parameters;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let (_dir, config, path) = fixture("あ\r\n😀\r\nlast\r");
    let config = Arc::new(config);
    let fs = super::FsTools::new(Arc::new(RwLock::new(None)), config.clone());
    let runtime = ToolRuntime::build(&fs, None, "fixture", None).await?;
    runtime
        .tool_catalog
        .activate(&["fs_read".into(), "fs_read_many_files".into()])
        .await;
    let service = DogeMcpService::new(Arc::new(McpServiceState::new(
        config,
        Arc::new(RwLock::new(None)),
    )));

    for (bytes, budget, read_ok, many_ok) in [
        ("あ\r\n😀\r\nlast\r".as_bytes().to_vec(), 20, true, true),
        // A valid selected page/snippet does not conceal malformed unread content.
        (b"ok\n\xff".to_vec(), 20, false, false),
        (b"abcdef".to_vec(), 2, false, true),
        (b"ok".to_vec(), 0, false, false),
    ] {
        std::fs::write(&path, bytes)?;
        for (name, expected_ok) in [("fs_read", read_ok), ("fs_read_many_files", many_ok)] {
            let args = json!({"path":path,"paths":[path],"page_size":1,"response_budget_chars":budget,"snippet_max_chars":10});
            let call = ToolCall {
                id: Some("bounded-read".into()),
                r#type: "function".into(),
                function: ToolCallFunction {
                    name: name.into(),
                    arguments: args.to_string(),
                },
            };
            let dispatched = dispatch_tool_call(&runtime, &call).await;
            assert_eq!(dispatched.is_ok(), expected_ok, "{name}: {dispatched:?}");
            let mcp = if name == "fs_read" {
                service.fs_read(Parameters(serde_json::from_value(args)?))?
            } else {
                service.fs_read_many_files(Parameters(serde_json::from_value(args)?))?
            };
            assert_eq!(mcp.is_error == Some(true), !expected_ok);
            if let Ok(output) = dispatched {
                let encoded = serde_json::to_value(&mcp)?;
                let value: serde_json::Value =
                    serde_json::from_str(encoded["content"][0]["text"].as_str().unwrap())?;
                assert_eq!(value, output.value["result"]);
                if budget == 20 {
                    if name == "fs_read" {
                        assert_eq!(value["content"], "あ");
                        assert_eq!(value["next_cursor"], 2);
                    } else {
                        assert_eq!(value["files"][0]["snippet"], "あ\n😀\nlast\r");
                    }
                } else {
                    assert_eq!(value["files"][0]["snippet"], "ab");
                    assert_eq!(value["files"][0]["truncated"], true);
                }
            } else if budget == 2 {
                assert!(
                    dispatched
                        .unwrap_err()
                        .to_string()
                        .contains("requires at least 6 Unicode characters")
                );
            }
        }
    }
    Ok(())
}

#[test]
fn read_streaming_many_full_keeps_original_line_separators_and_summary_boundary() {
    use super::read::FsReadMode;
    let content = (0..42).map(|i| format!("{i}あ\r\n")).collect::<String>();
    let (_dir, config, path) = fixture(&content);
    for mode in [FsReadMode::Full, FsReadMode::Summary] {
        let result = fs_read_many_files(
            vec![path.clone()],
            None,
            None,
            &config,
            FsReadManyOptions {
                mode,
                response_budget_chars: Some(40000),
                snippet_max_chars: Some(40000),
                ..Default::default()
            },
        )
        .unwrap();
        let expected = if mode == FsReadMode::Full {
            content.clone()
        } else {
            content.lines().take(40).collect::<Vec<_>>().join("\n")
        };
        assert_eq!(result.files[0].snippet, expected);
        assert_eq!(result.files[0].total_lines, 42);
        assert_eq!(result.files[0].truncated, mode == FsReadMode::Summary);
    }
}
