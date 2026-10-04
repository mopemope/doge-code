#![cfg(unix)]
use super::{
    edit::{EditParams, edit_with_receipt},
    write::fs_write_with_receipt,
};
use crate::config::AppConfig;
use std::os::unix::fs::symlink;

#[test]
fn mutation_target_write_preserves_link_and_changes_resolved_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.txt");
    let alias = dir.path().join("alias.txt");
    std::fs::write(&target, "old\n").unwrap();
    symlink("target.txt", &alias).unwrap();
    let config = AppConfig {
        project_root: dir.path().to_path_buf(),
        ..Default::default()
    };
    let result = fs_write_with_receipt(alias.to_str().unwrap(), "new\n", &config).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert!(alias.is_symlink());
    assert_eq!(
        std::fs::read_link(&alias).unwrap(),
        std::path::PathBuf::from("target.txt")
    );
    assert_eq!(result.receipt.unwrap().path, target.canonicalize().unwrap());
}

#[tokio::test]
async fn mutation_target_edit_preserves_chain() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target.txt");
    let first = dir.path().join("first.txt");
    let alias = dir.path().join("alias.txt");
    std::fs::write(&target, "old\n").unwrap();
    symlink(&target, &first).unwrap();
    symlink("first.txt", &alias).unwrap();
    let config = AppConfig {
        project_root: dir.path().to_path_buf(),
        ..Default::default()
    };
    let result = edit_with_receipt(
        EditParams {
            file_path: alias.to_str().unwrap().into(),
            target_block: "old".into(),
            new_block: "new".into(),
            start_line: None,
            end_line: None,
            allow_multiple: None,
        },
        &config,
    )
    .await
    .unwrap();
    assert!(result.result.success);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\n");
    assert!(alias.is_symlink() && first.is_symlink());
    assert_eq!(result.receipt.unwrap().path, target.canonicalize().unwrap());
}

#[tokio::test]
async fn mutation_target_patch_semantic_parent_alias_and_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("real");
    std::fs::create_dir(&root).unwrap();
    let alias = dir.path().join("root-alias");
    symlink(&root, &alias).unwrap();
    let config = AppConfig {
        project_root: alias.clone(),
        ..Default::default()
    };
    let target = root.join("target.rs");
    std::fs::write(&target, "fn foo() { 1; }\n").unwrap();
    let link = alias.join("link.rs");
    symlink("target.rs", &link).unwrap();
    let patch = diffy::create_patch("fn foo() { 1; }\n", "fn foo() { 2; }\n").to_string();
    let exec = super::apply_patch::apply_patch_with_recovery_and_receipt(
        super::apply_patch::ApplyPatchParams {
            file_path: link.to_str().unwrap().into(),
            patch_content: patch,
        },
        &config,
    )
    .await
    .unwrap();
    assert!(exec.result.success);
    assert_eq!(exec.receipt.unwrap().path, target);
    assert!(link.is_symlink());
    let prepared = crate::features::semantic_edit::prepare_edit(&alias, &link, 1).unwrap();
    assert_eq!(prepared.file, target);
    crate::features::semantic_edit::apply_edit(&prepared, "fn foo() { 3; }", &alias)
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "fn foo() { 3; }\n"
    );
    assert!(link.is_symlink());
    let new = alias.join("new/deep/file.txt");
    let exec = fs_write_with_receipt(new.to_str().unwrap(), "new", &config).unwrap();
    assert_eq!(exec.receipt.unwrap().path, root.join("new/deep/file.txt"));
    assert!(alias.is_symlink());
}

