use super::{SessionData, SessionStore, error::SessionError, lease::SessionLease};
use serde::Serialize;
use std::{
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
};

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum RecoveryFault {
    Write,
    FileSync,
    Publish,
    RootSync,
}

pub(crate) const MAX_RECOVERY_BYTES: u64 = 64 * 1024 * 1024;

/// Deliberately separate from the normal session format and resume inventory.
#[derive(Serialize)]
struct RecoveryEnvelope<'a> {
    version: u32,
    source_session_id: &'a str,
    exported_at: String,
    session: &'a SessionData,
}

#[derive(Debug)]
pub(crate) struct RecoveryExport {
    pub path: PathBuf,
    pub bytes: u64,
    /// Publishing succeeded: don't treat a directory-sync warning as no file.
    pub durability_warning: Option<String>,
}

struct BoundedWriter<W> {
    inner: W,
    limit: u64,
    written: u64,
    exceeded: Option<u64>,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let attempted = self.written.saturating_add(bytes.len() as u64);
        if attempted > self.limit {
            self.exceeded = Some(attempted);
            return Err(std::io::Error::other("session JSON capacity exceeded"));
        }
        let count = self.inner.write(bytes)?;
        self.written += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Stream into a bounded buffer/file, never build a second full JSON String.
pub(super) fn write_bounded_json<W: Write, T: Serialize>(
    target: W,
    value: &T,
    limit: u64,
) -> Result<u64, SessionError> {
    let mut writer = BoundedWriter {
        inner: BufWriter::new(target),
        limit,
        written: 0,
        exceeded: None,
    };
    if let Err(error) = serde_json::to_writer_pretty(&mut writer, value) {
        return Err(match writer.exceeded {
            Some(detected_at_least) => SessionError::CapacityExceeded {
                limit,
                detected_at_least,
            },
            None => SessionError::WriteError(std::io::Error::other(error)),
        });
    }
    writer.flush().map_err(SessionError::WriteError)?;
    Ok(writer.written)
}

impl SessionStore {
    pub(crate) fn export_recovery(
        &self,
        data: &SessionData,
        lease: &SessionLease,
    ) -> Result<RecoveryExport, SessionError> {
        self.export_recovery_named(
            data,
            lease,
            MAX_RECOVERY_BYTES,
            &uuid::Uuid::now_v7().to_string(),
            #[cfg(test)]
            None,
        )
    }

