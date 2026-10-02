//! Tool Routing / Tool Catalog.
//!
//! Separates the tool *inventory* (every known built-in + remote MCP tool)
//! from the *active set* actually sent to the LLM on each iteration.
//! Deferred tools stay out of the request payload until `tool_search`
//! activates them; activation is sticky for the agent run.
//!
//! Provider-independent: ranking is deterministic lexical scoring with no
//! embeddings, no vector DB, and no provider-specific branches.

use std::collections::{BTreeMap, BTreeSet};

use tokio::sync::RwLock;
use tracing::debug;

use crate::config::tool_routing::{
    MAX_TOOL_SEARCH_RESULT_LIMIT, MIN_TOOL_SEARCH_RESULT_LIMIT, ToolRoutingConfig,
};
use crate::llm::types::{ChatMessage, ToolDef};
use crate::tools::remote_tools::RemoteToolInfo;
use crate::tools::tool_search::{TOOL_SEARCH_DESC_CHARS, TOOL_SEARCH_TOOL_NAME};

/// Tools always visible in deferred mode: navigation, reading, literal
/// search, isolated research, and structured verification.
pub const CORE_EAGER_TOOLS: &[&str] = &[
    "search_repomap",
    "fs_read",
    "search_text",
    "task",
    "execute_process",
    "observation_read",
];

/// Where a catalog entry came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSource {
    Builtin,
    RemoteMcp {
        server_name: String,
        remote_name: String,
    },
}

impl ToolSource {
    /// Short source label used in `tool_search` results.
    pub fn label(&self) -> &'static str {
        match self {
            ToolSource::Builtin => "builtin",
            ToolSource::RemoteMcp { .. } => "mcp",
        }
    }

    pub fn server_name(&self) -> Option<&str> {
        match self {
            ToolSource::Builtin => None,
            ToolSource::RemoteMcp { server_name, .. } => Some(server_name.as_str()),
        }
    }

    pub fn remote_name(&self) -> Option<&str> {
        match self {
            ToolSource::Builtin => None,
            ToolSource::RemoteMcp { remote_name, .. } => Some(remote_name.as_str()),
        }
    }
}

/// One known tool plus its origin and precomputed searchable text.
#[derive(Debug, Clone)]
pub struct ToolCatalogEntry {
    pub definition: ToolDef,
    pub source: ToolSource,
    pub searchable_text: String,
}

impl ToolCatalogEntry {
    pub fn name(&self) -> &str {
        &self.definition.function.name
    }

    pub fn description(&self) -> &str {
        &self.definition.function.description
    }
}

/// A ranked `tool_search` hit (no full schema; schemas ship only in the
/// next LLM request's `tools` array).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSearchHit {
    pub name: String,
    pub source: ToolSource,
    pub server: Option<String>,
    pub description: String,
    pub score: i64,
}

/// Ranked search result.
///
/// Progress-monotonic contract: `hits` carries the top inactive matches
/// first (activation capacity, bounded by the search limit), followed by
/// already-active matches (transparency only, bounded independently so
/// reporting can never starve activation).
#[derive(Debug, Clone)]
pub struct ToolSearchResult {
    pub query: String,
    pub hits: Vec<ToolSearchHit>,
    /// Real deferred tools still inactive after this search
    /// (pre-activation count, excluding managed `tool_search`).
    pub remaining_deferred: usize,
}

/// Inventory of all known tools plus the currently LLM-visible active set.
///
/// Built once per agent run from a `RemoteToolManager` snapshot; never
/// re-discovers MCP servers itself.
pub struct ToolCatalog {
    entries: BTreeMap<String, ToolCatalogEntry>,
    active: RwLock<BTreeSet<String>>,
    deferred: bool,
    search_result_limit: usize,
}

impl std::fmt::Debug for ToolCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCatalog")
            .field("total", &self.entries.len())
            .field("deferred", &self.deferred)
            .finish_non_exhaustive()
    }
}

impl ToolCatalog {
    /// Build from built-in definitions plus a remote snapshot.
    pub fn from_parts(
        builtin_defs: Vec<ToolDef>,
        remote_infos: &[RemoteToolInfo],
        routing: &ToolRoutingConfig,
    ) -> Self {
        let mut entries: BTreeMap<String, ToolCatalogEntry> = BTreeMap::new();
        for def in builtin_defs {
            // `tool_search` itself is catalog-managed (deferred-only); a
            // builtin of the same name would collide with the managed entry.
            if def.function.name == TOOL_SEARCH_TOOL_NAME {
                continue;
            }
            let entry = ToolCatalogEntry {
                searchable_text: build_searchable_text(&def, &ToolSource::Builtin),
                definition: def.clone(),
                source: ToolSource::Builtin,
            };
            entries.insert(def.function.name.clone(), entry);
        }
        for info in remote_infos {
            let description = info.description.clone().unwrap_or_else(|| {
                format!(
                    "Remote MCP tool '{}' from server '{}'",
                    info.remote_name, info.server_name
                )
            });
            let def = ToolDef {
                kind: "function".into(),
                function: crate::llm::types::ToolFunctionDef {
                    name: info.alias.clone(),
                    description,
                    parameters: info.parameters.clone(),
                    strict: info.strict,
                },
            };
            let source = ToolSource::RemoteMcp {
                server_name: info.server_name.clone(),
                remote_name: info.remote_name.clone(),
            };
            let entry = ToolCatalogEntry {
                searchable_text: build_searchable_text(&def, &source),
                definition: def,
                source,
            };
            // Alias generation is owned by `RemoteToolManager`; never
            // reconstructed here. First-seen wins on pathological collision.
            entries.entry(info.alias.clone()).or_insert(entry);
        }
        // The managed discovery tool is part of the inventory so history
        // scans and lookups resolve it, but it is only *active* in deferred
        // mode when something remains to discover.
        let search_def = crate::tools::tool_search::tool_def();
        let search_entry = ToolCatalogEntry {
            searchable_text: build_searchable_text(&search_def, &ToolSource::Builtin),
            definition: search_def,
            source: ToolSource::Builtin,
        };
        entries.insert(TOOL_SEARCH_TOOL_NAME.to_string(), search_entry);

        let deferred = routing.is_deferred_for_count(entries.len());
        let initial = initial_active_set(&entries, deferred);

        debug!(
            total = entries.len(),
            active = initial.len(),
            deferred,
            mode = ?routing.mode,
            "tool catalog built"
        );

        Self {
            entries,
            active: RwLock::new(initial),
            deferred,
            search_result_limit: routing.effective_limit(),
        }
    }

