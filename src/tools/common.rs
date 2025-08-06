use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct FsTools {
    pub(crate) root: PathBuf,
}

impl FsTools {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}
