# Orchestrator M5: Schedules, Heartbeats and Workspace Scripts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Codex など superpowers が無い環境では、各タスクを上から順に実行し、完了ごとに `docs/orchestrator/PROGRESS.md` を更新してコミットする。

**Goal:** cron 式で新しいエージェントを定期起動する（スケジュール）、既存エージェントへ定期的に追送する（heartbeat）、リポジトリの `dgc.json` に書いたコマンドを内容単位の信頼確認のうえデーモンが監督実行する（ワークスペーススクリプト）。

**Architecture:**
- **スケジュールと heartbeat:** 1 つのエンティティ `Schedule` にまとめ、対象（新エージェント／既存エージェント）で区別する。
  - デーモン内の `ScheduleManager` が 30 秒以内の間隔で時刻を見て発火する。
  - cron の評価は依存を足さず、`libc` の `localtime_r`/`mktime` を使ってデーモンのローカル時刻で行う。
- **スクリプト:** `ScriptManager` がワークスペースの `dgc.json` を 1 回だけ読み、信頼済みの内容と一致する場合に限り、そのとき読んだ内容からプロセスグループで起動する。出力は 0600 のログファイルへ書く。
- **インターフェース:** CLI は `dgc schedule`、`dgc heartbeat`、`dgc script` を追加する。TUI は変更しない。

**Tech Stack:** 既存の crates のみ（新しい依存なし。`libc` は既存）

**Spec:** `docs/superpowers/specs/2026-11-25-orchestrator-design.md` §2 ゴール、§6、§9、§10（M4 行の後半。M4 計画時の分割で M5 に移った）。

ユーザー決定（2026-11-26、PROGRESS 決定事項）:
- ワークスペーススクリプトはリポジトリの `dgc.json` に置く。初回と内容変更時に、内容単位で信頼を確認する。
- デーモン停止中に過ぎたスケジュールは実行しない（記録のみ）。

機能の形は paseo を参考にする（schedule: cron、prompt、provider、name、max runs、expires。heartbeat: 既存エージェントへの定期追送で、作成・一覧・削除だけ）。

## Global Constraints

### プロトコルと状態ファイル

- `PROTOCOL_VERSION` を 3 に上げる（新しいメソッドとフィールドを古いデーモンが黙って無視しないようにするため。M4 と同じ理由）。
- ID は `ScheduleId`（接頭辞 `sch_`）。状態ファイルは次の 3 つで、すべて既存の `Store` の規則に従う（0600、atomic replace、シンボリックリンクは辿らない、`version` つき）。
  - `<state>/schedules/<id>.json`
  - `<state>/script-trust/<project_id>.json`
  - `<state>/scripts/<workspace_id>/<name>.log`
- 状態ファイルに保存する本文の上限:
  - スケジュールの prompt: 64 KiB
  - スケジュール数: 256
  - 実行履歴: 直近 20 件
- 時刻はすべて Unix 秒（`i64`）で保存する。表示はデーモンのローカル時刻で `YYYY-MM-DD HH:MM ±hhmm` にし、`dgc_daemon::schedule::localtime::format_local` を使う。

### cron 式

- 5 フィールド（分 時 日 月 曜日）。
- 使える書き方:
  - `*`、数値、範囲 `a-b`、リスト `a,b`、ステップ `*/n` `a-b/n`
  - 月名 `jan`–`dec`、曜日名 `sun`–`sat`（大文字小文字を区別しない）。曜日の `7` は日曜。
  - マクロ `@hourly` `@daily` `@weekly` `@monthly` `@yearly` `@annually`
