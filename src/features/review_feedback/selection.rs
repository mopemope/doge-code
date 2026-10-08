//! Exact selectable unified-diff rows; deletion rows retain old-side coordinates.
use super::{Hunk, LineRange, identity};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Selection {
    pub start_row: usize,
    pub end_row: usize,
    pub old: Option<LineRange>,
    pub new: Option<LineRange>,
    pub excerpt: String,
    pub identity: String,
}
#[derive(Debug, Clone)]
pub struct DiffRow {
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
}
pub fn rows(hunk: &Hunk) -> Vec<DiffRow> {
    let (mut old, mut new) = (hunk.old.start, hunk.new.start);
    hunk.excerpt
        .lines()
        .skip(1)
        .filter_map(|text| {
            let (o, n) = match text.as_bytes().first() {
                Some(b' ') => (Some(old), Some(new)),
                Some(b'-') => (Some(old), None),
                Some(b'+') => (None, Some(new)),
                _ => return None,
            };
            if o.is_some() {
                old = old.saturating_add(1);
            }
            if n.is_some() {
                new = new.saturating_add(1);
            }
            Some(DiffRow {
                old: o,
                new: n,
                text: text.into(),
            })
        })
        .collect()
}
pub fn select(hunk: &Hunk, start: usize, end: usize) -> anyhow::Result<Selection> {
    let rows = rows(hunk);
    anyhow::ensure!(
        start <= end && end < rows.len(),
        "Selected rows are outside the source hunk."
    );
    let slice = &rows[start..=end];
    fn range(coords: impl Iterator<Item = usize>) -> Option<LineRange> {
        let values: Vec<_> = coords.collect();
        Some(LineRange {
            start: *values.first()?,
            count: values.len(),
        })
    }
    let excerpt = slice
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Selection {
        start_row: start,
        end_row: end,
        old: range(slice.iter().filter_map(|r| r.old)),
        new: range(slice.iter().filter_map(|r| r.new)),
        identity: identity(&excerpt),
        excerpt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hunk(text: &str) -> Hunk {
        super::super::parse_hunks(&text.lines().map(str::to_owned).collect::<Vec<_>>()).remove(0)
    }
    #[test]
    fn feedback_line_selection_coordinates_deletion_addition_context_and_markers() {
        let h = hunk("@@ -3,3 +3,3 @@\n same\n-古い\n+新しい\n tail\n\\ No newline at end of file");
        assert_eq!(rows(&h).len(), 4);
        let deleted = select(&h, 1, 1).unwrap();
        assert_eq!(deleted.old, Some(LineRange { start: 4, count: 1 }));
        assert_eq!(deleted.new, None);
        assert_eq!(deleted.excerpt, "-古い");
        let added = select(&h, 2, 2).unwrap();
        assert_eq!(added.old, None);
        assert_eq!(added.new, Some(LineRange { start: 4, count: 1 }));
        let range = select(&h, 1, 3).unwrap();
        assert_eq!(range.old, Some(LineRange { start: 4, count: 2 }));
        assert_eq!(range.new, Some(LineRange { start: 4, count: 2 }));
        assert_eq!(
            select(&h, 0, 0).unwrap().old,
            Some(LineRange { start: 3, count: 1 })
        );
        assert!(select(&h, 3, 1).is_err());
        assert!(select(&h, 0, usize::MAX).is_err());
        let deleted_file = hunk("@@ -1,2 +0,0 @@\n-a\n-b");
        assert!(select(&deleted_file, 0, 1).unwrap().new.is_none());
        let new_file = hunk("@@ -0,0 +1,2 @@\n+a\n+b");
        assert!(select(&new_file, 0, 1).unwrap().old.is_none());
    }
}
