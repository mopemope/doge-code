# Orchestrator M1: Daemon Core and CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Codex など superpowers が無い環境では、各タスクを上から順に実行し、完了ごとに `docs/orchestrator/PROGRESS.md` を更新してコミットする。

**Goal:** Unix ソケットで待ち受けるデーモンが Claude Code / Codex のエージェントを起動・監督し、`dgc-next` CLI からプロジェクト、ワークスペース、エージェント、権限要求を操作できるようにする。

**Architecture:** `dgc-protocol`（型のみ）→ `dgc-daemon`（Store、ワークスペース、AgentManager、プロバイダアダプタ、ソケットサーバー）→ `dgc-client`（接続とデーモン自動起動）→ `dgc-cli`（`dgc-next` バイナリ）。プロバイダ差分は `ProviderAdapter` トレイトの実装に閉じ込め、AgentManager は正規化イベントだけを見る。

**Tech Stack:** Rust 2024, tokio（Unix ソケット、子プロセス）、serde/serde_json、clap、toml、uuid v7、libc

**Spec:** `docs/superpowers/specs/2026-11-25-orchestrator-design.md`

## Global Constraints

- ソケット: 改行区切り JSON-RPC 2.0、1 メッセージ上限 `1 MiB`（1_048_576 bytes）、超過で接続切断。接続直後 `hello` 必須、`protocol_version` メジャー不一致は拒否。
- ソケットディレクトリ `0700`、ソケット `0600`、peer UID がデーモンの UID と異なる接続は即切断。ネットワークソケットは一切開かない。
- 信頼境界は UID。クライアント役割やトークンは作らない（ユーザー決定）。
- 状態ファイルは 1 エンティティ 1 JSON、temp 書き込み → rename、`0600`、シンボリックリンクへは書かない、`version` 不一致は読み込み拒否。
- events.jsonl の 1 ファイル上限 初期値 `32 MiB`（設定可能）、超過でローテーション。
- 子プロセスはプロセスグループで起動し、停止時はグループごと終了。環境変数はデーモン起動時の環境をそのまま継承。クライアントの環境変数をソケットで送らない。
- tracing に prompt 本文・エージェント出力本文を出さない（ID と状態のみ）。
- 権限要求はクライアント不在でも期限なしで保留。デーモン再起動で保留は失われ、エージェントは `stopped`。
- 自動承認系 mode（Claude: `bypassPermissions`/`auto`/`acceptEdits`、Codex: `full-access`）は `--mode` 明示時のみ。応答・一覧に `auto_approve: true` を出す。
- worktree 削除は `owned == true` かつ `$XDG_DATA_HOME/dgc/worktrees/` 配下のパスに限る。
- 動作確認済み CLI: Claude Code `2.1.332`、codex-cli `0.172.0`。範囲外は警告（起動は止めない）。
- テストは `tempdir()` と明示パス（`DaemonPaths::under(root)`）を使い、実ユーザーの XDG ディレクトリに触れない。実プロバイダはテストで起動しない。
- 依存は設計書 §5.1 の表のもののみ。追加時は PROGRESS.md「決定事項」に理由を記録。

## Review Focus

1. プロバイダ CLI が無い・未ログイン → エージェントは `failed` になり、stderr 末尾（上限 4 KiB）を `error` イベントに含める。ハングしない（Task 11/12 にテスト）。
2. ターン途中でプロバイダプロセスが落ちる → `failed`、保留中の権限要求は `permission_resolved{decision: cancelled}` で閉じ、queue は保持して `agent.resume` 後に送る（Task 8 にテスト）。
3. 遅い / 切断したクライアント → 他エージェントの処理を止めない。購読は上限付きチャネルで、溢れたら `lagged{next_seq}` 通知を送り、クライアントは `agent.events` で取り直す（Task 5/13 にテスト）。
4. CLI 2 つが同時にデーモンを自動起動 → ロックで 1 つだけ起動し、両方接続できる（Task 6 にテスト）。
5. プロバイダの 1 行が巨大（Claude のツール結果など）→ 子プロセス出力の 1 行上限 `16 MiB`、超過行は捨てて `error` イベント、エージェントは継続（Task 4 にテスト）。

---

## ファイル構成

```
crates/dgc-protocol/src/
  lib.rs        PROTOCOL_VERSION, MAX_MESSAGE_BYTES, re-export
  ids.rs        ProjectId / WorkspaceId / AgentId / PermissionId（"prj_"/"ws_"/"agt_"/"perm_" + uuid v7 simple）
  model.rs      Project, Workspace, Agent, 各 enum
  rpc.rs        Request / Response / Notification / RpcError / エラーコード
  methods.rs    メソッド名定数と各 Params / Result 構造体
  event.rs      AgentEvent, EventKind
crates/dgc-daemon/src/
  lib.rs        run_daemon(paths, config) -> Result<()>
  paths.rs      DaemonPaths
  config.rs     DaemonConfig（config.toml）
  lock.rs       InstanceLock
  store.rs      Store（JSON エンティティ永続化）
  events_log.rs EventLog（jsonl 追記・ローテーション・ページ読み）
  process.rs    ChildProcess（プロセスグループ、行読み取り、終了）※ src/execution/lifecycle.rs から移植
  workspace.rs  WorkspaceManager ※ src/features/worktree_manager.rs から必要部分を移植
  provider/mod.rs     ProviderAdapter トレイトと共通型
  provider/claude.rs  ClaudeAdapter（wire 型は同ファイル内 mod wire）
  provider/codex.rs   CodexAdapter（wire 型は同ファイル内 mod wire）
  agent.rs      AgentManager（状態機械、queue、権限保留、イベント配信、永続化）
  server.rs     ソケット listener、peer 確認、フレーミング、dispatch
  bin/dgc-fake-provider.rs  テスト用シナリオ再生バイナリ
crates/dgc-daemon/tests/
  fixtures/claude/*.jsonl, fixtures/codex/*.jsonl, fixtures/README.md
  *.rs          結合テスト
crates/dgc-client/src/lib.rs   Client
crates/dgc-cli/src/{main.rs,commands/*.rs,output.rs}
```