#[tokio::test]
async fn mutation_target_scope_dangling_cycle_and_special_files() {
    use super::mutation::{MutationTarget, read_text_snapshot, read_text_snapshot_async};
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let target = root.join("target.txt");
    std::fs::write(&target, "old").unwrap();
    let config = AppConfig {
        project_root: root.clone(),
        ..Default::default()
    };
    let external_alias = outside.path().join("external.txt");
    symlink(&target, &external_alias).unwrap();
    let exec = fs_write_with_receipt(external_alias.to_str().unwrap(), "new", &config).unwrap();
    assert_eq!(exec.receipt.unwrap().path, target);
    assert!(external_alias.is_symlink());
    let external = outside.path().join("outside.txt");
    std::fs::write(&external, "external").unwrap();
    let escape = root.join("escape.txt");
    symlink(&external, &escape).unwrap();
    assert!(MutationTarget::resolve(&escape, &config, &[]).is_err());
    // fs_write retains its existing tool-specific temp allowance; edit does not.
    assert!(MutationTarget::resolve(&escape, &config, &[std::env::temp_dir()]).is_ok());
    for (name, destination) in [("dangling", "missing"), ("cycle", "cycle")] {
        let path = root.join(name);
        symlink(destination, &path).unwrap();
        assert!(MutationTarget::resolve(&path, &config, &[]).is_err());
        assert!(fs_write_with_receipt(path.to_str().unwrap(), "bad", &config).is_err());
        assert!(path.is_symlink());
    }
    let fifo = root.join("fifo");
    let bytes = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(bytes.as_ptr(), 0o600) }, 0);
    let socket = root.join("socket");
    let _socket = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    for path in [&fifo, &socket, &root] {
        assert!(read_text_snapshot(path).is_err());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            read_text_snapshot_async(path),
        )
        .await
        .unwrap();
        assert!(result.is_err());
    }
    assert!(
        read_text_snapshot(&external_alias).is_err(),
        "unresolved symlink cannot enter shared writer"
    );
}

#[tokio::test]
async fn mutation_target_prepublication_races_reject_async_and_blocking() {
    use super::mutation::{MutationCommitError, MutationTarget, read_text_snapshot};
    for asynchronous in [false, true] {
        for variant in 0..6 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let target = root.join("target.txt");
            let other = root.join("other.txt");
            let alias = root.join("alias.txt");
            std::fs::write(&other, "same").unwrap();
            if variant != 5 {
                std::fs::write(&target, "same").unwrap();
            }
            symlink(&target, &alias).unwrap();
            let requested = if variant == 5 {
                target.clone()
            } else {
                alias.clone()
            };
            let config = AppConfig {
                project_root: root.clone(),
                ..Default::default()
            };
            let mut resolved = MutationTarget::resolve(&requested, &config, &[]).unwrap();
            let before = read_text_snapshot(resolved.path()).unwrap();
            let observed = target.clone();
            let alternate = other.clone();
            let changed_alias = alias.clone();
            resolved.before_publish_hook = Some(std::sync::Arc::new(move || match variant {
                0 => {
                    std::fs::remove_file(&changed_alias).unwrap();
                    symlink(&alternate, &changed_alias).unwrap();
                }
                1 => {
                    std::fs::remove_file(&observed).unwrap();
                    symlink(&alternate, &observed).unwrap();
                }
                2 => {
                    std::fs::remove_file(&observed).unwrap();
                    std::fs::write(&observed, "same").unwrap();
                }
                3 => {
                    std::fs::remove_file(&observed).unwrap();
                }
                4 => {
                    std::fs::remove_file(&changed_alias).unwrap();
                    symlink("/outside-not-authorized", &changed_alias).unwrap();
                }
                _ => {
                    symlink(&alternate, &observed).unwrap();
                }
            }));
            let error = if asynchronous {
                resolved.commit(&before, "ours").await.unwrap_err()
            } else {
                resolved.commit_blocking(&before, "ours").unwrap_err()
            };
            assert!(
                matches!(error, MutationCommitError::ConcurrentModification),
                "{variant}: {error}"
            );
            assert_eq!(std::fs::read_to_string(&other).unwrap(), "same");
            assert!(
                !std::fs::read_dir(&root).unwrap().any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tmp_mutation_")),
                "candidate cleaned up"
            );
            if target.is_file() {
                assert_eq!(std::fs::read_to_string(&target).unwrap(), "same");
            }
        }
    }
}

