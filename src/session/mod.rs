use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub created_at: i64,
    pub title: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionData {
    pub meta: SessionMeta,
    pub history: Vec<String>,
}

pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn new_default() -> Result<Self> {
        let base = default_store_dir()?;
        fs::create_dir_all(&base).ok();
        Ok(Self { root: base })
    }

    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).ok();
        Ok(Self { root })
    }

    pub fn list(&self) -> Result<Vec<SessionMeta>> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        for entry in fs::read_dir(&self.root).with_context(|| "read session dir")? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let meta_p = p.join("meta.json");
            if let Ok(s) = fs::read_to_string(&meta_p) {
                if let Ok(meta) = serde_json::from_str::<SessionMeta>(&s) {
                    out.push(meta);
                }
            }
        }
        out.sort_by_key(|m| m.created_at);
        out.reverse();
        Ok(out)
    }

    pub fn create(&self, title: impl Into<String>) -> Result<SessionData> {
        let id = Uuid::new_v4().to_string();
        let dir = self.root.join(&id);
        fs::create_dir_all(&dir)?;
        let meta = SessionMeta {
            id: id.clone(),
            created_at: chrono::Utc::now().timestamp(),
            title: title.into(),
        };
        let data = SessionData {
            meta: meta.clone(),
            history: Vec::new(),
        };
        fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&meta)?)?;
        fs::write(
            dir.join("history.json"),
            serde_json::to_string_pretty(&data.history)?,
        )?;
        Ok(data)
    }

    pub fn load(&self, id: &str) -> Result<SessionData> {
        let dir = self.root.join(id);
        let meta_s = fs::read_to_string(dir.join("meta.json")).with_context(|| "read meta.json")?;
        let meta: SessionMeta = serde_json::from_str(&meta_s).with_context(|| "parse meta")?;
        let hist_s = fs::read_to_string(dir.join("history.json")).unwrap_or_else(|_| "[]".into());
        let history: Vec<String> = serde_json::from_str(&hist_s).unwrap_or_default();
        Ok(SessionData { meta, history })
    }

    pub fn save(&self, data: &SessionData) -> Result<()> {
        let dir = self.root.join(&data.meta.id);
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("meta.json"),
            serde_json::to_string_pretty(&data.meta)?,
        )?;
        fs::write(
            dir.join("history.json"),
            serde_json::to_string_pretty(&data.history)?,
        )?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.root.join(id);
        if dir.exists() {
            fs::remove_dir_all(dir).ok();
        }
        Ok(())
    }
}

fn default_store_dir() -> Result<PathBuf> {
    use std::env;
    if let Ok(xdg) = env::var("XDG_DATA_HOME") {
        return Ok(Path::new(&xdg).join("doge-code/sessions"));
    }
    if let Ok(home) = env::var("HOME") {
        return Ok(Path::new(&home).join(".local/share/doge-code/sessions"));
    }
    Ok(PathBuf::from("./.doge/sessions"))
}
