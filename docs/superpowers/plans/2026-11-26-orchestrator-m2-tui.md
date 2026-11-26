# Orchestrator M2: TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Codex など superpowers が無い環境では、各タスクを上から順に実行し、完了ごとに `docs/orchestrator/PROGRESS.md` を更新してコミットする。

**Goal:** `dgc-next tui`（引数なしの `dgc-next` も同じ）で、プロジェクト／ワークスペース／エージェントのツリー、選択エージェントのストリーム、追送、権限ダイアログ、エージェントとワークスペースの作成を 1 画面で扱えるようにする。

**Architecture:** TUI は `dgc-cli` の `src/tui/` に置く。状態（`AppState`）と入力処理（`update`）は純粋関数で I/O を持たず、副作用は `Effect` 値として返す。描画（`render`）は `AppState` だけを見る。ランタイムがキー入力スレッド、デーモン購読、RPC 実行を `AppEvent` に変換して `update` に流す。デーモン側の変更は M1 保留指摘のうち TUI に直結する 2 件だけ。

**Tech Stack:** ratatui 0.30.2（feature `crossterm_0_29`）、crossterm 0.29、tokio、既存の dgc-client / dgc-protocol

**Spec:** `docs/superpowers/specs/2026-11-25-orchestrator-design.md`（§10 M2）。M1 の決定と保留指摘は `docs/orchestrator/PROGRESS.md`。

## 画面構成（ユーザー決定 2026-11-26: サイドバー + ストリーム）

```
┌ dgc ──────────────┬ agt_…3f claude  running ─────────┐
│ ▾ doge-code        │ [12] user: fix the failing test │
│  ▾ ws fix-x (wt)   │ [13] tool: Bash cargo test      │
│    ● claude  run   │ [14] assistant: The test fails  │
│    ! codex   perm  │      because …                  │
│  ▸ ws local        │ ┌ Permission (codex) ────────┐  │
│ ▸ other-repo       │ │ Command: rm -rf target     │  │
│                    │ │ [a]llow [s]ession [d]eny   │  │
│                    │ └────────────────────────────┘  │
│                    ├─────────────────────────────────┤
│ n:new  w:workspace │ > queue│steer│interrupt: _      │
└────────────────────┴─────────────────────────────────┘
 daemon ok · 3 agents · 1 waiting permission
```

検討した案: (1) サイドバー + ストリーム（採用: 複数エージェントを見ながら操作できる、Paseo に近い）、(2) 一覧 → 詳細の 2 画面（実装は簡単だが同時に見られない）。

## Global Constraints

- 新しい依存は `ratatui = { version = "0.30.2", default-features = false, features = ["crossterm_0_29"] }` と `crossterm = "0.29"`（feature なし）だけ。バージョンはルートパッケージと同一にし `[workspace.dependencies]` に追加する。futures / tokio-stream / ratatui-textarea は使わない（キー入力は専用スレッドで `crossterm::event::read` → `mpsc`）。
- 依存変更を含むので完了時に `bash scripts/verify.sh tui-deps` と `msrv` も必須。
- 表示するテキスト（agent 出力、要約、エラー、タイトル）は必ず `output::escape_text` / `escape_line` を通す（制御文字・bidi 対策。M1 最終レビューの決定）。
- 自動承認系 mode を選ぶとき、作成ダイアログに赤字の警告を出す（CLI と同じ趣旨）。
- 終了・パニック時は必ず端末を復元する（raw mode 解除、代替画面終了、カーソル表示）。TUI を閉じてもエージェントは動き続ける。
- tracing を TUI 実行中に標準エラーへ出さない（画面が壊れる）。デーモンはこれまで通り daemon.log。
- テストは ratatui `TestBackend` と純粋関数で行い、実端末・実プロバイダを使わない。デーモンを使う結合テストは M1 の `TestEnv` と偽プロバイダを使い、デーモンを残さない。
- Rust 2024、production code に unwrap/expect なし、typed errors。

## Review Focus

1. 選択中エージェントの履歴が長い（数千イベント）→ 表示用バッファは上限（直近 2000 行）で、古いものは捨てる。UI が固まらない（Task 3 にテスト）。
2. デーモンが落ちる／再起動する → ステータスバーに `daemon disconnected` を出し、一定間隔で再接続し、再接続後に一覧と選択中ストリームを読み直す（Task 4 にテスト）。
3. 購読が `lagged` で終わる → 再購読し、一覧と選択中エージェントのイベントを `since_seq` から読み直す。重複表示しない（Task 4 にテスト）。
4. 端末が狭い（例 60x15）→ サイドバー幅を縮める／省略表示し、パニックしない（Task 3 にテスト）。
5. 他のクライアント（CLI）で作られたエージェントや権限要求 → ツリーと権限マークに反映される（Task 4 にテスト）。