    fn export_recovery_named(
        &self,
        data: &SessionData,
        lease: &SessionLease,
        limit: u64,
        name: &str,
        #[cfg(test)] fault: Option<RecoveryFault>,
    ) -> Result<RecoveryExport, SessionError> {
        self.ensure_writable()?;
        lease.validate(&self.root, &data.meta.id)?;
        let root = self.root.canonicalize().map_err(SessionError::WriteError)?;
        let dir = root.join(".recovery");
        match fs::symlink_metadata(&dir) {
            Ok(meta)
                if !meta.is_dir()
                    || meta.file_type().is_symlink()
                    || meta.permissions().readonly() =>
            {
                return Err(SessionError::WriteError(std::io::Error::other(
                    "unsafe or read-only recovery directory",
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    builder.mode(0o700);
                }
                if let Err(error) = builder.create(&dir)
                    && error.kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(SessionError::CreateDirError(error));
                }
            }
            Err(error) => return Err(SessionError::WriteError(error)),
        }
        let meta = fs::symlink_metadata(&dir).map_err(SessionError::WriteError)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().readonly() {
            return Err(SessionError::WriteError(std::io::Error::other(
                "unsafe recovery directory",
            )));
        }
        let envelope = RecoveryEnvelope {
            version: 1,
            source_session_id: &data.meta.id,
            exported_at: chrono::Utc::now().to_rfc3339(),
            session: data,
        };
        let mut candidate =
            tempfile::NamedTempFile::new_in(&dir).map_err(SessionError::WriteError)?;
        #[cfg(test)]
        if fault == Some(RecoveryFault::Write) {
            return Err(SessionError::WriteError(std::io::Error::other(
                "injected recovery write failure",
            )));
        }
        let bytes = write_bounded_json(&mut candidate, &envelope, limit)?;
        #[cfg(test)]
        if fault == Some(RecoveryFault::FileSync) {
            return Err(SessionError::WriteError(std::io::Error::other(
                "injected recovery file sync failure",
            )));
        }
        candidate
            .as_file()
            .sync_all()
            .map_err(SessionError::WriteError)?;
        let path = dir.join(format!("{name}.json"));
        #[cfg(test)]
        if fault == Some(RecoveryFault::Publish) {
            return Err(SessionError::WriteError(std::io::Error::other(
                "injected recovery publication failure",
            )));
        }
        candidate
            .persist_noclobber(&path)
            .map_err(|error| SessionError::WriteError(error.error))?;
        #[cfg(unix)]
        let sync = fs::File::open(&dir)
            .and_then(|file| file.sync_all())
            .and_then(|_| {
                // Also persist the parent entry for a newly-created .recovery.
                // Sync every export so a preceding failed export cannot leave it unconfirmed.
                #[cfg(test)]
                if fault == Some(RecoveryFault::RootSync) {
                    return Err(std::io::Error::other("injected recovery root sync failure"));
                }
                fs::File::open(&root).and_then(|file| file.sync_all())
            });
        #[cfg(not(unix))]
        let sync: std::io::Result<()> = Ok(());
        #[cfg(test)]
        let sync = if self.fail_directory_sync {
            Err(std::io::Error::other("injected directory sync failure"))
        } else {
            sync
        };
        Ok(RecoveryExport {
            path,
            bytes,
            durability_warning: sync.err().map(|error| {
                format!("recovery file published, but directory durability sync failed: {error}")
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capacity_streaming_exact_limit_and_unicode_escape_bytes() {
        let value = json!({"text": "点\n\"\\\u{0}終"});
        let encoded = serde_json::to_vec_pretty(&value).unwrap();
        let limit = encoded.len() as u64;
        let mut output = Vec::new();
        assert_eq!(
            write_bounded_json(&mut output, &value, limit).unwrap(),
            limit
        );
        assert_eq!(output, encoded);
        assert!(
            matches!(write_bounded_json(Vec::new(), &value, limit - 1), Err(SessionError::CapacityExceeded { limit: found, detected_at_least }) if found == limit - 1 && detected_at_least > found)
        );
        let data = SessionData::new();
        let envelope = RecoveryEnvelope {
            version: 1,
            source_session_id: &data.meta.id,
            exported_at: "fixed timestamp".into(),
            session: &data,
        };
        let exact = serde_json::to_vec_pretty(&envelope).unwrap().len() as u64;
        assert_eq!(
            write_bounded_json(Vec::new(), &envelope, exact).unwrap(),
            exact
        );
        assert!(matches!(
            write_bounded_json(Vec::new(), &envelope, exact - 1),
            Err(SessionError::CapacityExceeded { .. })
        ));
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full fixture"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(matches!(
            write_bounded_json(Failing, &value, limit),
            Err(SessionError::WriteError(_))
        ));
    }

    #[test]
    fn capacity_normal_save_boundary_retains_original_and_cleans_temp() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path()).unwrap();
        let data = store.create().unwrap();
        let lease = store.try_lease(&data.meta.id).unwrap();
        let checkpoint = store.session_dir(&data.meta.id).join("session.json");
        let original = fs::read(&checkpoint).unwrap();
        let mut changed = data.clone();
        changed.meta.title = "点\n\"changed".into();
        let bytes = serde_json::to_vec_pretty(&changed).unwrap().len() as u64;
        assert!(matches!(
            store.save_with_lease_limit(&changed, &lease, bytes - 1),
            Err(SessionError::CapacityExceeded { .. })
        ));
        assert_eq!(fs::read(&checkpoint).unwrap(), original);
        assert_eq!(
            fs::read_dir(checkpoint.parent().unwrap()).unwrap().count(),
            1
        );
        store
            .save_with_lease_limit(&changed, &lease, bytes)
            .unwrap();
        assert_eq!(
            serde_json::to_value(store.load(&data.meta.id).unwrap()).unwrap(),
            serde_json::to_value(&changed).unwrap()
        );
    }

    #[test]
    fn capacity_recovery_complete_no_clobber_and_failure_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = SessionStore::new(dir.path()).unwrap();
        let mut data = store.create().unwrap();
        let lease = store.try_lease(&data.meta.id).unwrap();
        let checkpoint = store.session_dir(&data.meta.id).join("session.json");
        let original = fs::read(&checkpoint).unwrap();
        data.meta.title = "memory version\n点".into();
        data.inference_binding = Some("provider:test-account:model".into());
        data.token_count = 123;
        data.requests = 2;
        data.tool_calls = 1;
        data.conversation = vec![
            serde_json::from_value(json!({"role":"assistant","content":null,"tool_calls":[{"id":"pending","type":"function","function":{"name":"fs_read","arguments":"{}"}}],"provider_state":{"provider":"openai-chatgpt","account_id":"test-account","model":"test-model","output":[{"type":"reasoning","encrypted_content":"opaque-state"}]}})).unwrap(),
            serde_json::from_value(json!({"role":"tool","tool_call_id":"pending","content":"complete result"})).unwrap(),
        ];

        data.unseen_tool_results.insert("pending".into());
        data.observations
            .insert("pending".into(), "test".into(), "recoverable".into(), 32)
            .unwrap();
        let name = uuid::Uuid::now_v7().to_string();
        let export = store
            .export_recovery_named(&data, &lease, MAX_RECOVERY_BYTES, &name, None)
            .unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(&export.path).unwrap()).unwrap();
        assert_eq!(saved["version"], 1);
        assert_eq!(saved["source_session_id"], data.meta.id);
        assert_eq!(saved["session"], serde_json::to_value(&data).unwrap());
        assert_eq!(export.bytes, export.path.metadata().unwrap().len());
        assert!(export.path.is_absolute());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                export.path.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let before = fs::read(&export.path).unwrap();
        assert!(
            store
                .export_recovery_named(&data, &lease, MAX_RECOVERY_BYTES, &name, None)
                .is_err()
        );
        assert_eq!(fs::read(&export.path).unwrap(), before);
        let second = store.export_recovery(&data, &lease).unwrap();
        assert_ne!(export.path, second.path);
        for fault in [
            Some(RecoveryFault::Write),
            Some(RecoveryFault::FileSync),
            Some(RecoveryFault::Publish),
        ] {
            assert!(
                store
                    .export_recovery_named(
                        &data,
                        &lease,
                        MAX_RECOVERY_BYTES,
                        &uuid::Uuid::now_v7().to_string(),
                        fault
                    )
                    .is_err()
            );
        }
        assert!(matches!(
            store.export_recovery_named(&data, &lease, 1, &uuid::Uuid::now_v7().to_string(), None),
            Err(SessionError::CapacityExceeded { limit: 1, .. })
        ));
        assert_eq!(
            fs::read_dir(export.path.parent().unwrap()).unwrap().count(),
            2
        );
        assert_eq!(fs::read(&checkpoint).unwrap(), original);
        let root_warning = store
            .export_recovery_named(
                &data,
                &lease,
                MAX_RECOVERY_BYTES,
                &uuid::Uuid::now_v7().to_string(),
                Some(RecoveryFault::RootSync),
            )
            .unwrap();
        assert!(root_warning.path.exists() && root_warning.durability_warning.is_some());
        store.fail_directory_sync = true;
        let warning = store.export_recovery(&data, &lease).unwrap();
        assert!(warning.path.exists() && warning.durability_warning.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn capacity_recovery_readonly_symlink_permissions_and_inventory() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let store = SessionStore::new(&root).unwrap();
        let data = store.create().unwrap();
        let lease = store.try_lease(&data.meta.id).unwrap();
        let readonly = SessionStore::open_existing(&root).unwrap();
        assert!(readonly.export_recovery(&data, &lease).is_err());
        assert!(!root.join(".recovery").exists());
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join(".recovery")).unwrap();
        assert!(store.export_recovery(&data, &lease).is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        fs::remove_file(root.join(".recovery")).unwrap();
        fs::create_dir(root.join(".recovery")).unwrap();
        fs::set_permissions(root.join(".recovery"), fs::Permissions::from_mode(0o500)).unwrap();
        assert!(store.export_recovery(&data, &lease).is_err());
        fs::set_permissions(root.join(".recovery"), fs::Permissions::from_mode(0o700)).unwrap();
        let export = store.export_recovery(&data, &lease).unwrap();
        let outside_file = outside.join("keep.json");
        fs::write(&outside_file, "keep original").unwrap();
        let occupied_name = uuid::Uuid::now_v7().to_string();
        let occupied_path = root.join(".recovery").join(format!("{occupied_name}.json"));
        symlink(&outside_file, &occupied_path).unwrap();
        assert!(
            store
                .export_recovery_named(&data, &lease, MAX_RECOVERY_BYTES, &occupied_name, None)
                .is_err()
        );
        assert!(occupied_path.is_symlink());
        assert_eq!(fs::read_to_string(&outside_file).unwrap(), "keep original");
        fs::remove_file(occupied_path).unwrap();
        fs::write(root.join(".recovery/session.json"), "not a normal session").unwrap();
        for _ in 0..101 {
            store.create().unwrap();
        }
        assert!(export.path.exists());
        assert_eq!(store.list().unwrap().len(), 100);
        assert!(store.resolve_id_prefix(".recovery").is_err());
        assert!(store.load(".recovery").is_err());
        assert_eq!(
            SessionStore::open_existing(&root)
                .unwrap()
                .list()
                .unwrap()
                .len(),
            100
        );
    }
}
