#[cfg(test)]
mod tests {
    use super::super::tools::FsTools;
    use std::fs;

    #[test]
    fn read_rejects_escape_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("dir")).unwrap();
        fs::write(root.join("dir/file.txt"), "x").unwrap();

        let tools = FsTools::new(root);
        // directory read should error
        assert!(tools.fs_read("dir", None, None).is_err());
        // path escape using .. should error
        assert!(tools.fs_read("../etc/passwd", None, None).is_err());
    }

    #[test]
    fn write_rejects_binary_and_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let tools = FsTools::new(root);
        // binary content (contains NUL)
        let bin = String::from_utf8(vec![b'a', 0, b'b']).unwrap();
        assert!(tools.fs_write("bin.txt", &bin).is_err());
        // attempt to escape root
        assert!(tools.fs_write("../x.txt", "y").is_err());
    }

    #[test]
    fn search_invalid_regex_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("x.txt"), "hello").unwrap();
        let tools = FsTools::new(root);
        assert!(tools.fs_search("[unterminated", Some("**/*.txt")).is_err());
    }
}