---

### Task 1: プロトコル型

**Files:** Create `crates/dgc-protocol/src/{ids,model,rpc,methods,event}.rs`、Modify `lib.rs`。Test: 各ファイル内 `#[cfg(test)]`。

**Interfaces（Produces）:**
- `pub const PROTOCOL_VERSION: u32 = 1; pub const MAX_MESSAGE_BYTES: usize = 1_048_576;`
- ID: `pub struct AgentId(String)` など。`AgentId::generate() -> Self`（uuid v7。protocol crate に uuid 依存を足す理由を PROGRESS.md に記録）、`as_str()`、`FromStr`（接頭辞検証）、serde は文字列透過。
- `enum ProviderKind { Claude, Codex }`（serde `"claude"|"codex"`）。
- `enum Isolation { Local, Worktree }`、`enum WorkspaceState { Active, Archived }`。
- `enum AgentState { Starting, Idle, Running, WaitingPermission, Stopped, Failed, Archived }`（serde snake_case）。
- `enum SendBehavior { Queue, Steer, Interrupt }`（既定 Queue）。`enum SendOutcome { Sent, Queued, Steered, InterruptedAndSent }`。
- `enum PermissionKind { Command, FileChange, Tool, Other }`、`enum Decision { Allow, AllowForSession, Deny }`、解決理由 `enum Resolution { Decided(Decision), Cancelled }`。
- `struct Project { id, path: PathBuf, name: String, created_at: String }`（時刻はすべて RFC3339 UTC 文字列。chrono は使わず、生成は daemon 側で `SystemTime` から整形）。
- `struct Workspace { id, project_id, isolation, path, branch: Option<String>, base_ref: Option<String>, state, owned: bool, created_at }`
- `struct Agent { id, workspace_id, title, provider, model: Option<String>, mode: Option<String>, auto_approve: bool, provider_session_id: Option<String>, state, parent_id: Option<AgentId>, labels: BTreeMap<String,String>, created_at, last_error: Option<String> }`
- `struct PendingPermission { id: PermissionId, agent_id, kind, summary: String, raw: serde_json::Value, created_at }`（`raw` は直列化後 64 KiB 上限、超過は `{"truncated":true}`）
- `struct AgentEvent { agent_id, seq: u64, at: String, kind: EventKind }`、`enum EventKind`（`#[serde(tag = "type", rename_all = "snake_case")]`）: `TurnStarted`, `AssistantText { text: String, final_: bool }`（serde 名 `final`）, `ToolCall { name, summary }`, `ToolResult { summary, is_error: bool }`, `PermissionRequested { permission: PendingPermission }`, `PermissionResolved { id, resolution }`, `TurnCompleted { usage: Option<Value> }`, `RateLimited { detail: Value }`, `Error { message }`, `StateChanged { state }`, `UserMessage { text, behavior }`。`summary` 類は 4 KiB 上限で切り詰め。
- RPC: `struct Request { jsonrpc: "2.0", id: u64, method: String, params: Value }`、`Response { id, result: Option<Value>, error: Option<RpcError> }`、`Notification { method, params }`。エラーコード: `PARSE_ERROR=-32700, INVALID_REQUEST=-32600, METHOD_NOT_FOUND=-32601, INVALID_PARAMS=-32602, INTERNAL=-32603, NOT_FOUND=-32004, INVALID_STATE=-32009, VERSION_MISMATCH=-32010`。
- メソッド名定数（`methods::HELLO = "hello"` など）: `hello, daemon.status, daemon.shutdown, project.add, project.list, project.remove, workspace.create, workspace.list, workspace.archive, agent.create, agent.list, agent.get, agent.send, agent.interrupt, agent.archive, agent.resume, agent.events, permission.list, permission.respond, subscribe, unsubscribe`。通知: `event`（params = AgentEvent）、`lagged`（`{agent_id, next_seq}`）。
- 主な Params: `HelloParams { protocol_version, client: String }`、`WorkspaceCreateParams { project_id, isolation, branch: Option<String>, base_ref: Option<String>, slug: Option<String> }`（worktree 時 `branch` 必須＝新規ブランチ名）、`AgentCreateParams { workspace_id, title, provider, model, mode, initial_prompt: String, labels, parent_id }`、`AgentSendParams { agent_id, text, behavior }`、`AgentEventsParams { agent_id, since_seq: u64, limit: u32 }`（limit 上限 1000）、`PermissionRespondParams { id, decision }`、`SubscribeParams { agent_id: Option<AgentId>, since_seq: Option<u64> }`、`DaemonStatus { pid, protocol_version, started_at, providers: Vec<ProviderStatus { kind, path: Option<PathBuf>, version: Option<String>, tested: bool }>, path_env: String }`。

