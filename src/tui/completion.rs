use chrono::{DateTime, Utc};
use globwalk::GlobWalkerBuilder;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub rel: String,
    #[allow(dead_code)]
    pub size: u64,
    pub mtime: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Clone)]
pub struct RecentLRU {
    cap: usize,
    items: Vec<String>,
}

impl RecentLRU {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            items: Vec::new(),
        }
    }
    pub fn touch(&mut self, rel: &str) {
        if let Some(i) = self.items.iter().position(|s| s == rel) {
            self.items.remove(i);
        }
        self.items.insert(0, rel.to_string());
        if self.items.len() > self.cap {
            self.items.pop();
        }
    }
    pub fn rank(&self, rel: &str) -> Option<usize> {
        self.items.iter().position(|s| s == rel)
    }
}

#[derive(Clone)]
pub struct AtFileIndex {
    root: PathBuf,
    pub entries: Arc<RwLock<Vec<FileEntry>>>,
    pub recent: Arc<RwLock<RecentLRU>>,
}

impl AtFileIndex {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            entries: Arc::new(RwLock::new(Vec::new())),
            recent: Arc::new(RwLock::new(RecentLRU::new(256))),
        }
    }

    pub fn scan(&self) {
        let root = self.root.clone();
        let mut v = Vec::new();
        let walker = GlobWalkerBuilder::from_patterns(&root, &["**/*"]) // respect .gitignore by default via globwalk
            .follow_links(false)
            .case_insensitive(true)
            .max_depth(64)
            .build();
        if let Ok(walker) = walker {
            for e in walker.filter_map(Result::ok) {
                let p = e.path();
                if p.is_dir() {
                    continue;
                }
                if should_skip(p) {
                    continue;
                }
                if let Ok(relp) = p.strip_prefix(&root) {
                    let rel = relp.to_string_lossy().replace('\\', "/");
                    let (size, mtime) = file_meta(p);
                    v.push(FileEntry { rel, size, mtime });
                }
            }
        }
        v.sort_by(|a, b| a.rel.cmp(&b.rel));
        if let Ok(mut guard) = self.entries.write() {
            *guard = v;
        }
    }

    pub fn complete(&self, query: &str) -> Vec<FileEntry> {
        let q = query.trim_start_matches('@');
        let entries = self
            .entries
            .read()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default();
        let recent = self.recent.read().ok();
        let mut scored: Vec<(i32, i32, i32, i64, usize)> = Vec::new();
        for (idx, e) in entries.iter().enumerate() {
            let (tier, score) = match_tier(&e.rel, q);

            if tier == i32::MAX {
                continue;
            }

            let rec_rank = recent
                .as_ref()
                .and_then(|r| r.rank(&e.rel))
                .map(|x| x as i32)
                .unwrap_or(9999);

            let mtime_key = e.mtime.map(|t| t.timestamp()).unwrap_or(0);
            // store index to pick entry later
            scored.push((tier, -score, rec_rank, -mtime_key, idx));
        }
        scored.sort();
        scored
            .into_iter()
            .filter_map(|t| entries.get(t.4).cloned())
            .take(50)
            .collect()
    }
}

fn should_skip(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.contains("/.git/")
        || s.starts_with(".git/")
        || s.contains("/target/")
        || s.contains("/node_modules/")
}

fn file_meta(p: &Path) -> (u64, Option<DateTime<Utc>>) {
    let md = std::fs::metadata(p).ok();
    let size = md.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = md
        .and_then(|m| m.modified().ok())
        .and_then(|t| DateTime::<Utc>::from(t).into());
    (size, mtime)
}

fn match_tier(base: &str, pat: &str) -> (i32, i32) {
    if pat.is_empty() {
        return (1, 0);
    }
    let bl = base.to_ascii_lowercase();
    let pl = pat.to_ascii_lowercase();
    if bl.starts_with(&pl) {
        return (0, pl.len() as i32);
    }
    if bl.contains(&pl) {
        return (1, pl.len() as i32);
    }
    let f = fuzzy_score(&bl, &pl);
    if f > 0 { (2, f) } else { (i32::MAX, 0) }
}

fn fuzzy_score(text: &str, pat: &str) -> i32 {
    let mut ti = 0usize;
    let mut score = 0i32;
    let tb = text.as_bytes();
    let pb = pat.as_bytes();
    for &pc in pb {
        while ti < tb.len() && tb[ti] != pc {
            ti += 1;
            score -= 1;
        }
        if ti == tb.len() {
            return 0;
        }
        score += 5;
        ti += 1;
    }
    score
}

#[derive(Debug, Default, Clone)]
pub struct CompletionState {
    pub visible: bool,
    pub query: String,
    pub items: Vec<FileEntry>,
    pub selected: usize,
    // suppress reopening completion once right after it was closed/applied
    pub suppress_once: bool,
}

impl CompletionState {
    pub fn reset(&mut self) {
        self.visible = false;
        self.query.clear();
        self.items.clear();
        self.selected = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn completion_filtering_works() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // Create some dummy files
        File::create(root.join("README.md")).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        File::create(root.join("src/main.rs")).unwrap();
        File::create(root.join("src/lib.rs")).unwrap();
        std::fs::create_dir(root.join("src/tui")).unwrap();
        File::create(root.join("src/tui/completion.rs")).unwrap();

        let index = AtFileIndex::new(root);
        index.scan();

        // Test completion for "src/"
        let completions = index.complete("@src/");
        assert_eq!(completions.len(), 3);
        assert!(completions.iter().any(|e| e.rel == "src/main.rs"));
        assert!(completions.iter().any(|e| e.rel == "src/lib.rs"));
        assert!(completions.iter().any(|e| e.rel == "src/tui/completion.rs"));

        // Test completion for "src/tui/comp"
        let completions = index.complete("@src/tui/comp");
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].rel, "src/tui/completion.rs");

        // Test completion for "README"
        let completions = index.complete("@README");
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].rel, "README.md");

        // Test for empty query
        let completions = index.complete("@");
        assert_eq!(completions.len(), 4);
    }
}
