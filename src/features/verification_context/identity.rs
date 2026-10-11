//! Read-only identity of the invocation's bounded observable environment.
//! Hashes are correspondence handles, not proof of reproducibility or secrecy.
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, io::Read, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    pub version: u32,
    pub digest: String,
}
impl ExecutionIdentity {
    pub fn known(&self) -> bool {
        self.version == 1
            && self.digest.len() == 64
            && self.digest.bytes().all(|b| b.is_ascii_hexdigit())
    }
}
/// No process is started. Unresolved executables and inputs beyond bounds stay unknown.
pub fn capture(root: &Path, cwd: &Path, program: &str) -> Option<ExecutionIdentity> {
    capture_with_env(root, cwd, program, std::env::vars_os().collect())
}
fn capture_with_env(
    root: &Path,
    cwd: &Path,
    program: &str,
    mut env: Vec<(OsString, OsString)>,
) -> Option<ExecutionIdentity> {
    let root = root.canonicalize().ok()?;
    let cwd = cwd.canonicalize().ok()?;
    if !cwd.starts_with(&root) || env.len() > 1024 {
        return None;
    }
    let executable = super::resolve_executable(program, &cwd)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(&executable).ok()?;
    let before = file.metadata().ok()?;
    if !before.is_file() || before.len() > 16 * 1024 * 1024 {
        return None;
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let after = file.metadata().ok()?;
    if bytes.len() as u64 != before.len()
        || before.len() != after.len()
        || before.modified().ok()? != after.modified().ok()?
    {
        return None;
    }
    let mut hash = blake3::Hasher::new();
    fn field(hash: &mut blake3::Hasher, bytes: &[u8]) {
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    for value in [
        b"dgc-execution-identity-v1".as_slice(),
        std::env::consts::OS.as_bytes(),
        std::env::consts::ARCH.as_bytes(),
        root.as_os_str().as_encoded_bytes(),
        cwd.as_os_str().as_encoded_bytes(),
        executable.as_os_str().as_encoded_bytes(),
        &bytes,
    ] {
        field(&mut hash, value);
    }
    env.sort();
    let mut size = 0usize;
    for (key, value) in env {
        size = size
            .checked_add(key.as_encoded_bytes().len())?
            .checked_add(value.as_encoded_bytes().len())?;
        if size > 128 * 1024 {
            return None;
        }
        field(&mut hash, key.as_encoded_bytes());
        field(&mut hash, value.as_encoded_bytes());
    }
    Some(ExecutionIdentity {
        version: 1,
        digest: hash.finalize().to_hex().to_string(),
    })
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn verification_identity_tracks_executable_cwd_and_env_without_values() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("root");
        let program = root.path().join("test-runner");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("program");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("mode");
        let env = vec![("SECRET".into(), "private-value".into())];
        let run = |cwd: &Path, env| {
            capture_with_env(root.path(), cwd, program.to_str().expect("path"), env)
        };
        let first = run(root.path(), env.clone()).expect("identity");
        assert_eq!(Some(first.clone()), run(root.path(), env.clone()));
        assert_ne!(
            Some(first.clone()),
            run(root.path(), vec![("SECRET".into(), "changed".into())])
        );
        let child = root.path().join("child");
        std::fs::create_dir(&child).expect("cwd");
        assert_ne!(Some(first.clone()), run(&child, env.clone()));
        std::fs::write(&program, "#!/bin/sh\nexit 1\n").expect("edit");
        assert_ne!(Some(first.clone()), run(root.path(), env));
        let json = serde_json::to_string(&first).expect("json");
        assert!(
            !json.contains("SECRET")
                && !json.contains("private-value")
                && !json.contains(program.to_str().expect("path"))
        );
        assert!(
            run(
                root.path(),
                vec![("X".into(), "x".repeat(128 * 1024).into())]
            )
            .is_none()
        );
    }
}
