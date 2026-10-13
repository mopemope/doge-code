use super::*;
use tempfile::TempDir;

fn fixture() -> (TempDir, MemoryTools, PathBuf) {
    let root = tempfile::tempdir().expect("root");
    let dir = root.path().join(".doge/memory");
    std::fs::create_dir_all(&dir).expect("memory dir");
    let tools = MemoryTools::new(Arc::new(AppConfig {
        project_root: root.path().into(),
        ..Default::default()
    }));
    (root, tools, dir)
}

#[tokio::test]
async fn bounded_content_preserves_unicode_and_counts_json_escaping() {
    let (_root, tools, dir) = fixture();
    let text = format!("日本語\n{}", "\n\"\\\u{0001}".repeat(30_000));
    std::fs::write(dir.join("escaped.md"), &text).unwrap();
    let before = std::fs::read(dir.join("escaped.md")).unwrap();
    let content = tools.read_memory("escaped").await.unwrap();
    assert!(content.starts_with("日本語\n"));
    assert!(content.contains("output truncated"));
    let serialized = json!({"content": content}).to_string();
    assert!(serialized.chars().count() <= MAX_MEMORY_OUTPUT_CHARS);
    assert_eq!(
        crate::llm::truncate_tool_output(serialized.clone(), "read_memory"),
        serialized
    );
    assert_eq!(std::fs::read(dir.join("escaped.md")).unwrap(), before);
}

#[tokio::test]
async fn oversize_write_preserves_existing_bytes_and_does_not_create_directories() {
    let (root, tools, dir) = fixture();
    tools.write_memory("kept", "old", None, None).await.unwrap();
    let before = std::fs::read(dir.join("kept.md")).unwrap();
    assert!(
        tools
            .write_memory("kept", &"x".repeat(MAX_MEMORY_FILE_BYTES), None, None)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(dir.join("kept.md")).unwrap(), before);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        tools
            .write_memory(
                "new",
                "body",
                None,
                Some(json!({"huge": "x".repeat(MAX_MEMORY_FILE_BYTES)}))
            )
            .await
            .is_err()
    );
    assert!(!root.path().join(".doge/memory").exists());
}

#[test]
fn exact_file_limit_and_total_scan_limit_are_fail_closed() {
    let (_root, _tools, dir) = fixture();
    let path = dir.join("exact.md");
    std::fs::write(&path, "x".repeat(MAX_MEMORY_FILE_BYTES)).unwrap();
    assert_eq!(
        read_memory_file(&path, MAX_MEMORY_FILE_BYTES)
            .unwrap()
            .len(),
        MAX_MEMORY_FILE_BYTES
    );
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len((MAX_MEMORY_FILE_BYTES + 1) as u64).unwrap();
    assert!(read_memory_file(&path, MAX_MEMORY_FILE_BYTES).is_err());
    std::fs::remove_file(path).unwrap();
    for i in 0..8 {
        std::fs::write(
            dir.join(format!("{i}.md")),
            "x".repeat(MAX_MEMORY_FILE_BYTES),
        )
        .unwrap();
    }
    assert_eq!(
        search_memories(&dir, Some("no-match"), None).unwrap(),
        "No matching memories found."
    );
    std::fs::write(dir.join("9.md"), "one byte beyond total").unwrap();
    assert!(search_memories(&dir, Some("no-match"), None).is_err());
}

#[tokio::test]
async fn sorted_complete_keys_or_explicit_output_error_never_silent_partial_matches() {
    let (_root, tools, dir) = fixture();
    for key in ["z", "a", "m"] {
        std::fs::write(dir.join(format!("{key}.md")), "日本語 match").unwrap();
    }
    assert_eq!(
        tools.list_memories().await.unwrap(),
        "Available memories:\n- a\n- m\n- z"
    );
    assert_eq!(
        tools
            .search_memory(Some("日本語".into()), None)
            .await
            .unwrap(),
        "Matching memories:\n- a\n- m\n- z"
    );
    for i in 0..40 {
        std::fs::write(
            dir.join(format!("long-{i:02}-{}.md", "q".repeat(200))),
            "match",
        )
        .unwrap();
    }
    assert!(tools.list_memories().await.is_err());
    assert!(
        tools
            .search_memory(Some("match".into()), None)
            .await
            .is_err()
    );
    assert_eq!(
        tools
            .search_memory(Some("long-00".into()), None)
            .await
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[test]
fn inventory_limit_counts_irrelevant_entries_too() {
    let (_root, _tools, dir) = fixture();
    for i in 0..MAX_MEMORY_ENTRIES {
        std::fs::write(dir.join(format!("{i}.txt")), "").unwrap();
    }
    assert!(memory_inventory(&dir).unwrap().is_empty());
    std::fs::write(dir.join("extra.txt"), "").unwrap();
    assert!(memory_inventory(&dir).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn regular_aliases_remain_readable_but_fifo_and_directory_inputs_do_not_block() {
    let (_root, tools, dir) = fixture();
    std::fs::write(dir.join("regular.md"), "literal 日本語").unwrap();
    std::os::unix::fs::symlink(dir.join("regular.md"), dir.join("alias.md")).unwrap();
    assert_eq!(tools.read_memory("alias").await.unwrap(), "literal 日本語");
    std::fs::create_dir(dir.join("directory.md")).unwrap();
    assert!(tools.read_memory("directory").await.is_err());
    assert!(tools.search_memory(None, None).await.is_err());
    std::fs::remove_dir(dir.join("directory.md")).unwrap();
    let fifo = dir.join("fifo.md");
    let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), tools.read_memory("fifo"))
        .await
        .expect("must not block");
    assert!(result.is_err());
    assert!(tools.search_memory(None, None).await.is_err());
    assert!(tools.list_memories().await.is_err());
}