- 日と曜日の両方が `*` 以外なら、どちらかに一致すれば発火する（Vixie cron と同じ）。
- 不正な式、または 5 年以内に発火しない式（例: `0 0 31 2 *`）は作成時に `INVALID_PARAMS` にする。
- タイムゾーンはデーモンのローカル時刻だけ（`TZ` を変えたらデーモンを再起動する）。
- DST の扱い:
  - 存在しない時刻（夏時間開始で飛ぶ時間帯）には発火しない。
  - 繰り返す時刻（夏時間終了）には 1 回だけ発火する（`mktime` の `tm_isdst=-1` が選ぶ側）。

### 発火の規則

- `next_run_at` は「今の分より後の最初の一致」。
- ScheduleManager の確認間隔は 30 秒以内。期限を 120 秒より多く過ぎて気付いた場合は実行せず、`missed` を 1 件記録して次回を再計算する。対象はデーモン停止中、サスペンド、時計の変更。
- 起動時も同じ処理をする。過ぎた回数は数えない（1 件だけ記録）。

### 対象ごとの扱い

**新エージェント（schedule）**
- 前回の実行で作ったエージェントが `starting`/`running`/`waiting_permission` のときは、`skipped`（`previous_run_active`）を記録して起動しない。
- 作るエージェントの設定:
  - `title`: `<name>: <prompt 1 行目>`。name が無ければ prompt の 1 行目。
  - `labels`: `{"schedule": <id>}`
  - `stop_when_idle = true`: ターンが終わって待ち行列が空になったら、プロバイダのプロセスを終えて `stopped` にする。`dgc resume` で再開できる。

**既存エージェント（heartbeat）**
- 対象が `idle` のときだけ `queue` で送る。
- `running`/`waiting_permission`/`starting` のときは `skipped`（`agent_busy`）。
- `stopped`/`failed` のときは `skipped`（`agent_not_running`）。
- `archived` のとき、または存在しないときは、スケジュールを `ended`（`agent_archived`）にする。

**共通**
- ワークスペースが archived か存在しないときは、スケジュールを `ended`（`workspace_unavailable`）にする。
- プロバイダが使えないときは `failed`（エラー種別のみ）を記録し、スケジュールは続ける。

### 自動承認

- プロファイルとモードは作成時に解決し、具体的な `provider`/`model`/`mode`/`auto_approve` を保存する（後で `config.toml` のプロファイルを変えても既存スケジュールは変わらない）。
- 自動承認系モードは明示した場合だけ使い、CLI で `warning:` 行、一覧で `auto-approve` と表示する。
- dgc は承認を代行しない。無人の実行で出た権限要求もユーザーを待つ。

### `max_runs` と `expires_at`

- `max_runs` は実際に起動・送信した回数だけを数える。skipped/missed/failed は数えない。
- 上限に達するか `expires_at` を過ぎたら `ended`。
- 手動実行（`schedule.run`）も回数に数える。`next_run_at` は変えない。前回の実行が動作中なら `INVALID_STATE`（`previous_run_active`）を返す。

### スクリプト

**`dgc.json` の読み込み**
- ワークスペース直下の `dgc.json` を `O_NOFOLLOW` で開く。通常ファイルで 64 KiB 以下に限る。
- 形式は `{"version":1,"scripts":{"<name>":{"command":"<sh -c の文字列>","cwd":"<相対パス、任意>"}}}`。
  - 未知のキーはエラー。
  - スクリプトは 32 個まで。
  - 名前は `[A-Za-z0-9_-]{1,32}`。
  - `command` は空でない 4 KiB 以下の文字列。
  - `cwd` は相対パスで、`..` と絶対パスは不可。正規化した結果がワークスペースの正規化パスの内側にない場合（シンボリックリンクで外へ出る場合を含む）は拒否する。

**信頼**
- 信頼はプロジェクト単位。同じ内容なら別の worktree でも信頼済みになる。
- ハッシュではなく、内容のバイト列そのものを比較する（暗号依存なしで「内容単位」を満たす。決定事項に記録）。
- プロジェクトごとに直近 16 件の内容を保持する。
- `script.trust` は CLI が表示した内容を `expected_content` で受け取り、現在のファイルとバイト単位で一致した場合だけ信頼する。一致しなければ `INVALID_STATE`（`script_file_changed`）。

