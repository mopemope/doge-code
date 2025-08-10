#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::path::PathBuf;

    // Helper function to create a mock reader that returns a specific content for a given path
    fn create_mock_reader(expected_path: PathBuf, content: String) -> impl Fn(&Path) -> io::Result<String> {
        move |path: &Path| {
            if path == expected_path {
                Ok(content.clone())
            } else {
                Err(io::Error::new(io::ErrorKind::NotFound, "File not found"))
            }
        }
    }

    #[test]
    fn test_find_project_instructions_file_found_project_md() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let project_md_path = project_root.join("PROJECT.md");
        let qwen_md_path = project_root.join("QWEN.md");
        let gemini_md_path = project_root.join("GEMINI.md");

        // Create PROJECT.md
        std::fs::write(&project_md_path, "Project instructions content").unwrap();
        // Also create QWEN.md and GEMINI.md to show PROJECT.md has priority
        std::fs::write(&qwen_md_path, "Qwen instructions content").unwrap();
        std::fs::write(&gemini_md_path, "Gemini instructions content").unwrap();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, Some(project_md_path));
    }

    #[test]
    fn test_find_project_instructions_file_found_qwen_md() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let qwen_md_path = project_root.join("QWEN.md");
        let gemini_md_path = project_root.join("GEMINI.md");

        // Create QWEN.md (PROJECT.md does not exist)
        std::fs::write(&qwen_md_path, "Qwen instructions content").unwrap();
        // Also create GEMINI.md to show QWEN.md has priority
        std::fs::write(&gemini_md_path, "Gemini instructions content").unwrap();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, Some(qwen_md_path));
    }

    #[test]
    fn test_find_project_instructions_file_found_gemini_md() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let gemini_md_path = project_root.join("GEMINI.md");

        // Create GEMINI.md (PROJECT.md and QWEN.md do not exist)
        std::fs::write(&gemini_md_path, "Gemini instructions content").unwrap();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, Some(gemini_md_path));
    }

    #[test]
    fn test_find_project_instructions_file_not_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();

        let found_path = find_project_instructions_file(project_root);
        assert_eq!(found_path, None);
    }

    #[test]
    fn test_load_project_instructions_inner_success() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let project_md_path = project_root.join("PROJECT.md");
        let content = "Project instructions content".to_string();

        std::fs::write(&project_md_path, &content).unwrap();

        let mock_reader = create_mock_reader(project_md_path.clone(), content.clone());
        let result = load_project_instructions_inner(project_root, mock_reader);
        
        assert_eq!(result, Some(content));
    }

    #[test]
    fn test_load_project_instructions_inner_not_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();

        let mock_reader = create_mock_reader(
            project_root.join("PROJECT.md"),
            "content".to_string()
        );
        let result = load_project_instructions_inner(project_root, mock_reader);
        
        assert_eq!(result, None);
    }

    #[test]
    fn test_load_project_instructions_inner_read_error() {
        let temp_dir = tempfile::tempdir().unwrap();
        let project_root = temp_dir.path();
        let project_md_path = project_root.join("PROJECT.md");

        // Create the file so it's found, but simulate a read error
        std::fs::write(&project_md_path, "dummy").unwrap();

        let mock_reader = |_path: &Path| -> io::Result<String> {
            Err(io::Error::new(io::ErrorKind::Other, "Simulated read error"))
        };
        let result = load_project_instructions_inner(project_root, mock_reader);
        
        // It should return None on read error and print an error message (we won't test the print)
        assert_eq!(result, None);
    }
}