    /// Test/fixture constructor from explicit entries.
    pub fn from_entries(entries: Vec<ToolCatalogEntry>, routing: &ToolRoutingConfig) -> Self {
        let mut map: BTreeMap<String, ToolCatalogEntry> = BTreeMap::new();
        for e in entries {
            // Mirror `from_parts`: a caller-supplied entry named `tool_search`
            // would collide with the managed discovery entry, so it is
            // dropped in favor of the canonical managed definition below.
            if e.definition.function.name == TOOL_SEARCH_TOOL_NAME {
                continue;
            }
            map.insert(e.definition.function.name.clone(), e);
        }
        // Mirror `from_parts`: the managed discovery tool is always in the
        // inventory so counts and lookups behave identically.
        let search_def = crate::tools::tool_search::tool_def();
        let search_entry = ToolCatalogEntry {
            searchable_text: build_searchable_text(&search_def, &ToolSource::Builtin),
            definition: search_def,
            source: ToolSource::Builtin,
        };
        map.insert(TOOL_SEARCH_TOOL_NAME.to_string(), search_entry);
        let deferred = routing.is_deferred_for_count(map.len());
        let initial = initial_active_set(&map, deferred);
        Self {
            entries: map,
            active: RwLock::new(initial),
            deferred,
            search_result_limit: routing.effective_limit(),
        }
    }

    pub fn all_tool_count(&self) -> usize {
        self.entries.len()
    }

    pub async fn active_count(&self) -> usize {
        self.active.read().await.len()
    }

    pub async fn deferred_count(&self) -> usize {
        let active = self.active.read().await;
        self.real_deferred_count(&active)
    }

    /// Real inactive tools: every catalog entry except managed
    /// `tool_search`, minus the active set. Discovery machinery is never
    /// deferred work, so it is excluded in both eager and deferred modes.
    fn real_deferred_count(&self, active: &BTreeSet<String>) -> usize {
        self.entries
            .keys()
            .filter(|name| is_real_tool(name) && !active.contains(name.as_str()))
            .count()
    }