- [ ] **Step 1: テストを書く** — `ids_reject_wrong_prefix`（`"ws_x".parse::<AgentId>()` が Err）、`event_kind_serializes_with_type_tag`（`AssistantText{text:"a",final_:true}` → `{"type":"assistant_text","text":"a","final":true}`）、`agent_state_roundtrip_snake_case`（`WaitingPermission` ↔ `"waiting_permission"`）、`permission_raw_is_truncated_over_64k`、`request_roundtrip`。
- [ ] **Step 2: 失敗を確認** — Run: `bash scripts/verify.sh test dgc_protocol` → FAIL（未定義）
- [ ] **Step 3: 型を実装**（依存は serde, serde_json, uuid のみ。時刻・ID 以外の生成ロジックは置かない）
- [ ] **Step 4: 通過を確認** — 同コマンドで PASS
- [ ] **Step 5: コミット** — `feat(protocol): define orchestrator socket protocol types`（PROGRESS.md 更新を含める。以降の全タスク同様）

### Task 2: パス、設定、ロック、Store

**Files:** Create `crates/dgc-daemon/src/{paths,config,lock,store}.rs`。Test: `crates/dgc-daemon/tests/store.rs`。

**Interfaces（Produces）:**
- `pub struct DaemonPaths { pub socket: PathBuf, pub state_dir: PathBuf, pub config_file: PathBuf, pub worktrees_dir: PathBuf }`
  - `DaemonPaths::from_env() -> Result<Self, PathsError>`: socket = `$XDG_RUNTIME_DIR/dgc/daemon.sock`、無ければ `<state>/run/daemon.sock`。state = `$XDG_STATE_HOME/dgc` 無ければ `$HOME/.local/state/dgc`。config = `$XDG_CONFIG_HOME/dgc/config.toml` 無ければ `$HOME/.config/dgc/config.toml`。worktrees = `$XDG_DATA_HOME/dgc/worktrees` 無ければ `$HOME/.local/share/dgc/worktrees`。macOS も同じ規則。
  - `DaemonPaths::under(root: &Path) -> Self`（テスト用、全部 root 配下）
  - `fn ensure_private_dir(path: &Path) -> io::Result<()>`: 作成して 0700。既存がシンボリックリンクなら Err、他ユーザー所有なら Err。
  - socket パスが 100 bytes を超えたら `PathsError::SocketPathTooLong`。
- `pub struct DaemonConfig { pub claude_path: Option<PathBuf>, pub codex_path: Option<PathBuf>, pub events_max_bytes: u64 /* 既定 32 MiB */ }`、`DaemonConfig::load(path) -> Result<Self>`（ファイル無しは既定値、未知キーはエラー `deny_unknown_fields`）。
- `pub struct InstanceLock`、`InstanceLock::acquire(state_dir) -> Result<Self, LockError::AlreadyRunning>`（`<state>/daemon.lock` に `flock(LOCK_EX|LOCK_NB)`、pid を書く）。
- `pub struct Store`、`Store::open(state_dir) -> Result<Self>`。`put_project/get_project/list_projects/remove_project`、同様に workspace / agent。保存形式 `{"version":1,"data":{...}}`、場所 `<state>/{projects,workspaces,agents}/<id>.json`。`fn write_atomic(path, bytes)`: 同ディレクトリの一時ファイル（0600, `O_NOFOLLOW|O_CREAT|O_EXCL`）→ fsync → rename。

- [ ] **Step 1: テスト** — `store_roundtrip_agent`、`store_files_are_0600`、`store_refuses_symlink_target`（対象パスに symlink を置くと Err）、`store_rejects_unknown_version`（`"version":2` で Err）、`lock_second_acquire_fails`、`private_dir_rejects_symlink`、`socket_path_too_long_is_error`、`config_unknown_key_is_error`。
- [ ] **Step 2: 失敗確認** — `bash scripts/verify.sh test store` → FAIL
- [ ] **Step 3: 実装**（`libc` で flock / getuid / fchmod。`dirs` は使わず env と HOME で解決）
- [ ] **Step 4: 通過確認**、**Step 5: コミット** `feat(daemon): add state paths, config, lock and store`

### Task 3: イベントログ

**Files:** Create `crates/dgc-daemon/src/events_log.rs`。Test: `crates/dgc-daemon/tests/events_log.rs`。

**Interfaces（Produces）:** `pub struct EventLog`、`EventLog::open(agent_dir: &Path, max_bytes: u64) -> Result<Self>`（`events.jsonl`、既存なら最後の seq を復元）、`append(&mut self, kind: EventKind) -> Result<AgentEvent>`（seq 採番、時刻付与）、`read_page(&self, since_seq: u64, limit: u32) -> Result<Vec<AgentEvent>>`、`last_seq() -> u64`。ローテーション: 現ファイルが max_bytes 超過で `events.1.jsonl` へ rename（既存 `.1` は削除、保持は 1 世代）。`read_page` は `.1` → 現ファイルの順に読む。壊れた行は読み飛ばして `tracing::warn`（本文は出さない）。