**起動と停止**
- `script.start` はファイルを 1 回だけ読み、そのバイト列で信頼を確認し、同じバイト列から解析したコマンドを起動する。2 回目は読まない。
- 起動の仕方:
  - `/bin/sh -c <command>` を新しいプロセスグループで起動する。
  - stdin は null。stdout と stderr はログファイルに追記する。8 MiB を超えたら `.log.1` に 1 世代だけ回転する。
  - 環境はデーモンの環境に `DGC_WORKSPACE_ID`、`DGC_WORKSPACE_PATH`、`DGC_SCRIPT_NAME` を足したもの。
- 停止はプロセスグループに SIGTERM を送り、5 秒後に SIGKILL を送る。デーモンの停止と `workspace.archive` は、そのワークスペースのスクリプトを先に止める。
- 実行状態はメモリだけに持つ（再起動後は `not_running`）。デーモンがクラッシュした場合、スクリプトはプロバイダと同様に残りうる（README に記載）。

### 共通

- tracing に prompt、スクリプトの command、スクリプトの出力を出さない。出すのは ID、スクリプト名、状態、エラー種別だけ。AGENTS.md のセキュリティ不変条件の「本文の保存場所」に、スケジュールの状態ファイルとスクリプトのログを足す。
- 人向けの出力はすべてエスケープする（`output::escape_text`/`escape_line`）。対象は prompt、command、スクリプトの出力。
- 新しい依存なし。既存のセキュリティ不変条件と TUI の権限安全規則を弱めない。TUI は変更しない（スケジュールで作ったエージェントは既存の一覧に `labels` 付きで出る）。

## Review Focus

1. **作成後の設定変更で、スケジュールが黙って自動承認になる**
   - 期待: プロファイルを後から自動承認系に書き換えても、既存スケジュールのモードは変わらない。
   - テスト: Task 2 の `schedule_snapshots_profile_at_creation`。
2. **信頼の確認と実行の間に `dgc.json` が変わる**
   - 期待: 表示した内容と違う内容は信頼しない。起動は確認に使ったバイト列からだけ行う。
   - テスト: Task 4 の `trust_requires_expected_content` と `start_runs_the_bytes_it_checked`。
3. **停止・サスペンドで期限を過ぎていた**
   - 期待: 実行せず、`missed` を 1 件だけ記録する。
   - テスト: Task 2 の `overdue_runs_are_recorded_missed_not_run`。
4. **前回の実行が権限待ちのまま次の時刻が来る**
   - 期待: エージェントを積み増さない。
   - テスト: Task 2 の `schedule_skips_while_previous_run_active`。
5. **`dgc.json` やその `cwd` がシンボリックリンクや `..` でワークスペースの外を指す**
   - 期待: 拒否する。
   - テスト: Task 4 の `script_file_symlink_is_refused` と `script_cwd_cannot_escape_workspace`。

---

### Task 1: cron とローカル時刻

**Files:** Create `crates/dgc-daemon/src/schedule/{mod.rs,cron.rs,localtime.rs}`（`mod.rs` はこのタスクでは子モジュールの公開だけ）、`crates/dgc-daemon/src/lib.rs`。テストは `cron.rs` と `localtime.rs` の単体テスト。

**Interfaces（Produces）:**
- `pub struct CronExpr`。`CronExpr::parse(s: &str) -> Result<CronExpr, CronError>`。`CronError` は `thiserror` で、どの位置のどのフィールドかを示す（式の全文は含めない）。
- `pub struct LocalDateTime { year: i32, month: u32, day: u32, hour: u32, minute: u32, offset_secs: i32 }`
- `pub trait LocalClock`:
  - `fn to_local(&self, unix: i64) -> LocalDateTime`
  - `fn from_local(&self, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> Option<i64>`。存在しない時刻は `None`。繰り返す時刻は 1 つに決める。