#[tokio::test]
async fn mutation_target_tracking_aliases_undo_and_review_reject() {
    use super::FsTools;
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let target = root.join("target.txt");
    let alias = root.join("alias.txt");
    let other_alias = root.join("other-alias.txt");
    std::fs::write(&target, "old").unwrap();
    symlink("target.txt", &alias).unwrap();
    symlink(&target, &other_alias).unwrap();
    let config = Arc::new(AppConfig {
        project_root: root.clone(),
        ..Default::default()
    });
    let plain = FsTools::new(Arc::new(tokio::sync::RwLock::new(None)), config);
    plain
        .fs_write(alias.to_str().unwrap(), "first")
        .await
        .unwrap();
    plain
        .fs_write(other_alias.to_str().unwrap(), "second")
        .await
        .unwrap();
    assert_eq!(plain.undo_stack.read().await.len(), 2);
    assert_eq!(
        plain.undo_stack.read().await.peek_last().unwrap().path,
        target
    );
    assert!(super::undo::undo(&plain).await.unwrap().success);
    assert!(super::undo::undo(&plain).await.unwrap().success);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
    assert!(alias.is_symlink() && other_alias.is_symlink());
    let fs = plain.clone().with_review_capture(crate::jobs::JobId(11));
    fs.fs_write(alias.to_str().unwrap(), "first").await.unwrap();
    fs.fs_write(other_alias.to_str().unwrap(), "second")
        .await
        .unwrap();
    let id = fs.seal_review().unwrap().review_id.unwrap();
    let report = fs
        .reject_review(&id, &tokio_util::sync::CancellationToken::new())
        .await;
    assert!(report.error.is_none(), "{:?}", report.error);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
    assert!(alias.is_symlink() && other_alias.is_symlink());
    plain
        .fs_write(alias.to_str().unwrap(), "third")
        .await
        .unwrap();
    let unrelated = root.join("unrelated.txt");
    std::fs::write(&unrelated, "third").unwrap();
    std::fs::remove_file(&target).unwrap();
    symlink(&unrelated, &target).unwrap();
    let undone = super::undo::undo(&plain).await.unwrap();
    assert!(!undone.success && undone.conflict);
    assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "third");
    assert!(target.is_symlink());
    assert_eq!(plain.undo_stack.read().await.len(), 1);
}

#[tokio::test]
async fn mutation_target_undo_parent_retarget_never_writes_or_deletes_outside() {
    use super::FsTools;
    use std::sync::Arc;
    for created in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let sub = root.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let target = sub.join("file.txt");
        if !created {
            std::fs::write(&target, "before").unwrap();
        }
        let fs = FsTools::new(
            Arc::new(tokio::sync::RwLock::new(None)),
            Arc::new(AppConfig {
                project_root: root.clone(),
                ..Default::default()
            }),
        );
        fs.fs_write(target.to_str().unwrap(), "after")
            .await
            .unwrap();
        let external = outside.path().join("file.txt");
        std::fs::write(&external, "after").unwrap();
        std::fs::rename(&sub, root.join("saved-sub")).unwrap();
        symlink(outside.path(), &sub).unwrap();
        let undo = super::undo::undo(&fs).await.unwrap();
        assert!(!undo.success && undo.conflict);
        assert_eq!(fs.undo_stack.read().await.len(), 1);
        assert_eq!(std::fs::read_to_string(&external).unwrap(), "after");
        assert!(sub.is_symlink());
        std::fs::remove_file(&sub).unwrap();
        std::fs::rename(root.join("saved-sub"), &sub).unwrap();
        assert!(super::undo::undo(&fs).await.unwrap().success);
        assert!(fs.undo_stack.read().await.is_empty());
    }
}

#[tokio::test]
async fn mutation_target_semantic_alias_retarget_and_shared_writer_guard() {
    use super::mutation::{
        MutationTarget, commit_text_candidate, commit_text_candidate_blocking, read_text_snapshot,
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let target = root.join("target.rs");
    let other = root.join("other.rs");
    let alias = root.join("alias.rs");
    for file in [&target, &other] {
        std::fs::write(file, "fn foo() { 1; }\n").unwrap();
    }
    symlink(&target, &alias).unwrap();
    let prepared = crate::features::semantic_edit::prepare_edit(&root, &alias, 1).unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&other, &alias).unwrap();
    assert!(matches!(
        crate::features::semantic_edit::apply_edit(&prepared, "fn foo() { 2; }\n", &root).await,
        Err(crate::features::semantic_edit::SemanticEditError::ConcurrentModification)
    ));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "fn foo() { 1; }\n"
    );
    assert_eq!(
        std::fs::read_to_string(&other).unwrap(),
        "fn foo() { 1; }\n"
    );
    let before = read_text_snapshot(&target).unwrap();
    std::fs::remove_file(&target).unwrap();
    symlink(&other, &target).unwrap();
    assert!(
        commit_text_candidate(&target, &before, "bad")
            .await
            .is_err()
    );
    assert!(commit_text_candidate_blocking(&target, &before, "bad").is_err());
    assert!(target.is_symlink());
    let root_alias = dir.path().join("root-alias");
    symlink(&root, &root_alias).unwrap();
    let config = AppConfig {
        project_root: root_alias.clone(),
        ..Default::default()
    };
    let resolved = MutationTarget::resolve(&other, &config, &[]).unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::remove_file(&root_alias).unwrap();
    symlink(outside.path(), &root_alias).unwrap();
    assert!(resolved.revalidate().is_err());
}