- [ ] **Step 1: テスト** — `append_assigns_monotonic_seq`、`reopen_restores_last_seq`、`rotation_keeps_one_generation`（max_bytes=1024 で 100 件追記 → `.1` と現ファイルのみ、seq は連続）、`read_page_spans_rotated_file`、`corrupt_line_is_skipped`。
- [ ] **Step 2–5:** `bash scripts/verify.sh test events_log` で FAIL→実装→PASS、コミット `feat(daemon): add per-agent event log`

### Task 4: 子プロセス管理

**Files:** Create `crates/dgc-daemon/src/process.rs`（`src/execution/lifecycle.rs` の `configure_process_group` / `terminate_process_tree_with_group` / `cleanup_process_group_after_exit` 相当を移植）。Test: `crates/dgc-daemon/tests/process.rs`。

**Interfaces（Produces）:**
- `pub struct ChildSpec { pub program: PathBuf, pub args: Vec<String>, pub cwd: PathBuf }`
- `pub struct ChildProcess`、`ChildProcess::spawn(spec) -> Result<(Self, ChildIo)>`。`ChildIo { pub stdin: ChildStdin, pub lines: mpsc::Receiver<LineEvent>, pub stderr_tail: Arc<Mutex<TailBuffer>> }`。
- `enum LineEvent { Line(String), Oversized { bytes: usize }, Eof }`。1 行上限 `16 MiB`、超過分は読み捨てて `Oversized`。
- `TailBuffer`: stderr の末尾 4 KiB を保持。
- `ChildProcess::terminate(&mut self, grace: Duration) -> Result<ExitStatus>`: SIGTERM → grace（既定 3 秒）→ SIGKILL、プロセスグループ全体。`Drop` でも kill（グループ）。
- `ChildProcess::wait(&mut self) -> Result<ExitStatus>`。

- [ ] **Step 1: テスト**（`sh -c` を使う）— `lines_are_delivered_in_order`、`oversized_line_is_reported_and_reading_continues`（17 MiB の 1 行の後に `ok` 行 → `Oversized` の後 `Line("ok")`）、`terminate_kills_grandchildren`（`sh -c 'sleep 100 & echo $!; wait'` の孫 pid が terminate 後に存在しない）、`stderr_tail_keeps_last_4k`。
- [ ] **Step 2–5:** `bash scripts/verify.sh test process` → FAIL→実装→PASS、コミット `feat(daemon): add process-group child management`

### Task 5: ソケットサーバー

**Files:** Create `crates/dgc-daemon/src/server.rs`、Modify `lib.rs`（`run_daemon`）。Test: `crates/dgc-daemon/tests/server.rs`。

**Interfaces（Produces）:**
- `pub async fn run_daemon(paths: DaemonPaths, config: DaemonConfig, shutdown: tokio::sync::watch::Receiver<bool>) -> Result<()>`（tokio-util は使わない。SIGTERM/SIGINT と `daemon.shutdown` で true を送る）: lock 取得 → ソケットディレクトリ 0700 → 既存ソケットファイルは lock 取得後に削除 → bind → `fchmod`/`chmod` 0600 → accept ループ。終了時にソケットファイルを削除。
- `trait Dispatcher: Send + Sync { async fn call(&self, conn: &ConnCtx, method: &str, params: Value) -> Result<Value, RpcError>; }`。M1 では `AgentManager` 等を束ねる `DaemonDispatcher`（Task 7/13 で拡張）。このタスクでは `hello`、`daemon.status`、`daemon.shutdown` のみ。
- 接続処理: `UnixStream::peer_cred()` の uid ≠ `libc::geteuid()` なら即 close。最初の要求が `hello` 以外 → `INVALID_REQUEST` を返して close。`protocol_version` 不一致 → `VERSION_MISMATCH` で close。1 行 > `MAX_MESSAGE_BYTES` → close。
- 通知送信: 接続ごとに上限 1024 の送信キュー。満杯になった購読は破棄し `lagged{agent_id,next_seq}` を 1 回送る（購読の実体は Task 13）。

- [ ] **Step 1: テスト** — `socket_is_0600_and_dir_0700`、`first_request_must_be_hello`、`version_mismatch_is_rejected`、`oversized_message_closes_connection`、`status_reports_pid_and_version`、`shutdown_removes_socket_file`、`no_tcp_listener_is_opened`（`/proc/self/net/tcp{,6}` に自プロセスの LISTEN が増えていないことを Linux のみで確認。`#[cfg(target_os = "linux")]`）。
- [ ] **Step 2–5:** `bash scripts/verify.sh test server` → FAIL→実装→PASS、コミット `feat(daemon): serve JSON-RPC over private unix socket`

### Task 6: クライアントと `dgc-next daemon`

**Files:** Create `crates/dgc-client/src/lib.rs`、`crates/dgc-cli/src/{main.rs,commands/daemon.rs,output.rs}`。Test: `crates/dgc-cli/tests/daemon_cli.rs`。