- `pub struct SystemClock`: `libc::localtime_r`/`mktime`（`tm_isdst = -1`）で実装する。`from_local` は変換結果を `to_local` に戻し、壁時計の時・分が一致しなければ `None` を返す。
- `CronExpr::next_after(&self, unix: i64, clock: &dyn LocalClock) -> Option<i64>`:
  - `unix` の分より後の最初の発火時刻（秒は 0）を返す。5 年以内に無ければ `None`。
  - 方式: ローカル日付を 1 日ずつ進め、月・日・曜日が合う日について、合う (時, 分) を昇順に `from_local` で Unix 秒に変換し、`unix` より後の最初のものを返す。分単位の全探索はしない。
- `localtime::format_local(unix: i64) -> String`（`2026-11-28 09:00 +0900`）。

- [ ] **Step 1: テスト**
  - `parse_accepts_fields_ranges_steps_lists_names_and_macros`
  - `parse_rejects_out_of_range_and_bad_syntax`: `60 * * * *`、`* * 0 * *`、`*/0 * * * *`、`* * * 13 *`、6 フィールド。
  - `next_after_every_minute_and_hourly`
  - `dom_and_dow_restricted_matches_either`
  - `dow_seven_is_sunday`
  - `never_firing_expression_returns_none`: `0 0 31 2 *`。
  - `nonexistent_local_time_is_skipped_and_repeated_time_fires_once`: DST を模した `LocalClock` のテスト実装を使う。
  - `fixed_offset_clock_round_trips`
  - `system_clock_round_trips_current_time`: 実時刻で `to_local` → `from_local` が一致する。
- [ ] **Step 2: 失敗を確認** — `bash scripts/verify.sh test schedule::`。期待: コンパイルエラーか FAIL。
- [ ] **Step 3: 実装**
- [ ] **Step 4: 通過を確認** — `bash scripts/verify.sh test schedule::`。期待: PASS（0 件ではないこと）。
- [ ] **Step 5: コミット** `feat(daemon): parse cron expressions in local time`

### Task 2: スケジュールと heartbeat（デーモン）

**Files:**
- `crates/dgc-protocol/src/{ids.rs,model.rs,methods.rs,lib.rs}`（`ScheduleId`、`PROTOCOL_VERSION = 3`）
- `crates/dgc-daemon/src/{store.rs,schedule/mod.rs,agent.rs,agent/actor.rs,handlers.rs,server.rs}`
- テスト: `crates/dgc-daemon/tests/schedule.rs`（新規）、`crates/dgc-daemon/tests/store.rs`、`crates/dgc-daemon/tests/server.rs`
- `AGENTS.md`（本文の保存場所）、`docs/orchestrator/PROGRESS.md`

**Interfaces（Produces）:**
- モデル（`dgc-protocol::model`）:
  - `Schedule { id, name: Option<String>, target: ScheduleTarget, prompt: String, cron: String, state: ScheduleState, end_reason: Option<String>, max_runs: Option<u32>, runs: u32, expires_at: Option<i64>, next_run_at: Option<i64>, last_agent_id: Option<AgentId>, recent_runs: Vec<ScheduleRun>, created_at: String }`
  - `ScheduleTarget`（`#[serde(tag = "kind", rename_all = "snake_case")]`）:
    - `NewAgent { workspace_id, provider, model: Option<String>, mode: Option<String>, auto_approve: bool }`
    - `Agent { agent_id }`
  - `ScheduleState { Active, Paused, Ended }`
  - `ScheduleRun { scheduled_for: i64, at: i64, outcome: RunOutcome }`
  - `RunOutcome`（tag `kind`）: `Started { agent_id }`、`Sent { agent_id, outcome: SendOutcome }`、`Skipped { reason }`、`Missed`、`Failed { error_kind }`