    pub fn is_deferred(&self) -> bool {
        self.deferred
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub async fn is_active(&self, name: &str) -> bool {
        self.active.read().await.contains(name)
    }

    /// Full inventory (insertion-independent name order). Used for the
    /// sub-agent allowlist lookup and size-regression tests; never sent to
    /// the LLM directly in deferred mode.
    pub fn all_tool_defs(&self) -> Vec<ToolDef> {
        self.entries
            .values()
            .map(|e| e.definition.clone())
            .collect()
    }

    /// Currently LLM-visible schemas in stable name order.
    pub async fn active_tool_defs(&self) -> Vec<ToolDef> {
        let active = self.active.read().await;
        active
            .iter()
            .filter_map(|name| self.entries.get(name))
            .map(|e| e.definition.clone())
            .collect()
    }

    pub async fn active_names(&self) -> Vec<String> {
        self.active.read().await.iter().cloned().collect()
    }

    /// Activate known tools. Unknown names are ignored; re-activating an
    /// active tool is a no-op. Returns newly activated names in sorted order.
    /// Once no real deferred tools remain, managed `tool_search` is retired
    /// from the active set so the next schema snapshot omits it.
    pub async fn activate(&self, names: &[String]) -> Vec<String> {
        let mut guard = self.active.write().await;
        let mut newly = Vec::new();
        for name in names {
            if self.entries.contains_key(name) && guard.insert(name.clone()) {
                newly.push(name.clone());
            }
        }
        newly.sort();
        if !newly.is_empty() {
            debug!(activated = newly.len(), "tool catalog activated tools");
        }
        if self.real_deferred_count(&guard) == 0 {
            guard.remove(TOOL_SEARCH_TOOL_NAME);
            // Retirement is sticky: a stale explicit activation of discovery
            // reports nothing newly activated and leaves it inactive.
            newly.retain(|name| name != TOOL_SEARCH_TOOL_NAME);
        }
        newly
    }

    /// Resume compatibility: re-activate catalog tools referenced by prior
    /// assistant tool calls so resumed history stays coherent.
    pub async fn activate_known_from_history(&self, messages: &[ChatMessage]) -> Vec<String> {
        let mut names = Vec::new();
        for msg in messages {
            for tc in &msg.tool_calls {
                let name = tc.function.name.as_str();
                if name == TOOL_SEARCH_TOOL_NAME {
                    continue;
                }
                if self.entries.contains_key(name) {
                    names.push(name.to_string());
                } else {
                    debug!(tool = name, "history references unknown tool; ignoring");
                }
            }
        }
        names.sort();
        names.dedup();
        self.activate(&names).await
    }

    /// Deterministic lexical search over deferred + active tools.
    /// Pure ranking; callers decide activation. `limit == 0` falls back to
    /// the configured default; values above the hard max are clamped.
    ///
    /// Progress-monotonic: active and inactive matches are partitioned
    /// *before* any limit is applied. The limit bounds inactive activation
    /// capacity; already-active matches are reported separately under the
    /// same independent bound, so high-ranking active tools can never crowd
    /// out a relevant inactive match. `hits` lists inactive matches first
    /// (score desc, name asc), then active matches (same order).
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        server: Option<&str>,
    ) -> ToolSearchResult {
        let limit = if limit == 0 {
            self.search_result_limit
        } else {
            limit.clamp(MIN_TOOL_SEARCH_RESULT_LIMIT, MAX_TOOL_SEARCH_RESULT_LIMIT)
        };
        let trimmed = query.trim();
        // Snapshot active state under a short read lock, then score lock-free.
        // Holding the guard across the scoring loop would block `activate`
        // writers for the whole ranking pass and risks lock-order issues.
        let (active_snapshot, remaining_deferred) = {
            let active = self.active.read().await;
            (active.clone(), self.real_deferred_count(&active))
        };
        let active = active_snapshot;
        if trimmed.is_empty() {
            return ToolSearchResult {
                query: query.to_string(),
                hits: Vec::new(),
                remaining_deferred,
            };
        }
        let query_norm = trimmed.to_lowercase();
        let query_tokens = tokenize(trimmed);
        let server_filter = server
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());

        let mut inactive: Vec<ToolSearchHit> = Vec::new();
        let mut already_active: Vec<ToolSearchHit> = Vec::new();
        // BTreeMap iteration is name-ordered; final sort breaks score ties
        // by name so results never depend on hash iteration order.
        for entry in self.entries.values() {
            if entry.name() == TOOL_SEARCH_TOOL_NAME {
                continue;
            }
            if let Some(ref wanted) = server_filter {
                let matches = entry
                    .source
                    .server_name()
                    .is_some_and(|s| s.to_lowercase() == *wanted);
                if !matches {
                    continue;
                }
            }
            let score = score_entry(entry, &query_norm, &query_tokens);
            if score > 0 {
                let hit = ToolSearchHit {
                    name: entry.name().to_string(),
                    source: entry.source.clone(),
                    server: entry.source.server_name().map(str::to_string),
                    description: truncate_chars(entry.description(), TOOL_SEARCH_DESC_CHARS),
                    score,
                };
                if active.contains(entry.name()) {
                    already_active.push(hit);
                } else {
                    inactive.push(hit);
                }
            }
        }
        for ranked in [&mut inactive, &mut already_active] {
            ranked.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
        }
        inactive.truncate(limit);
        already_active.truncate(limit);
        let total = inactive.len() + already_active.len();
        debug!(
            query_len = trimmed.len(),
            matches = total,
            limit,
            "tool_search ranked"
        );
        inactive.extend(already_active);
        ToolSearchResult {
            query: query.to_string(),
            hits: inactive,
            remaining_deferred,
        }
    }
}

/// Managed discovery machinery is lifecycle, not real deferred work.
fn is_real_tool(name: &str) -> bool {
    name != TOOL_SEARCH_TOOL_NAME
}

/// Initial LLM-visible set for one routing decision. Shared by both
/// constructors so `from_parts` and `from_entries` can never drift apart.
fn initial_active_set(
    entries: &BTreeMap<String, ToolCatalogEntry>,
    deferred: bool,
) -> BTreeSet<String> {
    let mut initial = BTreeSet::new();
    if deferred {
        for name in CORE_EAGER_TOOLS {
            if entries.contains_key(*name) {
                initial.insert((*name).to_string());
            }
        }
        let has_deferred = entries
            .keys()
            .any(|name| name != TOOL_SEARCH_TOOL_NAME && !initial.contains(name));
        if has_deferred {
            initial.insert(TOOL_SEARCH_TOOL_NAME.to_string());
        }
    } else {
        // Eager mode: the visible set matches the legacy inventory exactly
        // (same tool set, in stable name order). `tool_search` adds no value
        // when everything is already visible, so it stays inactive.
        for name in entries.keys() {
            if name != TOOL_SEARCH_TOOL_NAME {
                initial.insert(name.clone());
            }
        }
    }
    initial
}

