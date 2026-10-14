use super::workspace::hash;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Instant,
};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ledger {
    pub version: u32,
    pub plan_digest: String,
    pub status: String,
    pub reason: Option<String>,
    pub rounds_started: u32,
    pub requests_reserved: u64,
    pub tokens_reserved: u64,
    pub provider_reported_tokens: u64,
    pub usage_records: u64,
    pub acceptance: String,
}
#[derive(Debug)]
pub struct Store {
    pub root: PathBuf,
    pub data: Mutex<Ledger>,
    started: Instant,
    deadline_ms: u64,
    request_limit: u64,
    token_limit: u64,
    _lease: fs::File,
}
pub fn write_json(path: &Path, data: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("record parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    temp.write_all(&serde_json::to_vec_pretty(data)?)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
pub fn lock_owner(path: &Path, create: bool) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let lease = options.open(path)?;
    ensure!(
        lease.metadata()?.is_file(),
        "repair owner lease must be regular file"
    );
    lease.try_lock()?;
    Ok(lease)
}
impl Store {
    pub fn create(project: &Path, plan: &super::plan::Plan) -> Result<Self> {
        #[cfg(not(unix))]
        anyhow::bail!("repair currently requires Unix private storage");
        let parent = project.join(".doge/repairs");
        for p in parent.ancestors() {
            if let Ok(meta) = fs::symlink_metadata(p) {
                ensure!(
                    !meta.file_type().is_symlink(),
                    "symlink in repair storage path"
                );
            }
        }
        fs::create_dir_all(&parent)?;
        let root = parent.join(uuid::Uuid::now_v7().to_string());
        fs::create_dir(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        let lease = lock_owner(&root.join("owner.lock"), true)?;
        let store = Self {
            root,
            data: Mutex::new(Ledger {
                version: 1,
                plan_digest: hash(&serde_json::to_vec(plan)?),
                status: "preparing".into(),
                reason: None,
                rounds_started: 0,
                requests_reserved: 0,
                tokens_reserved: 0,
                provider_reported_tokens: 0,
                usage_records: 0,
                acceptance: "pending".into(),
            }),
            started: Instant::now(),
            deadline_ms: plan.total_elapsed_ms,
            request_limit: plan.total_requests,
            token_limit: plan.total_tokens,
            _lease: lease,
        };
        store.save()?;
        write_json(&store.root.join("plan.json"), plan)?;
        Ok(store)
    }
    pub fn save(&self) -> Result<()> {
        write_json(
            &self.root.join("ledger.json"),
            &*self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("ledger poisoned"))?,
        )
    }
    pub fn remaining_ms(&self) -> Result<u64> {
        self.deadline_ms
            .checked_sub(self.started.elapsed().as_millis().try_into()?)
            .filter(|v| *v > 0)
            .context("repair deadline reached")
    }
    pub fn release_lease(&self) -> Result<()> {
        // Finish artifact publication explicitly before review can acquire
        // ownership; do not depend on incidental client/future clone lifetimes.
        self._lease.unlock()?;
        Ok(())
    }
    pub fn reserve_round(&self, plan: &super::plan::Plan) -> Result<()> {
        self.remaining_ms()?;
        {
            let mut d = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("ledger poisoned"))?;
            ensure!(
                d.rounds_started <= plan.max_restarts
                    && d.tokens_reserved
                        .checked_add(plan.per_run.tokens)
                        .is_some_and(|v| v <= plan.total_tokens),
                "repair round/token reservation limit"
            );
            d.rounds_started += 1;
            d.tokens_reserved += plan.per_run.tokens;
            d.status = "agent_running".into();
        }
        self.save()
    }
    pub fn require_active(&self) -> Result<()> {
        self.remaining_ms()?;
        let d = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("ledger poisoned"))?;
        ensure!(
            d.status == "agent_running",
            "repair budget or usage checkpoint stopped workflow"
        );
        ensure!(
            d.requests_reserved == d.usage_records,
            "request usage unknown; no further work"
        );
        Ok(())
    }
    pub fn finish(&self, status: &str, reason: Option<String>) -> Result<()> {
        if matches!(status, "verified_candidate" | "already_verified") {
            self.remaining_ms()?;
        }
        {
            let mut d = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("ledger poisoned"))?;
            d.status = status.into();
            d.reason = reason;
        }
        self.save()
    }
}
impl crate::llm::client_core::RequestAttemptPolicy for Store {
    fn before_attempt(&self) -> Result<()> {
        self.require_active()?;
        {
            let mut d = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("ledger poisoned"))?;
            ensure!(
                d.status == "agent_running" && d.requests_reserved < self.request_limit,
                "repair request limit or inactive ledger"
            );
            ensure!(
                d.requests_reserved == d.usage_records,
                "previous request usage unknown; no further requests"
            );
            d.requests_reserved += 1;
        }
        self.save()
    }
    fn observe_usage(&self, usage: &crate::llm::types::Usage) {
        if let Ok(mut d) = self.data.lock() {
            d.provider_reported_tokens = d
                .provider_reported_tokens
                .saturating_add(u64::from(usage.total_tokens));
            d.usage_records = d.usage_records.saturating_add(1);
            if d.provider_reported_tokens > d.tokens_reserved
                || d.provider_reported_tokens > self.token_limit
            {
                d.status = "token_budget_exhausted".into();
            }
        }
        if self.save().is_err()
            && let Ok(mut d) = self.data.lock()
        {
            d.status = "storage_failed".into();
        }
    }
}
