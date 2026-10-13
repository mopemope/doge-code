//! Bounded regular-file acquisition; callers own text decoding and failure policy.
use std::{fs::OpenOptions, io::Read, path::Path};

#[derive(Clone, Copy)]
pub enum SymlinkPolicy {
    Follow,
    Reject,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("input is not a regular file")]
    NonRegular,
    #[error("input byte limit exceeded ({0} bytes)")]
    LimitExceeded(usize),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub fn read_regular_bytes(
    path: &Path,
    limit: usize,
    symlinks: SymlinkPolicy,
) -> Result<Vec<u8>, ReadError> {
    let metadata = match symlinks {
        SymlinkPolicy::Follow => path.metadata()?,
        SymlinkPolicy::Reject => path.symlink_metadata()?,
    };
    if !metadata.is_file() {
        return Err(ReadError::NonRegular);
    }
    if metadata.len() > limit as u64 {
        return Err(ReadError::LimitExceeded(limit));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let flags = libc::O_NONBLOCK
            | if matches!(symlinks, SymlinkPolicy::Reject) {
                libc::O_NOFOLLOW
            } else {
                0
            };
        options.custom_flags(flags);
    }
    let file = options.open(path)?;
    read_opened_regular_bytes(file, limit)
}

fn read_opened_regular_bytes(file: std::fs::File, limit: usize) -> Result<Vec<u8>, ReadError> {
    if !file.metadata()?.is_file() {
        return Err(ReadError::NonRegular);
    }
    let mut bytes = Vec::new();
    file.take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(ReadError::LimitExceeded(limit));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_limit_and_excess_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input");
        std::fs::write(&path, b"abcd").unwrap();
        assert_eq!(
            read_regular_bytes(&path, 4, SymlinkPolicy::Reject).unwrap(),
            b"abcd"
        );
        assert!(matches!(
            read_regular_bytes(&path, 3, SymlinkPolicy::Reject),
            Err(ReadError::LimitExceeded(3))
        ));
        assert!(matches!(
            read_regular_bytes(dir.path(), 100, SymlinkPolicy::Follow),
            Err(ReadError::NonRegular)
        ));
    }
    #[test]
    fn bytes_consumed_remain_bounded_when_file_grows_after_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("growing");
        std::fs::write(&path, b"a").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        std::fs::write(&path, b"a longer stream").unwrap();
        assert!(matches!(
            read_opened_regular_bytes(file, 4),
            Err(ReadError::LimitExceeded(4))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_symlink_policy_and_fifo_do_not_block() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        let link = dir.path().join("link");
        std::fs::write(&file, b"literal").unwrap();
        symlink(&file, &link).unwrap();
        assert_eq!(
            read_regular_bytes(&link, 20, SymlinkPolicy::Follow).unwrap(),
            b"literal"
        );
        assert!(matches!(
            read_regular_bytes(&link, 20, SymlinkPolicy::Reject),
            Err(ReadError::NonRegular)
        ));
        let fifo = dir.path().join("fifo");
        let path = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            read_regular_bytes(&fifo, 20, SymlinkPolicy::Follow),
            Err(ReadError::NonRegular)
        ));
    }
}