/// Build the lowercase searchable text for one tool.
pub fn build_searchable_text(def: &ToolDef, source: &ToolSource) -> String {
    let mut parts = Vec::new();
    parts.push(def.function.name.to_lowercase());
    parts.push(def.function.description.to_lowercase());
    if let serde_json::Value::Object(props) = def
        .function
        .parameters
        .get("properties")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()))
    {
        for (prop_name, prop_schema) in &props {
            parts.push(prop_name.to_lowercase());
            if let Some(desc) = prop_schema.get("description").and_then(|v| v.as_str()) {
                parts.push(desc.to_lowercase());
            }
        }
    }
    match source {
        ToolSource::Builtin => {}
        ToolSource::RemoteMcp {
            server_name,
            remote_name,
        } => {
            parts.push(server_name.to_lowercase());
            parts.push(remote_name.to_lowercase());
        }
    }
    parts.join(" ")
}

/// Split on anything non-alphanumeric; lowercase; drop empties.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Name tokens split on separators (`_`, `-`, `/`, `.`, …).
fn name_tokens(name: &str) -> Vec<String> {
    tokenize(name)
}

fn param_texts(def: &ToolDef) -> (Vec<String>, Vec<String>) {
    let mut names = Vec::new();
    let mut descs = Vec::new();
    if let Some(props) = def
        .function
        .parameters
        .get("properties")
        .and_then(|v| v.as_object())
    {
        for (prop_name, schema) in props {
            names.push(prop_name.to_lowercase());
            if let Some(d) = schema.get("description").and_then(|v| v.as_str()) {
                descs.push(d.to_lowercase());
            }
        }
    }
    (names, descs)
}