---

## ファイル構成

```
crates/dgc-cli/src/tui/
  mod.rs        pub fn run(paths, exe) -> Result<(), TuiError>（ランタイム）
  state.rs      AppState, Tree, Selection, StreamBuffer, Dialog, Focus
  update.rs     update(&mut AppState, AppEvent) -> Vec<Effect>
  render.rs     render(&AppState, &mut Frame)
  input.rs      キー入力スレッド、1 行エディタ（LineEditor）
  backend.rs    デーモン I/O: RPC ワーカー、購読、再接続、Effect の実行
crates/dgc-cli/tests/tui_*.rs   結合テスト
```

---

### Task 1: デーモン側の前提修正（M1 保留指摘 2 件）

**Files:** Modify `crates/dgc-daemon/src/agent.rs`（`Shared::logs`）、`crates/dgc-daemon/src/agent/actor.rs`。Test: `crates/dgc-daemon/tests/agent_manager.rs`。

**Interfaces（Produces）:**
- `EventLog` ハンドルのキャッシュは「生きているエージェント」と直近に読んだ最大 64 件の非 live エージェントに限る（LRU）。archive 時と上限超過時に閉じる。`events_page` は非 live でも従来どおり読める。
- エージェントが新しいターンを始めるたび（初期プロンプト送信、Idle からの送信、キュー先頭の送信、interrupt_and_send、resume 後のキュー送信）に `EventKind::TurnStarted` を、そのターンの `UserMessage` の直後に 1 回記録する。

- [ ] **Step 1: テスト** — `event_log_handles_are_bounded`（100 エージェントを作成→archive→events_page で読んだ後、保持ハンドル数 ≤ 64 + live 数。数え方は `AgentManager` に `#[doc(hidden)] pub fn open_log_handles(&self) -> usize` を足す）、`turn_started_follows_each_user_message`（初期プロンプト、Idle 送信、キュー送信で各 1 回、順序 UserMessage → TurnStarted）。
- [ ] **Step 2: 失敗確認** — `cargo test --locked -p dgc-daemon --test agent_manager`
- [ ] **Step 3: 実装**、**Step 4: 通過確認**
- [ ] **Step 5: コミット** `fix(daemon): bound event log handles and record turn starts`（PROGRESS.md の保留一覧の該当 2 行に「M2 Task 1 で解消」と追記）

### Task 2: TUI の状態と更新（純粋部分）

**Files:** Create `crates/dgc-cli/src/tui/{mod.rs,state.rs,update.rs,input.rs}`（mod.rs はこの時点では型と `pub mod` のみ）。Test: 各ファイルの `#[cfg(test)]`。

**Interfaces（Produces）:**

```rust
pub enum AppEvent {
    Key(KeyInput),                       // crossterm に依存しない自前のキー表現（テスト容易化）
    Snapshot { projects: Vec<Project>, workspaces: Vec<Workspace>, agents: Vec<Agent>, permissions: Vec<PendingPermission> },
    AgentEvent(AgentEvent),              // 購読から
    History { agent_id: AgentId, events: Vec<AgentEvent>, done: bool },   // agent.events のページ
    RpcDone { request: RequestTag, result: Result<serde_json::Value, String> },
    Connection(ConnectionState),         // Connected / Disconnected { reason } / Reconnecting
    Lagged,                              // 購読が lagged で終了した
    Resize { width: u16, height: u16 },
    Tick,
}
pub enum Effect {
    LoadSnapshot,
    LoadHistory { agent_id: AgentId, since_seq: u64 },
    Call { request: RequestTag, method: &'static str, params: serde_json::Value },
    Resubscribe,
    Quit,
}
pub fn update(state: &mut AppState, event: AppEvent) -> Vec<Effect>;
```