- `Agent` に `#[serde(default)] stop_when_idle: bool` を追加する。`AgentManager::create_with(params, CreateOptions { stop_when_idle })` を足し、既存の `create` はこれを `false` で呼ぶ。アクターは idle になり待ち行列も保留中の権限も無いとき、`stop_when_idle` なら `finish(Stopped)` する。`resume` 後も同じ扱い。
- RPC（`methods.rs`）:

  | メソッド | パラメータ | 戻り値 |
  |---|---|---|
  | `schedule.create` | `ScheduleCreateParams { name, target: ScheduleTargetParams, prompt, cron, max_runs, expires_at }` | `Schedule` |
  | `schedule.list` | `{ kind: Option<"new_agent"\|"agent"> }` | — |
  | `schedule.get` | `{ id }` | — |
  | `schedule.pause` | `{ id }` | — |
  | `schedule.resume` | `{ id }` | — |
  | `schedule.run` | `{ id }` | `ScheduleRun` |
  | `schedule.remove` | `{ id }` | — |

  - `ScheduleTargetParams::NewAgent` は `{ workspace_id, provider: Option, profile: Option, model, mode }` で、`AgentCreateParams` と同じ規則でプロファイルを展開する。
  - 不正な値は `INVALID_PARAMS`、状態の衝突は `INVALID_STATE`。
  - `resume` は `next_run_at` を「今」から再計算し、`ended` は再開できない。
- `ScheduleManager`:
  - `ScheduleManager::new(store, agents, workspaces, clock: Arc<dyn LocalClock>)`
  - `pub async fn tick(&self, now: i64)`: テストは直接呼ぶ。
  - `pub fn spawn_loop(self: Arc<Self>, shutdown)`: 30 秒以内の間隔で `tick(実時刻)` を呼ぶ。
  - 起動時に保存済みのスケジュールを読み込む（壊れたファイルは既存の `list` と同様に飛ばす）。

- [ ] **Step 1: テスト**（`tests/schedule.rs`。偽プロバイダと手動の時刻を使う）
  - `create_validates_cron_target_and_limits`: 不正な cron、5 年以内に発火しない式、archived のワークスペース、prompt が 64 KiB 超、257 個目 → `INVALID_PARAMS`。
  - `schedule_snapshots_profile_at_creation`
  - `due_schedule_starts_agent_with_label_and_title`
  - `scheduled_agent_stops_when_idle_and_can_resume`
  - `schedule_skips_while_previous_run_active`: 前回が権限待ち → skipped。
  - `overdue_runs_are_recorded_missed_not_run`: 期限 +180 秒で tick → missed 1 件、エージェント 0。
  - `missed_after_restart_records_once`: 状態を作り直して時刻を 3 日進める → missed 1 件。
  - `max_runs_and_expiry_end_the_schedule`
  - `manual_run_counts_and_keeps_next_run`
  - `pause_resume_recomputes_next_run`
  - `heartbeat_sends_only_when_idle`: idle → Sent。running → skipped `agent_busy`。stopped → skipped `agent_not_running`。
  - `heartbeat_ends_when_agent_archived`
  - `schedule_ends_when_workspace_archived`
  - `recent_runs_capped_at_20`
  - `schedule_files_are_private`: 0600、シンボリックリンク先には書かない（`store.rs`）。
  - `old_protocol_hello_is_refused`: v2 の hello が `VERSION_MISMATCH`（`server.rs`）。
  - `schedule_rpcs_round_trip`（`server.rs`）
- [ ] **Step 2: 失敗を確認** — `cargo test --locked -p dgc-daemon --test schedule`。期待: コンパイルエラー。
- [ ] **Step 3: 実装** — `server.rs` の `run_daemon` で `recover_on_startup` の後に ScheduleManager を作り、ループを開始する。停止時はループを先に止める。AGENTS.md と PROGRESS 決定事項（プロトコル v3、`stop_when_idle`、DST、プロファイルの固定）を更新する。
- [ ] **Step 4: 通過を確認** — `cargo test --locked -p dgc-daemon --test schedule --test store --test server --test agent_manager`。期待: PASS。`python3 -I scripts/check-sensitive-logging.py` も PASS。
- [ ] **Step 5: コミット** `feat(daemon): run schedules and heartbeats`