/// Deterministic lexical score. Priority (highest first):
/// exact tool-name > exact remote-name > name prefix > server exact >
/// name substring > name-token overlap > description/parameter overlap.
fn score_entry(entry: &ToolCatalogEntry, query_norm: &str, query_tokens: &[String]) -> i64 {
    let name_lower = entry.name().to_lowercase();
    let desc_lower = entry.description().to_lowercase();
    let name_toks = name_tokens(entry.name());
    let desc_toks = tokenize(entry.description());
    let (param_names, param_descs) = param_texts(&entry.definition);
    let param_desc_joined = param_descs.join(" ");

    // Exact identity matches dominate: they stack with the prefix /
    // substring / token bonuses below, so an exact name always outranks a
    // longer tool that merely contains it as a prefix.
    let mut score: i64 = 0;
    if *query_norm == name_lower {
        score += 10_000;
    }
    if let Some(remote) = entry.source.remote_name()
        && *query_norm == remote.to_lowercase()
    {
        score += 9_000;
    }

    let single_token = !query_norm.contains(' ') && !query_tokens.is_empty();

    if single_token && name_lower.starts_with(query_norm) {
        score += 5_000;
    }
    // Exact server-name match on any query token.
    if let Some(server) = entry.source.server_name() {
        let server_lower = server.to_lowercase();
        if query_tokens.contains(&server_lower) {
            score += 3_000;
        } else if query_tokens
            .iter()
            .any(|t| server_lower.contains(t.as_str()))
        {
            score += 100;
        }
    }
    if single_token && name_lower.contains(query_norm) {
        score += 2_000;
    }
    // Full-phrase hit in the description.
    if desc_lower.contains(query_norm) {
        score += 100;
    }

    for token in query_tokens {
        if name_toks.contains(token) {
            score += 500;
        } else if name_lower.contains(token.as_str()) {
            score += 200;
        } else if let Some(remote) = entry.source.remote_name() {
            let remote_lower = remote.to_lowercase();
            if remote_lower == *token {
                score += 150;
            } else if remote_lower.contains(token.as_str()) {
                score += 100;
            }
        }
        if desc_toks.contains(token) {
            score += 50;
        } else if desc_lower.contains(token.as_str()) {
            score += 20;
        }
        if param_names.contains(token) {
            score += 30;
        } else if param_names.iter().any(|p| p.contains(token.as_str())) {
            score += 10;
        } else if param_desc_joined.contains(token.as_str()) {
            score += 5;
        }
        if entry.searchable_text.contains(token.as_str()) {
            score += 1;
        }
    }
    score
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(max_chars).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tool_routing::{ToolRoutingConfig, ToolRoutingMode};
    use serde_json::json;

    fn builtin_def(name: &str, description: &str) -> ToolDef {
        ToolDef {
            kind: "function".into(),
            function: crate::llm::types::ToolFunctionDef {
                name: name.to_string(),
                description: description.to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Absolute path of the file"},
                        "content": {"type": "string", "description": "Text content to write"}
                    },
                    "required": ["path"]
                }),
                strict: None,
            },
        }
    }

    fn entry(name: &str, description: &str) -> ToolCatalogEntry {
        let def = builtin_def(name, description);
        let text = build_searchable_text(&def, &ToolSource::Builtin);
        ToolCatalogEntry {
            definition: def,
            source: ToolSource::Builtin,
            searchable_text: text,
        }
    }

    fn remote_entry(
        alias: &str,
        server: &str,
        remote: &str,
        description: &str,
    ) -> ToolCatalogEntry {
        let def = ToolDef {
            kind: "function".into(),
            function: crate::llm::types::ToolFunctionDef {
                name: alias.to_string(),
                description: description.to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "description": "Issue or pull request id"}
                    }
                }),
                strict: None,
            },
        };
        let source = ToolSource::RemoteMcp {
            server_name: server.to_string(),
            remote_name: remote.to_string(),
        };
        let text = build_searchable_text(&def, &source);
        ToolCatalogEntry {
            definition: def,
            source,
            searchable_text: text,
        }
    }

    fn deferred_routing() -> ToolRoutingConfig {
        ToolRoutingConfig {
            mode: ToolRoutingMode::Deferred,
            search_result_limit: 5,
        }
    }

    fn eager_routing() -> ToolRoutingConfig {
        ToolRoutingConfig {
            mode: ToolRoutingMode::Eager,
            search_result_limit: 5,
        }
    }

    fn core_entries() -> Vec<ToolCatalogEntry> {
        vec![
            entry(
                "search_repomap",
                "Search parsed code symbols with advanced filtering",
            ),
            entry("fs_read", "Read a file from disk"),
            entry("search_text", "Grep-like text search across files"),
            entry("task", "Delegate research to an isolated sub-agent"),
            entry("execute_process", "Run a program directly without a shell"),
            entry(
                "observation_read",
                "Retrieve an offloaded tool result without rerunning the tool",
            ),
            entry(
                "edit",
                "Replaces a single unique text block in a file for surgical edits",
            ),
            entry(
                "apply_patch",
                "Applies a unified diff patch to a file with context matching",
            ),
            entry(
                "fs_write",
                "Writes or overwrites a file completely. For partial edits use edit or apply_patch",
            ),
        ]
    }

    #[tokio::test]
    async fn test_deferred_initial_active_set() {
        let catalog = ToolCatalog::from_entries(core_entries(), &deferred_routing());
        assert!(catalog.is_deferred());
        for core in CORE_EAGER_TOOLS {
            assert!(
                catalog.is_active(core).await,
                "{core} should be eagerly active"
            );
        }
        assert!(!catalog.is_active("edit").await);
        assert!(!catalog.is_active("apply_patch").await);
        assert!(!catalog.is_active("fs_write").await);
        assert!(catalog.is_active(TOOL_SEARCH_TOOL_NAME).await);
    }

    #[tokio::test]
    async fn test_eager_mode_activates_everything_except_search() {
        let catalog = ToolCatalog::from_entries(core_entries(), &eager_routing());
        assert!(!catalog.is_deferred());
        for name in [
            "search_repomap",
            "fs_read",
            "search_text",
            "task",
            "execute_process",
            "observation_read",
            "edit",
            "apply_patch",
            "fs_write",
        ] {
            assert!(catalog.is_active(name).await, "{name} should be active");
        }
        assert!(!catalog.is_active(TOOL_SEARCH_TOOL_NAME).await);
    }

    #[tokio::test]
    async fn test_auto_mode_threshold_boundary() {
        // 9 entries + managed tool_search = 10 total -> deferred.
        let nine: Vec<ToolCatalogEntry> = (0..9)
            .map(|i| entry(&format!("tool_{i}"), "a helper tool"))
            .collect();
        let auto = ToolRoutingConfig {
            mode: ToolRoutingMode::Auto,
            search_result_limit: 5,
        };
        let catalog = ToolCatalog::from_entries(nine, &auto);
        assert_eq!(catalog.all_tool_count(), 10);
        assert!(catalog.is_deferred());
        // 8 entries + managed tool_search = 9 total -> eager.
        let eight: Vec<ToolCatalogEntry> = (0..8)
            .map(|i| entry(&format!("tool_{i}"), "a helper tool"))
            .collect();
        let catalog = ToolCatalog::from_entries(eight, &auto);
        assert_eq!(catalog.all_tool_count(), 9);
        assert!(!catalog.is_deferred());
    }

    #[tokio::test]
    async fn test_lexical_patch_query_finds_edit_tools() {
        let catalog = ToolCatalog::from_entries(core_entries(), &deferred_routing());
        // Single-capability query ranks the patch tool first.
        let result = catalog.search("patch source code", 5, None).await;
        let names: Vec<&str> = result.hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"apply_patch"), "hits: {names:?}");
        // The canonical multi-token query surfaces the whole edit family:
        // `edit` (name match), `apply_patch` (name match), `fs_write`
        // (description mentions edit/apply_patch).
        let result = catalog.search("edit source code patch", 5, None).await;
        let names: Vec<&str> = result.hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"apply_patch"), "hits: {names:?}");
        assert!(names.contains(&"edit"), "hits: {names:?}");
        assert!(names.contains(&"fs_write"), "hits: {names:?}");
    }

    #[tokio::test]
    async fn test_github_query_ranks_github_tools() {
        let entries = vec![
            remote_entry(
                "mcp_github_get_pull_request",
                "github",
                "get_pull_request",
                "Get a GitHub pull request by number",
            ),
            remote_entry(
                "mcp_github_add_review",
                "github",
                "add_review",
                "Add a review to a GitHub pull request",
            ),
            remote_entry(
                "mcp_slack_post_message",
                "slack",
                "post_message",
                "Post a message to a Slack channel",
            ),
            entry("fs_read", "Read a file from disk"),
        ];
        let catalog = ToolCatalog::from_entries(entries, &deferred_routing());
        let result = catalog.search("github pull request review", 5, None).await;
        assert!(result.hits.len() >= 2, "hits: {:?}", result.hits);
        assert_eq!(result.hits[0].server.as_deref(), Some("github"));
        let names: Vec<&str> = result.hits.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"mcp_github_get_pull_request"));
        assert!(names.contains(&"mcp_github_add_review"));
    }

    #[tokio::test]
    async fn test_server_filter_excludes_other_servers() {
        let entries = vec![
            remote_entry(
                "mcp_github_get_pull_request",
                "github",
                "get_pull_request",
                "Get a GitHub pull request",
            ),
            remote_entry(
                "mcp_slack_post_message",
                "slack",
                "post_message",
                "Post a message to Slack",
            ),
        ];
        let catalog = ToolCatalog::from_entries(entries, &deferred_routing());
        let result = catalog.search("message", 5, Some("github")).await;
        assert!(
            result
                .hits
                .iter()
                .all(|h| h.server.as_deref() == Some("github")),
            "hits: {:?}",
            result.hits
        );
        assert!(
            result.hits.iter().all(|h| h.name.contains("github")),
            "hits: {:?}",
            result.hits
        );
    }

    #[tokio::test]
    async fn test_exact_alias_ranks_first() {
        let entries = vec![
            remote_entry(
                "mcp_github_get_pull_request",
                "github",
                "get_pull_request",
                "Get a GitHub pull request",
            ),
            remote_entry(
                "mcp_github_get_pull_request_diff",
                "github",
                "get_pull_request_diff",
                "Get the diff of a GitHub pull request",
            ),
        ];
        let catalog = ToolCatalog::from_entries(entries, &deferred_routing());
        let result = catalog.search("mcp_github_get_pull_request", 5, None).await;
        assert!(!result.hits.is_empty());
        assert_eq!(result.hits[0].name, "mcp_github_get_pull_request");
    }

    #[tokio::test]
    async fn test_linear_query_finds_linear_tool() {
        let entries = vec![
            remote_entry(
                "mcp_linear_create_issue",
                "linear",
                "create_issue",
                "Create a Linear task issue for the team",
            ),
            remote_entry(
                "mcp_github_create_issue",
                "github",
                "create_issue",
                "Create a GitHub issue in a repository",
            ),
            remote_entry(
                "mcp_slack_post_message",
                "slack",
                "post_message",
                "Send a message to a Slack channel",
            ),
        ];
        let catalog = ToolCatalog::from_entries(entries, &deferred_routing());
        let result = catalog.search("create linear task", 5, None).await;
        assert!(!result.hits.is_empty());
        assert_eq!(result.hits[0].name, "mcp_linear_create_issue");
        let names: Vec<&str> = result.hits.iter().map(|h| h.name.as_str()).collect();
        assert!(
            names.contains(&"mcp_linear_create_issue"),
            "hits: {names:?}"
        );
    }

    #[tokio::test]
    async fn test_ranking_is_stable() {
        let entries = vec![
            entry("tool_b", "shared helper tool alpha"),
            entry("tool_a", "shared helper tool alpha"),
            entry("tool_c", "shared helper tool alpha"),
        ];
        let catalog = ToolCatalog::from_entries(entries, &deferred_routing());
        let first = catalog.search("helper", 5, None).await;
        let second = catalog.search("helper", 5, None).await;
        let a: Vec<&str> = first.hits.iter().map(|h| h.name.as_str()).collect();
        let b: Vec<&str> = second.hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(a, b);
        assert_eq!(a, vec!["tool_a", "tool_b", "tool_c"]);
    }

    #[tokio::test]
    async fn test_limit_clamping() {
        let entries: Vec<ToolCatalogEntry> = (0..20)
            .map(|i| entry(&format!("helper_tool_{i:02}"), "helper tool for testing"))
            .collect();
        let catalog = ToolCatalog::from_entries(entries, &deferred_routing());
        let one = catalog.search("helper", 1, None).await;
        assert_eq!(one.hits.len(), 1);
        let huge = catalog.search("helper", 100, None).await;
        assert_eq!(huge.hits.len(), MAX_TOOL_SEARCH_RESULT_LIMIT);
        let def = catalog.search("helper", 0, None).await;
        assert_eq!(def.hits.len(), 5);
    }

    #[tokio::test]
    async fn test_duplicate_activation_is_noop() {
        let catalog = ToolCatalog::from_entries(core_entries(), &deferred_routing());
        let before = catalog.active_count().await;
        let first = catalog.activate(&["edit".to_string()]).await;
        assert_eq!(first, vec!["edit".to_string()]);
        let second = catalog.activate(&["edit".to_string()]).await;
        assert!(second.is_empty());
        assert_eq!(catalog.active_count().await, before + 1);
        // Unknown tools are ignored.
        let unknown = catalog.activate(&["no_such_tool".to_string()]).await;
        assert!(unknown.is_empty());
    }

    #[tokio::test]
    async fn test_empty_and_unknown_queries_do_not_panic() {
        let catalog = ToolCatalog::from_entries(core_entries(), &deferred_routing());
        let empty = catalog.search("   ", 5, None).await;
        assert!(empty.hits.is_empty());
        let unknown = catalog.search("zzz_no_such_capability_qqq", 5, None).await;
        assert!(unknown.hits.is_empty());
    }

    #[tokio::test]
    async fn test_history_activation() {
        let catalog = ToolCatalog::from_entries(core_entries(), &deferred_routing());
        assert!(!catalog.is_active("edit").await);
        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some("1".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "edit".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        }];
        let activated = catalog.activate_known_from_history(&messages).await;
        assert_eq!(activated, vec!["edit".to_string()]);
        assert!(catalog.is_active("edit").await);
    }

    #[tokio::test]
    async fn test_active_hits_do_not_consume_activation_capacity() {
        // `alpha` is active and outscores everything (exact name match), but
        // the inactive `alpha_helper` must still receive activation capacity.
        let catalog = ToolCatalog::from_entries(
            vec![
                entry("alpha", "alpha capability for testing"),
                entry("alpha_helper", "alpha helper capability for testing"),
            ],
            &deferred_routing(),
        );
        catalog.activate(&["alpha".to_string()]).await;
        let result = catalog.search("alpha", 1, None).await;
        let names: Vec<&str> = result.hits.iter().map(|h| h.name.as_str()).collect();
        assert!(
            names.contains(&"alpha_helper"),
            "inactive match starved by active hit: {names:?}"
        );
        // Activation-relevant matches lead; transparency follows.
        assert_eq!(
            result.hits.first().map(|h| h.name.as_str()),
            Some("alpha_helper"),
            "hits: {names:?}"
        );
        // Reporting stays bounded independently of activation capacity.
        assert!(result.hits.len() <= 2, "hits: {names:?}");
    }

    #[tokio::test]
    async fn test_mixed_equal_scores_stay_deterministic() {
        let catalog = ToolCatalog::from_entries(
            vec![
                entry("m_tool_b", "shared helper tool alpha"),
                entry("m_tool_a", "shared helper tool alpha"),
                entry("m_tool_c", "shared helper tool alpha"),
            ],
            &deferred_routing(),
        );
        catalog.activate(&["m_tool_b".to_string()]).await;
        let first = catalog.search("helper", 10, None).await;
        let second = catalog.search("helper", 10, None).await;
        let a: Vec<&str> = first.hits.iter().map(|h| h.name.as_str()).collect();
        let b: Vec<&str> = second.hits.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(a, b);
        // Name tie-break applies within each set; inactive leads.
        assert_eq!(a, vec!["m_tool_a", "m_tool_c", "m_tool_b"]);
    }

    #[tokio::test]
    async fn test_already_active_reporting_is_bounded_independently() {
        let mut entries = vec![
            entry("wanted_one", "wanted recovery helper"),
            entry("wanted_two", "wanted recovery helper"),
        ];
        for i in 0..6 {
            entries.push(entry(
                &format!("wanted_active_{i}"),
                "wanted recovery helper",
            ));
        }
        let catalog = ToolCatalog::from_entries(entries, &deferred_routing());
        let actives: Vec<String> = (0..6).map(|i| format!("wanted_active_{i}")).collect();
        catalog.activate(&actives).await;
        let result = catalog.search("wanted recovery", 2, None).await;
        let names: Vec<&str> = result.hits.iter().map(|h| h.name.as_str()).collect();
        // Inactive capacity is intact (both inactive matches surface) while
        // already-active reporting is capped at the same independent bound.
        assert_eq!(
            names,
            vec![
                "wanted_one",
                "wanted_two",
                "wanted_active_0",
                "wanted_active_1"
            ],
            "hits: {names:?}"
        );
    }

    #[tokio::test]
    async fn test_remaining_deferred_excludes_managed_search() {
        // 6 core active + 3 real deferred; the managed entry is not work.
        let catalog = ToolCatalog::from_entries(core_entries(), &deferred_routing());
        assert_eq!(catalog.deferred_count().await, 3);
        let searched = catalog.search("edit source code patch", 5, None).await;
        assert_eq!(searched.remaining_deferred, 3);
        // Eager mode: everything real is visible, so nothing is deferred.
        let eager = ToolCatalog::from_entries(core_entries(), &eager_routing());
        assert_eq!(eager.deferred_count().await, 0);
        // Deferred routing with no real deferred tools: discovery alone is
        // not deferred work, so it starts retired.
        let core_only: Vec<ToolCatalogEntry> = CORE_EAGER_TOOLS
            .iter()
            .map(|name| entry(name, "core eager helper"))
            .collect();
        let retired = ToolCatalog::from_entries(core_only, &deferred_routing());
        assert_eq!(retired.deferred_count().await, 0);
        assert!(!retired.is_active(TOOL_SEARCH_TOOL_NAME).await);
    }

    #[tokio::test]
    async fn test_final_activation_retires_tool_search() {
        let catalog = ToolCatalog::from_entries(core_entries(), &deferred_routing());
        assert!(catalog.is_active(TOOL_SEARCH_TOOL_NAME).await);
        catalog
            .activate(&[
                "edit".to_string(),
                "apply_patch".to_string(),
                "fs_write".to_string(),
            ])
            .await;
        assert_eq!(catalog.deferred_count().await, 0);
        assert!(!catalog.is_active(TOOL_SEARCH_TOOL_NAME).await);
        let defs = catalog.active_tool_defs().await;
        assert!(
            defs.iter()
                .all(|d| d.function.name != TOOL_SEARCH_TOOL_NAME),
            "retired discovery must leave the schema surface"
        );
        // Deterministic name order is preserved after retirement.
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    #[tokio::test]
    async fn test_reactivating_tool_search_after_exhaustion_stays_retired() {
        let catalog = ToolCatalog::from_entries(
            vec![entry("alpha", "alpha capability for testing")],
            &deferred_routing(),
        );
        assert!(catalog.is_active(TOOL_SEARCH_TOOL_NAME).await);
        let first = catalog.activate(&["alpha".to_string()]).await;
        assert_eq!(first, vec!["alpha".to_string()]);
        assert!(!catalog.is_active(TOOL_SEARCH_TOOL_NAME).await);
        // Retirement is sticky: a stale explicit re-activation must report
        // nothing newly activated and leave discovery inactive.
        let stale = catalog.activate(&[TOOL_SEARCH_TOOL_NAME.to_string()]).await;
        assert!(stale.is_empty(), "newly: {stale:?}");
        assert!(!catalog.is_active(TOOL_SEARCH_TOOL_NAME).await);
    }

    #[tokio::test]
    async fn test_from_entries_drops_custom_tool_search_entry() {
        // `from_parts` skips a builtin named `tool_search` in favor of the
        // managed discovery definition; the fixture constructor must match so
        // counts and served schemas never drift between test and production.
        let catalog = ToolCatalog::from_entries(
            vec![
                entry("alpha", "alpha capability for testing"),
                entry(TOOL_SEARCH_TOOL_NAME, "custom impostor discovery"),
            ],
            &deferred_routing(),
        );
        assert_eq!(catalog.all_tool_count(), 2);
        let managed = crate::tools::tool_search::tool_def();
        let served = catalog
            .all_tool_defs()
            .into_iter()
            .find(|d| d.function.name == TOOL_SEARCH_TOOL_NAME)
            .expect("managed tool_search present");
        assert_eq!(served.function.description, managed.function.description);
        // The impostor never counts as deferred work.
        assert_eq!(catalog.deferred_count().await, 1);
    }

    #[tokio::test]
    async fn test_concurrent_search_and_activate_stay_consistent() {
        use std::sync::Arc;
        let catalog = Arc::new(ToolCatalog::from_entries(
            vec![
                entry("alpha", "alpha capability for testing"),
                entry("alpha_helper", "alpha helper capability for testing"),
            ],
            &deferred_routing(),
        ));
        // Ranking must not hold the active-set lock across scoring: a
        // concurrent activation must complete while searches are in flight,
        // and every search still returns a coherent snapshot.
        let searcher = {
            let catalog = Arc::clone(&catalog);
            tokio::spawn(async move {
                let mut seen = Vec::new();
                for _ in 0..20 {
                    let result = catalog.search("alpha", 1, None).await;
                    assert!(!result.hits.is_empty());
                    seen.push(result.hits.first().map(|h| h.name.clone()).expect("hit"));
                }
                seen
            })
        };
        catalog.activate(&["alpha".to_string()]).await;
        let seen = searcher.await.expect("search task");
        assert!(seen.iter().all(|n| n == "alpha" || n == "alpha_helper"));
        assert!(catalog.is_active("alpha").await);
    }

    #[tokio::test]
    async fn test_history_reactivation_does_not_starve_discovery() {
        let catalog = ToolCatalog::from_entries(
            vec![
                entry("alpha", "alpha capability for testing"),
                entry("alpha_helper", "alpha helper capability for testing"),
            ],
            &deferred_routing(),
        );
        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: vec![crate::llm::types::ToolCall {
                id: Some("1".into()),
                r#type: "function".into(),
                function: crate::llm::types::ToolCallFunction {
                    name: "alpha".into(),
                    arguments: "{}".into(),
                },
            }],
            tool_call_id: None,
        }];
        catalog.activate_known_from_history(&messages).await;
        assert!(catalog.is_active("alpha").await);
        // The reactivated high scorer cannot crowd out remaining discovery.
        let result = catalog.search("alpha", 1, None).await;
        assert_eq!(
            result.hits.first().map(|h| h.name.as_str()),
            Some("alpha_helper")
        );
    }

    #[tokio::test]
    async fn test_core_eager_surface_includes_observation_read() {
        let catalog = ToolCatalog::from_entries(core_entries(), &deferred_routing());
        for core in CORE_EAGER_TOOLS {
            assert!(
                catalog.is_active(core).await,
                "{core} should stay eagerly visible"
            );
        }
        assert!(catalog.is_active("observation_read").await);
        let defs = catalog.active_tool_defs().await;
        assert!(
            defs.iter().any(|d| d.function.name == "observation_read"),
            "observation_read must stay schema-visible without discovery"
        );
    }

    #[test]
    fn test_search_hit_carries_no_schema() {
        // Shape guarantee: hits carry only name/source/server/description/
        // score. Full JSON schemas ship solely in the next LLM request's
        // `tools` array, never in search results.
        let hit = ToolSearchHit {
            name: "edit".to_string(),
            source: ToolSource::Builtin,
            server: None,
            description: "short".to_string(),
            score: 1,
        };
        assert_eq!(hit.name, "edit");
        assert_eq!(hit.source, ToolSource::Builtin);
        assert!(hit.server.is_none());
        // Debug rendering must not embed a schema blob either.
        let rendered = format!("{hit:?}");
        assert!(!rendered.contains("parameters"));
    }
}