- `AppState` はツリー（project → workspace(active) → agent(非 archived)）、選択位置、フォーカス（Tree / Input / Dialog）、選択中エージェントの `StreamBuffer`（行のリング、上限 2000、最後に表示した seq）、送信モード（Queue/Steer/Interrupt、Tab で循環）、入力欄（`LineEditor`: 挿入、削除、←→、Home/End、Ctrl-U）、ダイアログ、ステータス（接続状態、エージェント数、権限待ち数、直近エラー 1 件）を持つ。
- キー割り当て（README に記載）: ↑↓/j k 移動、→/Enter 展開・選択、← 折りたたみ、`i` か Enter（エージェント選択時）で入力欄へ、Esc で戻る、Tab 送信モード切替、入力欄 Enter で `agent.send`、`x` interrupt、`r` resume、`A` archive（確認ダイアログ）、`n` 新規エージェント（選択中ワークスペースで）、`w` 新規ワークスペース（選択中プロジェクトで）、`p` 次の権限待ちエージェントへ移動、`q` 終了（確認なし。エージェントは動き続ける）。
- 権限ダイアログ: 選択中エージェントに保留中の権限があれば自動で開く。`a` allow / `s` session / `d` deny → `permission.respond`。Esc で閉じても権限は保留のまま（ツリーの `!` マークは残る）。
- 新規エージェントダイアログ: provider（claude/codex を Tab で切替）、model（任意）、mode（任意。自動承認系なら警告フラグ）、title（任意）、prompt（必須）。Enter で `agent.create`。
- 新規ワークスペースダイアログ: local / worktree 切替、worktree 時は branch（必須）、base（任意）。
- AgentEvent の反映: `StateChanged` でツリーの状態更新、未知のエージェントなら `LoadSnapshot`、`PermissionRequested/Resolved` で権限数と `!` マーク、選択中エージェントのイベントは `seq > last_seq` のものだけ StreamBuffer に追加（重複防止）。
- 選択が別エージェントに変わったら StreamBuffer を空にして `LoadHistory { since_seq: 0 }`。`History` は seq 重複を除いて追加し、`done == false` なら続きの `LoadHistory` を返す。
- `Lagged` → `Resubscribe` と `LoadSnapshot` と、選択中エージェントの `LoadHistory { since_seq: last_seq }`。
- `Connection(Disconnected)` → ステータス更新のみ。`Connected` → `LoadSnapshot` と選択中の `LoadHistory`。

- [ ] **Step 1: テスト**（update.rs）— `selecting_agent_loads_history_from_zero`、`history_pages_until_done_without_duplicates`、`live_events_after_history_are_deduplicated_by_seq`、`stream_buffer_keeps_last_2000_lines`、`enter_in_input_sends_with_current_mode`（Effect::Call agent.send, behavior 一致、入力欄が空になる）、`tab_cycles_send_mode`、`permission_dialog_opens_for_selected_waiting_agent_and_responds`、`unknown_agent_event_triggers_snapshot`、`lagged_resubscribes_and_reloads`、`new_agent_dialog_requires_prompt_and_warns_on_auto_approve_mode`、`new_worktree_workspace_requires_branch`、`archive_requires_confirmation`、`q_quits`、`line_editor_edits`。
- [ ] **Step 2: 失敗確認** — `cargo test --locked -p dgc-cli --bin dgc-next tui::`
- [ ] **Step 3: 実装**（tui は `dgc-cli` バイナリ内のモジュール。`AgentEvent` の 1 行化は既存 `output::event_line` を使い、結果を `escape_text` 済みとして扱う）
- [ ] **Step 4: 通過確認**、**Step 5: コミット** `feat(cli): add TUI state and update logic`

### Task 3: 描画

**Files:** Create `crates/dgc-cli/src/tui/render.rs`。Modify `Cargo.toml`（workspace deps: ratatui, crossterm）、`crates/dgc-cli/Cargo.toml`。Test: render.rs の `#[cfg(test)]`（`ratatui::backend::TestBackend`）。

**Interfaces（Produces）:** `pub fn render(state: &AppState, frame: &mut ratatui::Frame)`。レイアウト: 左ツリー（幅 = 端末幅の 30%、最小 18・最大 40、幅 < 50 ならツリーを 16 桁に縮め名前を省略）、右上ストリーム（タイトル行に agent 短縮 id・provider・状態）、右下入力欄（送信モード表示）、最下行ステータスバー、ダイアログは右ペイン中央のオーバーレイ。状態記号: `●` running/starting、`○` idle、`!` waiting_permission、`■` stopped、`✗` failed。自動承認エージェントは名前の後に `[auto]`。