**Interfaces（Produces）:**
- `pub struct Client`、`Client::connect(paths: &DaemonPaths) -> Result<Self, ClientError>`（接続と hello）、`Client::connect_or_spawn(paths, exe: &Path) -> Result<Self>`（接続失敗時に `exe daemon run --detach` を起動し、最大 5 秒、50ms 間隔で再接続）、`call<P: Serialize, R: DeserializeOwned>(&mut self, method: &str, params: &P) -> Result<R, ClientError>`、`subscribe(self, params: SubscribeParams) -> Result<mpsc::Receiver<Notification>>`（futures/tokio-stream は使わない）。
- CLI: `dgc-next [--json] daemon {run [--detach] | start | stop | status}`。`run` はフォアグラウンド。`--detach` は `setsid` して標準入出力を `<state>/daemon.log` へ向けて自身を再実行。`start` = detach 起動して接続確認。`status` は `DaemonStatus` を表示（`--json` で JSON）。
- テストとデーモンの場所: 環境変数 `DGC_STATE_ROOT` が設定されていれば `DaemonPaths::under($DGC_STATE_ROOT)` を使う（テスト・開発用。README に記載）。

- [ ] **Step 1: テスト**（`assert_cmd` は追加しない。`std::process::Command` と `env!("CARGO_BIN_EXE_dgc-next")`）— `status_autostarts_daemon`、`concurrent_autostart_yields_single_daemon`（2 プロセス同時に `status` → 両方成功、pid が同一）、`stop_removes_socket`、`json_output_is_parseable`。
- [ ] **Step 2–5:** `bash scripts/verify.sh test daemon_cli` → FAIL→実装→PASS、コミット `feat(cli): add daemon lifecycle commands with autostart`

### Task 7: プロジェクトとワークスペース

**Files:** Create `crates/dgc-daemon/src/workspace.rs`、Modify `server.rs`（dispatch 追加）、Create `crates/dgc-cli/src/commands/{project,workspace}.rs`。Test: `crates/dgc-daemon/tests/workspace.rs`（tempdir に `git init` したリポジトリ）。

**Interfaces（Produces）:**
- `pub struct WorkspaceManager`、`new(store: Arc<Store>, worktrees_dir: PathBuf)`。
- `add_project(path) -> Result<Project>`: canonicalize、ディレクトリ必須、同一パス重複は既存を返す。`remove_project(id)`: active ワークスペースがあれば `INVALID_STATE`。
- `create_workspace(params: WorkspaceCreateParams) -> Result<Workspace>`:
  - `Local`: パス = プロジェクトパス、`owned=false`。
  - `Worktree`: git リポジトリ必須。`base_ref` を `git rev-parse --verify <ref>^{commit}` で解決（省略時 `HEAD`）。パス = `<worktrees_dir>/<repo_key>/<slug>`。`repo_key` = `<英数字と -_. 以外を _ に置換した basename>-<project_id の uuid 部先頭 12 文字>`（blake3 を新 crate に入れないため、旧実装のハッシュではなくプロジェクト ID を使う）。`slug` 省略時はブランチ名を同じ規則で置換したもの。`git worktree add -b <branch> <path> <resolved>`。既存ブランチ名なら `INVALID_PARAMS`。`owned=true`。
- `archive_workspace(id)`: 紐づく非 archived エージェントを AgentManager に archive させる（Task 13 で接続。ここではエージェントが無い前提でテスト）。`owned` かつ `is_managed_path(path, worktrees_dir)` のときだけ `git worktree remove --force <path>` とブランチ削除（`git branch -D`、dgc が作ったブランチのみ）。それ以外はディレクトリを残す。
- git 呼び出しは `tokio::process::Command`、タイムアウト 30 秒。
- CLI: `dgc-next project {add [PATH] | ls | rm ID}`、`dgc-next workspace {create --project ID [--worktree --branch NAME [--base REF] [--slug S]] | ls [--project ID] | archive ID}`。

- [ ] **Step 1: テスト** — `add_project_is_idempotent`、`local_workspace_is_not_owned`、`worktree_created_under_managed_dir`、`existing_branch_is_rejected`、`archive_removes_owned_worktree`、`archive_keeps_local_directory`、`archive_refuses_path_outside_managed_dir`（owned=true だがパスを書き換えた Workspace → 削除しない）、`remove_project_with_active_workspace_fails`。
- [ ] **Step 2–5:** `bash scripts/verify.sh test workspace` → FAIL→実装→PASS、コミット `feat(daemon): manage projects and worktree workspaces`

### Task 8: ProviderAdapter と AgentManager（偽アダプタで検証）

**Files:** Create `crates/dgc-daemon/src/provider/mod.rs`、`crates/dgc-daemon/src/agent.rs`。Test: `crates/dgc-daemon/tests/agent_manager.rs`（テスト内に `ScriptedAdapter` を実装）。

**Interfaces（Produces）:**