### Task 3: スケジュールと heartbeat の CLI

**Files:** `crates/dgc-cli/src/commands/{schedule.rs（新規）,mod.rs}`、`crates/dgc-cli/src/main.rs`、`crates/dgc-cli/src/output.rs`、`crates/dgc-cli/tests/schedule_cli.rs`（新規）、`README.md`。

**Interfaces（Consumes）:** Task 2 の RPC と型。`format_local`。

**Interfaces（Produces）:**
- `dgc schedule create --cron EXPR --workspace WS (--provider P | --profile N) [--model M] [--mode MODE] [--name NAME] [--max-runs N] [--expires-in DUR] PROMPT`
  - `DUR` は `<整数>m|h|d`。CLI が `expires_at` を計算する。
  - PROMPT の受け取り方は `dgc run` と同じ。
  - 自動承認系モードでは `warning:` 行を出す（`dgc run` と同じ文言の形）。
- `dgc schedule ls [--json]`: id、name、cron、状態、次回（ローカル時刻）、回数/上限、対象。自動承認なら `auto-approve`。
- `dgc schedule show ID [--json]`: 設定と直近の実行（時刻、結果、エージェント id）。
- `dgc schedule pause|resume|run|rm ID`
- `dgc heartbeat create AGENT --cron EXPR [--name NAME] [--max-runs N] [--expires-in DUR] PROMPT`
- `dgc heartbeat ls [--json]`、`dgc heartbeat show ID`、`dgc heartbeat rm ID`
- README に「スケジュールと heartbeat」節を追加する。内容:
  - cron の書き方とローカル時刻、DST の扱い
  - missed と skipped の意味
  - スケジュールで作ったエージェントは idle で停止すること
  - 自動承認と権限待ち
  - プロファイルは作成時に固定されること
  - prompt の保存場所

- [ ] **Step 1: テスト**
  - `schedule_create_ls_show_rm_round_trip`
  - `schedule_create_warns_on_auto_approve`
  - `expires_in_parses_minutes_hours_days_and_rejects_others`
  - `heartbeat_create_ls_rm_round_trip`
  - `schedule_output_escapes_prompt_and_name`: 制御文字と bidi 文字。
  - `schedule_json_output_is_machine_readable`
- [ ] **Step 2: 失敗を確認** — `cargo test --locked -p dgc-cli --test schedule_cli`（先に `cargo build --locked -p dgc-daemon --bin dgc-fake-provider`）。期待: FAIL。
- [ ] **Step 3: 実装**
- [ ] **Step 4: 通過を確認** — 同じコマンド。期待: PASS。`bash scripts/verify.sh guidance` も PASS。
- [ ] **Step 5: コミット** `feat(cli): manage schedules and heartbeats`

### Task 4: ワークスペーススクリプト（デーモン）

**Files:**
- `crates/dgc-protocol/src/methods.rs`
- `crates/dgc-daemon/src/{script/mod.rs,script/file.rs,script/trust.rs（新規）,process.rs,workspace.rs,handlers.rs,server.rs,lib.rs}`
- テスト: `crates/dgc-daemon/tests/script.rs`（新規）、`crates/dgc-daemon/tests/server.rs`
- `AGENTS.md`、`docs/orchestrator/PROGRESS.md`

**Interfaces（Produces）:**
- `script::file::ScriptFile::read(workspace_root: &Path) -> Result<ScriptFile, ScriptFileError>`
  - `ScriptFile { bytes: Vec<u8>, scripts: BTreeMap<String, ScriptDef { command, cwd: Option<PathBuf> } > }`
  - `ScriptFileError` は `missing` / `not_regular` / `too_large` / `invalid { reason }` に区別する。reason に command の本文は入れない。
