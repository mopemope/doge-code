#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_count_lines_in_diff() {
        // Test case 1: Simple addition
        let diff_text = "---
+++
@@ -1,1 +1,2 @@
 Line 1
+Line 2";
        assert_eq!(count_lines_in_diff(diff_text), 1);

        // Test case 2: Simple deletion
        let diff_text = "---
+++
@@ -1,2 +1,1 @@
 Line 1
-Line 2";
        assert_eq!(count_lines_in_diff(diff_text), 1);

        // Test case 3: Modification (delete + add)
        let diff_text = "---
+++
@@ -1,2 +1,2 @@
 Line 1
-Line 2
+Line Two";
        assert_eq!(count_lines_in_diff(diff_text), 2);

        // Test case 4: Multiple changes
        let diff_text = "---
+++
@@ -1,3 +1,3 @@
 Line 1
-Line 2
+Line Two
 Line 3
+Line 4";
        assert_eq!(count_lines_in_diff(diff_text), 3);
    }
}