```rust
pub struct AgentSpec { pub agent_id: AgentId, pub cwd: PathBuf, pub model: Option<String>,
    pub mode: Option<String>, pub resume_session_id: Option<String> }
pub enum ProviderEvent { SessionStarted { session_id: String }, Event(EventKind),
    PermissionRequested { provider_request_id: String, kind: PermissionKind, summary: String, raw: Value },
    TurnCompleted { usage: Option<Value> }, Exited { success: bool, stderr_tail: String } }
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub trait ProviderAdapter: Send + Sync {
    fn kind(&self) -> ProviderKind;
    fn validate_mode(&self, mode: Option<&str>) -> Result<bool /* auto_approve */, String>;
    fn start(&self, spec: AgentSpec) -> BoxFut<'_, Result<Box<dyn ProviderSession>, ProviderError>>;
}
pub trait ProviderSession: Send {
    fn send_user<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, Result<(), ProviderError>>; // idle 時の新ターン
    fn steer<'a>(&'a mut self, text: &'a str) -> BoxFut<'a, Result<bool /* supported */, ProviderError>>;
    fn interrupt(&mut self) -> BoxFut<'_, Result<(), ProviderError>>;
    fn respond_permission<'a>(&'a mut self, provider_request_id: &'a str, d: Decision) -> BoxFut<'a, Result<(), ProviderError>>;
    fn events(&mut self) -> &mut mpsc::Receiver<ProviderEvent>;
    fn shutdown(&mut self) -> BoxFut<'_, Result<(), ProviderError>>;
}
```

（`async-trait` は追加しない。dyn 互換のため `BoxFut` を返す。）

- `pub struct AgentManager`、`new(store, paths, config, adapters: HashMap<ProviderKind, Arc<dyn ProviderAdapter>>)`。
- `create(params: AgentCreateParams) -> Result<Agent>`、`send(AgentSendParams) -> Result<SendOutcome>`、`interrupt(id)`、`archive(id)`、`resume(id)`、`respond_permission(PermissionRespondParams)`、`list()`、`get(id)`、`pending_permissions(agent: Option<AgentId>)`、`subscribe(Option<AgentId>) -> broadcast::Receiver<AgentEvent>`（容量 1024）、`events_page(...)`。
- 状態遷移（これ以外は `INVALID_STATE`）:
  - create: `Starting` → `start` 成功・初期プロンプト送信で `Running`。失敗で `Failed`（`last_error`）。
  - `Running` + TurnCompleted → queue が空なら `Idle`、あれば先頭を送って `Running`。
  - `Running` + PermissionRequested → `WaitingPermission`（PendingPermission 登録、イベント発行）。すべて解決で `Running`。
  - send: `Idle` → 送信 `Sent`。`Running/WaitingPermission` + Queue → `Queued`。+ Steer → `steer()` が true なら `Steered`、false なら interrupt してから送信 `InterruptedAndSent`。+ Interrupt → interrupt 後送信 `InterruptedAndSent`。`Stopped/Failed/Archived` → `INVALID_STATE`。
  - interrupt: `Running/WaitingPermission` → 保留権限を `Cancelled` で解決、`Idle`。
  - Exited（予期せぬ）→ `Failed`、保留権限を `Cancelled`、queue は保持（永続化）、`last_error` に stderr 末尾。
  - resume: `Stopped/Failed` → `resume_session_id = provider_session_id` で start、queue があれば先頭を送る、無ければ `Idle`。
  - archive: 任意（Archived 以外）→ shutdown、`Archived`。
- 状態変化は毎回 Store に保存し `StateChanged` を発行。queue は `<state>/agents/<id>/queue.json`。
- デーモン起動時の復元: Store 上の `Starting/Running/WaitingPermission/Idle` は `Stopped` に書き換える（プロセスは存在しないため）。

- [ ] **Step 1: テスト** — `create_runs_initial_prompt_then_idle`、`queue_is_drained_after_turn`、`steer_unsupported_falls_back_to_interrupt`、`permission_roundtrip_returns_to_running`、`interrupt_cancels_pending_permissions`、`crash_marks_failed_and_keeps_queue`、`resume_uses_provider_session_id`、`send_to_failed_agent_is_invalid_state`、`restart_recovery_marks_running_as_stopped`、`slow_subscriber_does_not_block_other_agents`（受信しない購読者がいても別エージェントの TurnCompleted が処理される）。
- [ ] **Step 2–5:** `bash scripts/verify.sh test agent_manager` → FAIL→実装→PASS、コミット `feat(daemon): add agent state machine over provider adapters`

### Task 9: 実プロバイダの記録（手動・トークン消費あり）

**Files:** Create `crates/dgc-daemon/tests/fixtures/{claude,codex}/*.jsonl`、`crates/dgc-daemon/tests/fixtures/README.md`。

この時点で wire 形式を実測して固定する。以下は Agent SDK / app-server スキーマからの**予想**であり、実測と違えば実測を正とし、差分を PROGRESS.md「決定事項」に記録する。

- Claude（予想）: 起動 `claude -p --input-format stream-json --output-format stream-json --verbose --permission-prompt-tool stdio [--model M] [--permission-mode MODE] [--resume SID]`。ホスト→CLI: `{"type":"control_request","request_id":"..","request":{"subtype":"initialize"}}`、ユーザー入力 `{"type":"user","message":{"role":"user","content":[{"type":"text","text":".."}]}}`、中断 `{"type":"control_request","request_id":"..","request":{"subtype":"interrupt"}}`、権限応答 `{"type":"control_response","response":{"subtype":"success","request_id":"..","response":{"behavior":"allow","updatedInput":{..}}}}` / `{"behavior":"deny","message":".."}`。CLI→ホスト: `system/init`（`session_id`）、`assistant`、`user`（tool_result）、`result`（`usage`、`is_error`）、`control_request{subtype:"can_use_tool", tool_name, input}`、レート制限は形式未確認。
- Codex（スキーマで確認済み）: `initialize{clientInfo:{name:"dgc",version}}` → 通知 `initialized` → `thread/start{cwd, model?, approvalPolicy, sandbox}` → `result.thread.id` → `turn/start{threadId, input:[{type:"text",text}]}` → `result.turn.id`。`turn/steer{threadId, expectedTurnId, input}`、`turn/interrupt{threadId, turnId}`。承認要求 `item/commandExecution/requestApproval`・`item/fileChange/requestApproval` への応答 `{decision: "accept"|"acceptForSession"|"decline"|"cancel"}`。通知 `turn/started`、`item/agentMessage/delta`、`item/started`、`item/completed`、`turn/completed`、`error`、`account/rateLimits/updated`。