- `ScriptManager::new(store_dir, workspaces)`、`start`、`stop`、`list`、`logs`、`stop_workspace(id)`、`shutdown_all(grace)`
- `process.rs`: `ChildSpec` に環境の追加と出力先（パイプ／ファイル）を足す。プロセスグループ、`terminate(grace)` は既存の仕組みを使う。
- RPC:

  | メソッド | パラメータ | 戻り値 |
  |---|---|---|
  | `script.list` | `{ workspace_id }` | `{ file: "ok"\|"missing"\|"invalid", error: Option<String>, trust: "trusted"\|"untrusted"\|"changed", content: Option<String>, scripts: [ScriptStatus { name, command, cwd, state: "not_running"\|"running"\|"exited", pid, exit_code, signal, started_at, ended_at }] }` |
  | `script.trust` | `{ workspace_id, expected_content }` | — |
  | `script.start` | `{ workspace_id, name }` | — |
  | `script.stop` | `{ workspace_id, name }` | — |
  | `script.logs` | `{ workspace_id, name, offset: Option<u64>, max_bytes: Option<u32> (既定・上限 64 KiB) }` | `{ data: String (UTF-8 lossy), next_offset: u64, running: bool }` |

  - `script.list` の `trust`: 一度も信頼していなければ `untrusted`、以前に信頼した内容と違えば `changed`。
  - `script.start` のエラー: 未信頼は `INVALID_STATE`（`script_untrusted`）、起動中は `INVALID_STATE`（`script_running`）、未知の名前は `INVALID_PARAMS`。
  - `script.logs` は `offset` を省くと末尾 `max_bytes` を返す。
  - archived のワークスペースは `INVALID_STATE`。
- `workspace.archive` は safety check の前に `stop_workspace` を呼ぶ。デーモン停止時は `shutdown_all(5 秒)`。

- [ ] **Step 1: テスト**（`tests/script.rs`。`tempdir` の git リポジトリと `sh` の短いコマンドを使う）
  - `script_file_parses_and_rejects_unknown_keys_bad_names_and_limits`
  - `script_file_symlink_is_refused`
  - `script_cwd_cannot_escape_workspace`: `..`、絶対パス、外を指すシンボリックリンク。
  - `untrusted_and_changed_files_cannot_start`
  - `trust_requires_expected_content`
  - `trust_is_per_project_content_and_shared_by_worktrees`
  - `trust_keeps_last_16_contents`
  - `start_runs_the_bytes_it_checked`: 確認後にファイルを書き換えても、確認した内容が実行される。テスト用の差し込み口で確認と起動の間に書き換える。
  - `script_output_goes_to_private_log_and_logs_pages_by_offset`
  - `script_env_has_workspace_vars`
  - `stop_kills_the_process_group`: 孫プロセスも終わる。
  - `exit_code_and_signal_are_reported`
  - `log_rotates_once_past_limit`: 上限はテスト用に小さくできる引数で。
  - `archive_stops_running_scripts`
  - `shutdown_stops_scripts`
  - `script_rpcs_round_trip`（`server.rs`）
- [ ] **Step 2: 失敗を確認** — `cargo test --locked -p dgc-daemon --test script`。期待: コンパイルエラー。
- [ ] **Step 3: 実装** — AGENTS.md（スクリプトのログの保存場所と、tracing に command と出力を出さないこと）と PROGRESS 決定事項（内容のバイト比較での信頼、メモリだけの実行状態、クラッシュ時に残りうること）を更新する。
- [ ] **Step 4: 通過を確認** — `cargo test --locked -p dgc-daemon --test script --test server --test workspace --test process`。期待: PASS。sensitive-logging も PASS。
- [ ] **Step 5: コミット** `feat(daemon): run trusted workspace scripts from dgc.json`