- [ ] **Step 1: テスト** — `renders_tree_stream_input_and_status`（120x30 の TestBackend バッファにプロジェクト名、`! codex`、ストリーム行、`queue`、`waiting permission` が含まれる）、`renders_permission_dialog`、`renders_auto_approve_warning_in_create_dialog`、`narrow_terminal_does_not_panic`（60x15 と 20x5）、`control_characters_are_escaped_in_stream`、`long_history_renders_tail`（2000 行で最後の行が見える）。
- [ ] **Step 2: 失敗確認** — `cargo test --locked -p dgc-cli --bin dgc-next tui::render`
- [ ] **Step 3: 実装**、**Step 4: 通過確認**（`bash scripts/verify.sh tui-deps` も実行）
- [ ] **Step 5: コミット** `feat(cli): render the TUI layout`

### Task 4: ランタイム（入力スレッド、デーモン I/O、再接続）と起動コマンド

**Files:** Create `crates/dgc-cli/src/tui/backend.rs`、Modify `crates/dgc-cli/src/tui/{mod.rs,input.rs}`、`crates/dgc-cli/src/main.rs`（`tui` サブコマンド、引数なしで TUI）。Test: `crates/dgc-cli/tests/tui_backend.rs`。

**Interfaces（Produces）:**
- `backend::Backend::start(paths, exe) -> (mpsc::Sender<Effect>, mpsc::Receiver<AppEvent>)`: RPC 用の接続 1 本（Effect を順に実行し `RpcDone` / `Snapshot` / `History` を返す）と購読用の接続 1 本（全エージェント購読、`AgentEvent` と `Lagged` を返す）。接続は `connect_or_spawn`。切断を検知したら `Connection(Disconnected)` を送り、1 秒間隔で再接続し、成功で `Connection(Connected)`。
- `tui::run(paths, exe) -> Result<(), TuiError>`: 端末初期化（raw mode、代替画面、マウスなし）、パニックフックと Drop ガードで必ず復元、入力スレッド（`crossterm::event::read` → `KeyInput` 変換 → チャネル）、Tick 250 ms、`update` → `Effect` を backend へ、`render` を毎イベント後に。
- TUI 実行中は tracing subscriber を設定しない（または daemon.log 以外に出さない）。

- [ ] **Step 1: テスト**（`TestEnv` + 偽プロバイダのシナリオを使い、`Backend` を直接駆動。端末は使わない）— `backend_snapshot_and_live_events`（エージェント作成→ `AgentEvent` が届く）、`backend_reports_disconnect_and_reconnects`（daemon stop → Disconnected、daemon start → Connected → Snapshot 再取得）、`backend_resubscribes_after_lagged`、`backend_history_pages_until_done`、`agents_created_by_cli_appear_in_snapshot`。加えて `KeyInput` 変換の単体テスト。
- [ ] **Step 2: 失敗確認** — `cargo test --locked -p dgc-cli --test tui_backend`
- [ ] **Step 3: 実装**、**Step 4: 通過確認**
- [ ] **Step 5: コミット** `feat(cli): run the TUI against the daemon`

### Task 5: M2 受け入れ（手動確認、記録、ドキュメント）

**Files:** Modify `README.md`（TUI 節: 起動、画面、キー割り当て、権限ダイアログ、注意点）、`docs/orchestrator/PROGRESS.md`。Create `docs/orchestrator/m2-tui-screen.txt`（下記の画面記録）。

- [ ] **Step 1:** 偽プロバイダ（`dgc-fake-provider` + 既存シナリオのラッパー、`DGC_STATE_ROOT` はスクラッチ）で `dgc-next tui` を実端末相当の環境（`script -q` か tmux の capture-pane）で起動し、ツリー表示・ストリーム・送信・権限ダイアログでの allow・新規エージェント作成・新規ワークスペース作成・終了と再起動後の再表示を操作する。画面を `tmux capture-pane -p`（無ければ `script` の出力を整形）でテキスト記録し `m2-tui-screen.txt` に保存（AGENTS.md の「TUI 変更はスクリーンショットか端末録画」を満たす）。個人情報・パスはスクラッチのものだけ。
- [ ] **Step 2:** 実 Claude（haiku、1〜2 ターン、自動承認モードなし、使い捨てリポジトリ）で TUI から作成→権限 allow→完了まで 1 回確認し、結果を PROGRESS の検証記録に書く（本文は書かない）。
- [ ] **Step 3:** `cargo fmt --all`、clippy（新 crate）、`bash scripts/verify.sh guidance`、`tui-deps`、`msrv`、ワークスペース全体のテスト（固定ツールチェーン 1.99.0、`TMPDIR=/var/tmp` 直列）。結果を記録。
- [ ] **Step 4: コミット** `docs(orchestrator): complete M2 acceptance`、PROGRESS の M2 を完了に。