- [ ] **Step 1:** 記録用の使い捨てスクリプト（scratch に置き、コミットしない）で、安価なモデル・短いプロンプトで次を記録する。各プロバイダ: (a) 単純な 1 ターン、(b) コマンド実行の権限要求→許可、(c) 同→拒否、(d) ターン中の中断、(e) resume、(f) 未ログイン/不正モデルのエラー、(g) Codex の steer。
- [ ] **Step 2:** 記録から個人情報・パス・トークン類を伏せ字にし、`{"dir":"in"|"out","msg":{..}}` の JSONL として保存。README に記録日・CLI バージョン・手順を書く。
- [ ] **Step 3:** 予想との差分を PROGRESS.md「決定事項」に記録。Claude が stdio で権限要求を出せなかった場合は設計書 §8.1 のフォールバック（`dgc-next permission-bridge`）を M1 に追加するタスクを PROGRESS.md に足す。
- [ ] **Step 4: コミット** `test(daemon): record provider wire fixtures`

### Task 10: 偽プロバイダ（シナリオ再生）

**Files:** Create `crates/dgc-daemon/src/bin/dgc-fake-provider.rs`。Test: `crates/dgc-daemon/tests/fake_provider.rs`。

**Interfaces（Produces）:** `dgc-fake-provider <scenario.jsonl>`。シナリオ行: `{"expect": <JSON 部分一致パターン>}`（stdin の次の 1 行が部分一致するまで待つ。不一致なら stderr に差分を出して exit 3）、`{"send": <JSON>}`（stdout に 1 行）、`{"capture": "name", "path": "/request_id"}`（直前の expect 行から値を取り出し、以降の send 内の `"${name}"` を置換）、`{"stderr": "text"}`、`{"exit": code}`、`{"sleep_ms": n}`。Task 9 の fixture を `dir` に従って expect/send に変換したシナリオを `tests/fixtures/*/scenarios/` に置く。

- [ ] **Step 1: テスト** — `replays_send_and_expect`、`capture_substitutes_request_id`、`mismatch_exits_3`。
- [ ] **Step 2–5:** `bash scripts/verify.sh test fake_provider` → FAIL→実装→PASS、コミット `test(daemon): add scenario-replay fake provider`

### Task 11: ClaudeAdapter

**Files:** Create `crates/dgc-daemon/src/provider/claude.rs`。Test: `crates/dgc-daemon/tests/claude_adapter.rs`（`ClaudeAdapter::with_program(fake_provider, ["<scenario>"])` で起動）。

**Interfaces（Produces）:** `ClaudeAdapter::new(program: PathBuf)`、`ClaudeAdapter::with_program(program, prefix_args: Vec<String>)`（テスト用）。`validate_mode`: 許容 `acceptEdits|auto|bypassPermissions|manual|dontAsk|plan`、auto_approve は `bypassPermissions|auto|acceptEdits` で true。`steer` は false を返す（未対応）。`can_use_tool` → `PermissionKind`（`Bash`→Command、`Edit|Write|MultiEdit|NotebookEdit`→FileChange、その他→Tool）。`Decision::AllowForSession` は Claude では `allow` として扱う（セッション単位の許可は送らない）。`result` → `TurnCompleted`、`system/init` → `SessionStarted`。未知の type は `raw` 付き debug ログのみ（本文はログに出さない）。

- [ ] **Step 1: テスト** — Task 9 の各シナリオ（a〜f）に対応: `claude_simple_turn`、`claude_permission_allow`、`claude_permission_deny`、`claude_interrupt`、`claude_resume_passes_session_id`（args に `--resume <id>` を含むことを expect で確認）、`claude_missing_binary_fails_agent`（存在しないパス → `Failed`、ハングしない、5 秒以内）、`claude_login_error_surfaces_stderr_tail`。
- [ ] **Step 2–5:** `bash scripts/verify.sh test claude_adapter` → FAIL→実装→PASS、コミット `feat(daemon): drive Claude Code over stream-json`

### Task 12: CodexAdapter

**Files:** Create `crates/dgc-daemon/src/provider/codex.rs`。Test: `crates/dgc-daemon/tests/codex_adapter.rs`。