### Task 5: スクリプトの CLI

**Files:** `crates/dgc-cli/src/commands/{script.rs（新規）,mod.rs}`、`crates/dgc-cli/src/main.rs`、`crates/dgc-cli/src/output.rs`、`crates/dgc-cli/tests/script_cli.rs`（新規）、`README.md`。

**Interfaces（Consumes）:** Task 4 の RPC。

**Interfaces（Produces）:**
- `dgc script ls --workspace WS [--json]`: 信頼の状態、名前、cwd、command、実行状態。
- `dgc script trust --workspace WS [--yes]`
  - `script.list` の `content` から全スクリプト（名前、cwd、command）をエスケープして表示し、`Trust these N scripts for <project>? [y/N]` を聞く。
  - 端末でない stdin では `--yes` が必須。
  - 表示した `content` をそのまま `expected_content` で送る。`script_file_changed` の場合は「内容が変わったので再確認して」と表示して非 0 で終わる。
- `dgc script start|stop NAME --workspace WS`
- `dgc script logs NAME --workspace WS [--follow]`
  - `--follow` は 500 ms 間隔で `offset` 付きの `script.logs` を呼ぶ。止まったら（`running=false` で残りが無い）終わる。
  - 出力は `escape_line` で表示する（`\n` と `\t` 以外の制御文字と bidi 文字をエスケープ）。
- README に「ワークスペーススクリプト」節を追加する。内容:
  - `dgc.json` の形式と制限
  - 信頼の単位と再確認の条件
  - 実行環境と env
  - ログの場所
  - 停止とクラッシュ時の注意
  - dgc は外部コードを取得しない（実行するのはユーザーのリポジトリにあり、ユーザーが信頼した内容だけ）

- [ ] **Step 1: テスト**
  - `script_trust_shows_commands_and_requires_yes_without_tty`
  - `script_trust_refuses_when_file_changed`
  - `script_start_untrusted_fails_with_hint`
  - `script_logs_escapes_control_and_bidi`
  - `script_logs_follow_ends_when_script_exits`
  - `script_ls_json`
- [ ] **Step 2: 失敗を確認** — `cargo test --locked -p dgc-cli --test script_cli`。期待: FAIL。
- [ ] **Step 3: 実装**
- [ ] **Step 4: 通過を確認** — 同じコマンド。期待: PASS。guidance も PASS。
- [ ] **Step 5: コミット** `feat(cli): trust, run and follow workspace scripts`

### Task 6: M5 受け入れ

- [ ] **Step 1: 偽プロバイダで確認する** — 作業用の `DGC_STATE_ROOT` と一時的な git リポジトリを使い、次を確かめる。
  - `* * * * *` のスケジュールが実際に 1 回発火し、エージェントが idle で停止する。
  - 権限待ちの間は次の回が skipped になる。
  - デーモンを止めて 2 分以上待ち、再起動すると missed になる。
  - heartbeat が idle のエージェントに届く。
  - `dgc.json` の trust → start → logs --follow → stop。ファイルを変えると `changed` になり、起動を拒否する。

  出力を PROGRESS の検証記録に書く。
- [ ] **Step 2: 実プロバイダで 1 回だけ確認する** — 最も安いモデル、自動承認なし、作業用リポジトリで、`dgc schedule run` によって Claude のエージェントが 1 ターン完了して停止することを確かめ、記録する。
- [ ] **Step 3: 全検証を通す** — 固定ツールチェーン 1.99.0 で fmt、`verify.sh rust`、guidance、msrv、tui-deps。README と PROGRESS も更新する。
- [ ] **Step 4: コミット** `docs(orchestrator): complete M5 acceptance`。PROGRESS の M5 を完了にし、「現在の作業」を M6（エージェント向け MCP）の計画作成にする。