**Interfaces（Produces）:** `CodexAdapter::new(program)`、`with_program(program, prefix_args)`。起動 `codex app-server`。mode 対応: `read-only`→`{sandbox:"read-only", approvalPolicy:"on-request"}`、`workspace-write`（既定）→`{sandbox:"workspace-write", approvalPolicy:"on-request"}`、`full-access`→`{sandbox:"danger-full-access", approvalPolicy:"never"}`（auto_approve=true）。`steer` は実行中ターンがあれば `turn/steer`（`expectedTurnId` = 現ターン id）で true。`Decision` 対応: Allow→`accept`、AllowForSession→`acceptForSession`、Deny→`decline`。対応する承認要求は `item/commandExecution/requestApproval`（Command）と `item/fileChange/requestApproval`（FileChange）。その他のサーバー要求には JSON-RPC エラー `-32601` を返し `Error` イベントを出す。`account/rateLimits/updated` → `RateLimited`。JSON-RPC の要求 id は単調増加。

- [ ] **Step 1: テスト** — `codex_simple_turn`、`codex_command_approval_accept`、`codex_file_change_decline`、`codex_interrupt_uses_turn_id`、`codex_steer_uses_expected_turn_id`、`codex_resume_calls_thread_resume`、`codex_unknown_server_request_is_rejected`、`codex_rate_limit_event`、`codex_missing_binary_fails_agent`。
- [ ] **Step 2–5:** `bash scripts/verify.sh test codex_adapter` → FAIL→実装→PASS、コミット `feat(daemon): drive Codex over app-server`

### Task 13: エージェント操作の公開（ソケットと CLI）

**Files:** Modify `crates/dgc-daemon/src/server.rs`（agent.* / permission.* / subscribe / unsubscribe）、`lib.rs`（起動時にプロバイダパス解決と `--version` 取得 → `DaemonStatus.providers`）。Create `crates/dgc-cli/src/commands/{agent,permit}.rs`。Test: `crates/dgc-cli/tests/agent_cli.rs`（`config.toml` の `claude_path`/`codex_path` を、シナリオを引数に取るラッパーシェルスクリプトに向ける）。

**Interfaces（Produces）:**
- プロバイダパス解決: config の絶対パス → 無ければ `PATH` 探索して絶対パス化。`--version` 出力から版を抽出し、既知版（`2.1.332` / `0.172.0`）と一致しなければ `tested=false` で warn。
- `subscribe{agent_id?, since_seq?}`: since_seq があれば EventLog から追いついてからライブ配信。broadcast が `Lagged` を返したら `lagged{agent_id,next_seq}` を送る。
- `workspace.archive` が紐づくエージェントを archive するよう接続。
- CLI:
  - `dgc-next run --workspace ID --provider claude|codex [--model M] [--mode MODE] [--title T] [--follow] PROMPT`（自動承認 mode のときは stderr に警告行を必ず出す）
  - `dgc-next ls [--all]`、`dgc-next show ID`
  - `dgc-next send ID [--behavior queue|steer|interrupt] TEXT`（結果の SendOutcome を表示）
  - `dgc-next interrupt ID`、`dgc-next archive ID`、`dgc-next resume ID`
  - `dgc-next logs ID [--since SEQ] [--follow]`（イベントを 1 行ずつ人間向けに表示。`--json` で AgentEvent JSONL）
  - `dgc-next permit ls [--agent ID]`、`dgc-next permit allow ID [--session]`、`dgc-next permit deny ID`

- [ ] **Step 1: テスト** — `run_follow_prints_assistant_text`、`permit_allow_unblocks_agent`、`send_queue_then_drain`、`logs_since_replays_history`、`auto_approve_mode_warns`、`archive_workspace_archives_agents`、`status_lists_provider_versions`、`lagged_subscriber_receives_lagged_notice`、`daemon_log_excludes_prompt_text`（初期プロンプトに `SECRET-PROMPT-MARKER` を含めて一連の操作後、`<state>/daemon.log` にその文字列が無い。設計書 §9-6）。
- [ ] **Step 2–5:** `bash scripts/verify.sh test agent_cli` → FAIL→実装→PASS、コミット `feat(cli): expose agent and permission commands`

### Task 14: M1 受け入れ（実 CLI 手動確認とドキュメント）

**Files:** Modify `README.md`（「dgc-next（開発中のオーケストレーター）」節: インストール、`daemon`、`project`、`workspace`、`run`、`send`、`permit`、`logs`、`DGC_STATE_ROOT`、保存場所、セキュリティ上の性質、systemd/launchd ユニット例）、`docs/orchestrator/PROGRESS.md`。

- [ ] **Step 1:** 実 Claude と実 Codex それぞれで「プロジェクト追加 → worktree ワークスペース作成 → `run` → 権限要求を `permit allow` → `send --behavior queue` → `interrupt` → `daemon stop` → `daemon start` → `resume` → `archive` → `workspace archive`」を実行し、コマンドと結果の要約を PROGRESS.md「検証記録」に書く（本文や認証情報は書かない）。
- [ ] **Step 2:** `ls -l` でソケット/状態ファイルの権限、`ss -lx`/`ss -ltn`（macOS は `lsof -U`/`lsof -iTCP -sTCP:LISTEN`）で TCP LISTEN が無いことを確認し記録。
- [ ] **Step 3:** `cargo fmt --all && bash scripts/verify.sh rust && bash scripts/verify.sh guidance`、依存を変えたので `bash scripts/verify.sh msrv`。結果を記録。
- [ ] **Step 4: コミット** `docs(orchestrator): complete M1 acceptance`、PROGRESS.md の M1 を完了に。
