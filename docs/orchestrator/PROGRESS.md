# オーケストレーター移行 進捗

このファイルが進捗の唯一の正本。Claude Code / Codex どちらで作業する場合も、
最初にここを読み「現在の作業」から再開する。

- 設計書: [2026-11-25-orchestrator-design.md](../superpowers/specs/2026-11-25-orchestrator-design.md)
- 計画: [M0](../superpowers/plans/2026-11-25-orchestrator-m0-scaffold.md) / [M1](../superpowers/plans/2026-11-25-orchestrator-m1-daemon.md)
  （M2 以降の計画は、前のマイルストーン完了時に作成する）

## 運用ルール

- タスクを 1 つ終えるごとに、コードとこのファイルを同じコミットに含める。
- 中断するとき（トークン切れが近いときを含む）は「現在の作業」を更新してコミットする。
  未完成のコードは `wip(scope): ...` コミットにしてよい。
- エージェント固有の記憶、TODO ツール、セッション状態に頼らない。必要なことはここに書く。
- 状態: `未着手` / `作業中` / `完了` / `保留`

## 現在の作業

- タスク: M1 Task 11（ClaudeAdapter）
- 状態: 未着手
- 済んだこと: M0 完了、M1 Task 1（プロトコル型）、Task 2（パス、設定、ロック、Store）、Task 3（イベントログ）、Task 4（子プロセス管理）、Task 5（ソケットサーバー）、Task 6（クライアントと `dgc-next daemon`）、Task 7（プロジェクトとワークスペース）、Task 8（ProviderAdapter と AgentManager）、Task 9（実プロバイダの記録）、Task 10（偽プロバイダ）完了（ブランチ `feat/orchestrator-m1`）
- 次の一手: M1 計画の Task 11 Step 1 から始める
- 未コミットの変更: なし

## マイルストーン

| M | 内容 | 状態 | 計画 |
|---|---|---|---|
| M0 | workspace 足場、検証/CI 対応 | 完了 | m0-scaffold |
| M1 | デーモン中核と CLI（`dgc-next`） | 未着手 | m1-daemon |
| M2 | TUI | 未着手 | 未作成 |
| M3 | 切替（旧 `src/` 削除、`dgc` に改名） | 未着手 | 未作成 |
| M4 | プロファイル、スケジュール、heartbeat、スクリプト、レート制限時の引き継ぎ提案 | 未着手 | 未作成 |
| M5 | エージェント向け MCP（サブエージェント） | 未着手 | 未作成 |

## タスク

### M0

| # | タスク | 状態 | コミット |
|---|---|---|---|
| 0 | 進捗表と AGENTS.md 入口 | 完了 | （計画と同じコミット） |
| 1 | Cargo workspace 化と新 crate の足場 | 完了 | `build(workspace): add orchestrator crate scaffold` |

### M1

| # | タスク | 状態 | コミット |
|---|---|---|---|
| 1 | プロトコル型 | 完了 | `feat(protocol): define orchestrator socket protocol types` |
| 2 | パス、設定、ロック、Store | 完了 | `feat(daemon): add state paths, config, lock and store` |
| 3 | イベントログ | 完了 | `feat(daemon): add per-agent event log` |
| 4 | 子プロセス管理 | 完了 | `feat(daemon): add process-group child management` |
| 5 | ソケットサーバー | 完了 | `feat(daemon): serve JSON-RPC over private unix socket` |
| 6 | クライアントと `dgc-next daemon` | 完了 | `feat(cli): add daemon lifecycle commands with autostart` |
| 7 | プロジェクトとワークスペース | 完了 | `feat(daemon): manage projects and worktree workspaces` |
| 8 | ProviderAdapter と AgentManager | 完了 | `feat(daemon): add agent state machine over provider adapters` |
| 9 | 実プロバイダの記録（手動・トークン消費あり） | 完了 | `test(daemon): record provider wire fixtures` |
| 10 | 偽プロバイダ | 完了 | `test(daemon): add scenario-replay fake provider` |
| 11 | ClaudeAdapter | 未着手 | |
| 12 | CodexAdapter | 未着手 | |
| 13 | エージェント操作の公開（ソケットと CLI） | 未着手 | |
| 14 | M1 受け入れ（実 CLI 手動確認とドキュメント） | 未着手 | |

## 決定事項

設計書から外れた判断、追加依存とその理由を日付付きで追記する。

- 2026-11-25: 信頼境界は UID。dgc が起動したエージェントは信頼する（ユーザー決定）。
- 2026-11-25: 権限の自動承認系 mode は M1 から `--mode` 明示で使える（設計者判断）。
- 2026-11-25: `dgc-protocol` に ID 生成のため `uuid` 依存を置く（設計書 §5.1 の表への追加）。
- 2026-11-25 (M0): `[workspace.dependencies]` には使用中のものだけ置く（現在 `clap` のみ）。
  M1 の各タスクで最初に使うときに追加する（バージョンはルートパッケージの宣言と同一にする）。
  注: 未使用依存の警告は PATH 上の Cargo 1.100 で出たもので、固定の 1.99.0 では出ない
  （最終レビューで確認）。判断の理由は「使うものだけを宣言する」こと。
- 2026-11-25 (M0): 新バイナリはルートから `cargo run -p dgc-cli --bin dgc-next` で起動する
  （ルートパッケージが default-run のため `--bin` だけでは見つからない）。
- 2026-11-25 (M0): 作業は `feat/orchestrator` ブランチで行う（別 worktree は旧 crate の
  再ビルドが重いため作らない）。

- 2026-11-25 (M1 Task 1): `[workspace.dependencies]` に `serde`（derive）、`serde_json`、`uuid`
  （`default-features = false`）を追加。`dgc-protocol` は uuid の features `std`, `v7` を有効にする
  （`Uuid::now_v7` は `std` を要する）。
- 2026-11-25 (M1 Task 1): ID の文字列形式は `prj_` / `ws_` / `agt_` / `perm_` + uuid v7 の 32 桁小文字 hex
  （ハイフンなし）。`FromStr` と serde の deserialize が接頭辞と本体を検証する。
- 2026-11-25 (M1 Task 1): 切り詰めは protocol crate の関数で提供する: `truncate_summary`（4096 バイト、
  UTF-8 境界）、`PendingPermission::new`（`raw` は直列化 64 KiB 超で `{"truncated":true}`）、
  `EventKind::tool_call` / `tool_result`。`Response` は計画どおり `jsonrpc` フィールドを持たない。
  `AgentEventsParams` の `since_seq` 既定 0、`limit` 既定 1000（`clamped_limit()` で上限適用）。
- 2026-11-25 (M1 Task 2): `[workspace.dependencies]` に `dgc-protocol`（path）、`libc`、`thiserror`、`toml`、
  `tempfile`（dev）を追加（バージョンはルートパッケージと同一）。`tracing` と `tokio` は未使用のため追加していない。
- 2026-11-25 (M1 Task 2): `DaemonPaths::from_vars(get)` を追加し `from_env` はそれを呼ぶ（環境変数に触れずにテストするため）。
  空文字・相対パスの XDG/HOME 値は未設定扱い（XDG 仕様）。ソケットパス上限は 100 バイトで `from_env`/`from_vars` のみ検査し、
  `under(root)` は検査しない。
- 2026-11-25 (M1 Task 2): `write_atomic` は対象パスまたは親ディレクトリが symlink なら Err（rename が symlink を
  黙って置換するのを防ぐ）。Store の読み取りも `O_NOFOLLOW`。`InstanceLock` は open で truncate せず、flock 取得後に pid を書く。
  Store の `list_*` は壊れたファイル・未知 version を Err にする（黙って読み飛ばさない）。
- 2026-11-25 (M1 Task 2 修正): 上記を変更。`list_*` は読めない/壊れた/未知 version のエントリを読み飛ばし、
  `tracing::warn!`（ファイル名とエラー種別のみ、内容は出さない）を出して残りを返す（1 ファイルの破損でデーモン起動時の
  復元を止めないため、コントローラ判断）。`get_*` は従来どおりその id のエラーを返す。`[workspace.dependencies]` に `tracing = "0.1"` を追加。
- 2026-11-25 (M1 Task 3): `EventLog::open(agent_dir, agent_id, max_bytes)`（event に agent_id を付けるため引数追加）。
  `dgc_daemon::time::{now_rfc3339, format_rfc3339}` を追加（chrono なし、UTC、ミリ秒、days-from-civil）。後続タスクで再利用する。
  ローテーションは「追記前に現サイズ + 新行 > max_bytes かつ現ファイルが空でない」とき `events.jsonl` → `events.1.jsonl`
  （既存 `.1` は rename で置換）。1 行が max_bytes を超えても新ファイルへ書く。ファイルは 0600、`O_NOFOLLOW`（symlink は
  `EventLogError::Symlink`）。open 時に末尾が改行でなければ改行を足す（クラッシュで切れた行が次のイベントを巻き込まないため）。
  現ファイルが空で `.1` のみ存在する場合は `.1` の最後の有効行から seq を復元する。
- 2026-11-25 (M1 Task 4): `[workspace.dependencies]` に `tokio`（`default-features = false`、バージョンはルートと同一）を追加。
  `dgc-daemon` は features `io-util`, `macros`, `process`, `rt`, `sync`, `time` のみ有効化。
  `ChildProcess` は `process_group(0)`（pre_exec 不要）で新グループのリーダーにし、`kill_on_drop(false)`、`Drop` でグループへ SIGKILL。
  `terminate`/`wait` はリーダーを回収した直後にグループの残りへ SIGKILL してから PGID を手放す（`wait` も同様。PGID 再利用による誤爆を避けるため）。
  行読み取りは 16 MiB 超を読み捨て `Oversized { bytes }`（改行なしの最終行は EOF で配信、空なら配信しない）。エラー型は `process::ProcessError`。
- 2026-11-25 (M1 Task 5): `dgc-daemon` の tokio features に `net` を追加（依存クレートの追加なし）。
  `run_daemon(paths, config, shutdown: watch::Sender<bool>)`（コントローラ裁定 R1）。`Dispatcher` トレイトは作らず具体型
  `DaemonDispatcher::call(&self, &ConnCtx, method, params)`（`hello` は接続状態を持つため接続層で処理し、`daemon.status` /
  `daemon.shutdown` をディスパッチャが処理）。`daemon.shutdown` は応答を書き終えた後に接続層が `request_shutdown()` で true を送る。
  `Response.id` は u64 のため、id 不明の PARSE_ERROR / INVALID_REQUEST 応答は id 0。hello 前の不正入力は PARSE_ERROR ではなく
  INVALID_REQUEST で close。1 行 > `MAX_MESSAGE_BYTES`（改行除く）は無応答で close。送信は接続ごとに有界 mpsc（1024）1 本の
  writer タスク。応答は `send().await`（破棄しない）、通知は `ConnCtx::notify` の `try_send`（満杯で false。購読の破棄と `lagged` 送信は Task 13）。
  終了時は accept 停止、接続を最大 2 秒で flush、ソケットファイル削除、lock 解放の順。ソケットパスが socket 以外なら
  `DaemonError::SocketPathOccupied` で起動拒否（削除しない）。`run_daemon` / `DaemonError` は `server` モジュールで定義し lib.rs から再エクスポート。

- 2026-11-25 (M1 Task 6): `[workspace.dependencies]` に `dgc-client`、`dgc-daemon`（path）と `tracing-subscriber`
  （`default-features = false`、バージョンはルートと同一。`dgc-cli` が `fmt` のみ有効化）を追加。
  逸脱: `dgc-client` は `dgc-daemon`（`DaemonPaths` と `ensure_private_dir` のため）と `libc`（`pre_exec(setsid)` と `O_NOFOLLOW`）に
  依存する。設計書 §5.1 の「tokio, dgc-protocol」だけでは、仕様の `Client::connect(&DaemonPaths)` と自動起動を実装できないため。
  `dgc-cli` は `serde` に依存しない（`serde_json` のみ）。`libc` と `tempfile` は `dgc-cli` の dev-dependency（テストの後始末用）。
- 2026-11-25 (M1 Task 6): 自動起動は `exe daemon run` を `setsid` 付きで起動する（`--detach` は子ではなく呼び出し側の
  `connect_or_spawn` の動作）。起動ログは `<state>/daemon.log`（0600、O_APPEND、O_NOFOLLOW）。接続失敗のうち
  ソケット欠如と接続拒否のときだけ起動し、50ms 間隔で最大 5 秒再接続する。`Client::call` は 1 要求ずつで、応答待ち中の通知は
  最大 256 件バッファして `subscribe` が先に配信する（超過分は古いものから捨てる）。クライアントの 1 行上限も `MAX_MESSAGE_BYTES`。
  `daemon stop` はソケット消滅に加えて instance lock が解放されるまで最大 5 秒待つ（直後の自動起動がロック競合で負けないため）。
  `--json` は `Cli` の global フラグ（`daemon status --json` でも可）。
- 2026-11-25 (M1 Task 7): 依存クレートの追加なし。`dgc-protocol` に `ProjectAddParams`、`ProjectRemoveParams`、
  `WorkspaceListParams`、`WorkspaceArchiveParams` を追加（コントローラ裁定）。結果は `project.list` → `{"projects":[..]}`、
  `workspace.list` → `{"workspaces":[..]}`（active のみ）、`workspace.archive` → archived の `Workspace`。
  `WorkspaceManager`（`workspace.rs`、`WorkspaceError` が NOT_FOUND / INVALID_PARAMS / INVALID_STATE / INTERNAL に対応）と
  パラメータ解析・エラー写像の `handlers.rs` を追加し、`DaemonDispatcher::new` が `Arc<WorkspaceManager>` を受け取る。
  `run_daemon` は lock 取得直後（bind 前）に `Store::open` する（`DaemonError::Store` を追加）。
- 2026-11-25 (M1 Task 7): `is_managed_path(path, worktrees_dir)` は「両方絶対、`.`/`..` なし、component 単位の prefix、
  かつ `<repo-key>/<slug>` の 2 階層以上下」を字句で確認し、さらに存在する最長の祖先を `canonicalize` した結果でも同じ検査を行う
  （symlink 脱出と dangling symlink は拒否）。削除は `owned` かつ `isolation == worktree` かつ managed のときだけ。
  managed 外の owned workspace は警告ログのうえディスクに触れず archived にする。worktree 削除（`git worktree remove --force`）が
  失敗したら何も archived にせず INTERNAL（stderr 末尾 4096 バイト以内）。ブランチ削除（`git branch -D`）の失敗は警告ログのみで
  archive は成功扱い。archive は冪等。（下の 2026-11-25 修正で、リポジトリ全体の `git worktree prune` とブランチの無条件削除は廃止。）
  `base_ref` には解決済みコミットのフルハッシュを保存する（設計書 §データモデルの「解決済み」）。
  `local` workspace に `branch`/`base_ref`/`slug` を渡すと INVALID_PARAMS。slug は `[A-Za-z0-9._-]` 1〜128 文字、先頭 `.` 不可
  （明示 slug も同規則、省略時は branch を同規則で置換）。ユーザー指定の ref/branch は `-` 始まりを拒否し git の option と
  解釈させない。git は `tokio::process::Command`（`kill_on_drop`、30 秒、`GIT_TERMINAL_PROMPT=0`、`GIT_DIR` 等を除去）で実行する。
  変更系操作は `tokio::sync::Mutex` で直列化する。archive 時のエージェント archive は Task 13 で接続（コード内 TODO）。
- 2026-11-25 (M1 Task 7): `crates/dgc-cli/tests/common/mod.rs` に Task 6 の `TestEnv` を移して `daemon_cli.rs` と共有（挙動変更なし）。
  README に `project` / `workspace` コマンドを追記。
- 2026-11-25 (M1 Task 7 修正 1): 計画の欠陥の訂正（コントローラ裁定）: `repo_key` の uuid 部は先頭 12 桁（uuid v7 のミリ秒
  タイムスタンプ部）ではなく**末尾 12 桁**（ランダム部）を使う。
- 2026-11-25 (M1 Task 7 修正 1): archive の後始末は dgc 自身の git 状態に限定する。リポジトリ全体の `git worktree prune` は
  使わない（ユーザーの欠けた worktree の登録まで消すため）。削除前に `git worktree list --porcelain -z` でこの workspace の
  path のエントリを探し、(a) ディレクトリが存在する、または stale エントリがあるときだけ `git worktree remove --force <path>`
  （欠けたディレクトリでも成功する）、(b) ブランチは、そのエントリが記録どおりの `refs/heads/<branch>` をチェックアウトして
  いる場合だけ `git branch -D`。エントリ無し・別ブランチ・detached HEAD なら archive は行い、ブランチは残して警告ログ（id のみ）。

- 2026-11-25 (M1 Task 8): 依存クレートの追加なし。`provider/mod.rs`（トレイトと共通型）、`agent.rs`（`AgentManager`）、
  `agent/actor.rs`（エージェントごとの actor。`agent.rs` が 700 行を超えたためコントローラ裁定どおり分割）。
  構成: 生存中のエージェント 1 つにつき tokio タスク 1 つ（actor）が `ProviderSession` と queue を持ち、manager は mpsc の
  コマンド + oneshot の応答で操作する。コマンドは actor がその時点の状態で評価するため、`starting` 中の send/interrupt/archive は
  起動完了後に処理される（待つ）。レコードは毎回 Store に保存し `get`/`list` は Store を読む。イベントは EventLog に追記してから
  全体 1 本の broadcast（容量 1024）へ送る。`subscribe(agent)` の引数は参考値で、絞り込みは受信側の責務。
  生存していないエージェントの変更と actor の最終遷移は `live` ロック下で行う（resume との競合防止）。
- 2026-11-25 (M1 Task 8): `AgentError` は `NotFound(&'static str)`（`agent`/`workspace`/`permission`）、
  `InvalidState { state: String }`（エージェント状態の snake_case、または `workspace_archived`）、`InvalidParams(String)`、
  `Provider(String)`、`Storage(String)`。`code()` で RPC コードに写像（Provider/Storage は INTERNAL）。
  `ProviderError` は `Spawn`/`Io`/`Protocol`（String）と `Exited`。公開 API の形: `create`/`resume`/`archive` は `Agent` を返す
  （create/resume 直後は `starting`）、`interrupt` は `()`、`events_page(&AgentEventsParams)`（`clamped_limit()` を適用）、
  `recover_on_startup()`（全件処理して最初のエラーを返す）、`shutdown_all(grace)`（各 session を shutdown して `stopped`、
  最大 grace + 0.5 秒待つ）。
- 2026-11-25 (M1 Task 8): アダプタの契約（Task 11/12 向け、`provider/mod.rs` の doc に記載）: `ProviderError` の文言にプロンプト・
  出力本文を含めない。`Exited` は `shutdown` 以外でプロセスが終わったときに 1 回だけ出す（イベントチャネルが `Exited` なしに
  閉じても予期せぬ終了扱い）。`interrupt` が返った後に中断したターンの `TurnCompleted` を出さない。interrupt 時に未解決の権限要求は
  manager 側で `cancelled` にするので、プロトコル上の応答が必要ならアダプタが返す。`start` はハングしない。
- 2026-11-25 (M1 Task 8): 状態機械の細部（ブリーフに無い部分の判断）: create は `state_changed{starting}` と
  `user_message{初期プロンプト, queue}` を記録してから起動する。起動失敗・初期/queue 先頭の送信失敗・`Exited` は `error`
  イベント（4096 バイトで切り詰め）を出して `failed`（`last_error` に同じ文言。`Exited` は stderr 末尾、空なら定型文）。
  起動失敗時の初期プロンプトは queue に入れない。ユーザーの send がプロバイダエラーになった場合は状態を変えずに
  `Provider` エラーを返す（後続の `Exited` で failed になる）。interrupt 後の送信が失敗したら `idle`。
  `waiting_permission` 中の `TurnCompleted` も `running` と同様に扱い、残った保留権限は cancelled にする。
  resume は workspace が active であることを確認し、`last_error` を消す。queue.json の読み込みは `O_NOFOLLOW`、未知 version は
  `Storage` エラー。

- 2026-11-25 (M1 Task 8 修正 1): プロバイダ呼び出しに上限を設ける。`agent::ProviderTimeouts { start, call }`
  （既定 `PROVIDER_START_TIMEOUT` = 60 秒、`PROVIDER_CALL_TIMEOUT` = 30 秒、`AgentManager::with_timeouts` で上書き。テストは 300ms）。
  actor は `start` と全 session 呼び出し（send_user/steer/interrupt/respond_permission）を `tokio::time::timeout` で囲み、超過は
  `ProviderError::TimedOut(call)`（文言 `provider <call> timed out`、本文なし）。呼び出し元には `Provider` エラーを返した後、
  エージェントを `failed` にして session を shutdown する（詰まった session が archive を含む以後の操作を塞がないため）。
  `shutdown` は従来どおり grace（archive/failed 時 5 秒）で打ち切る。
- 2026-11-25 (M1 Task 8 修正 1): interrupt 前に溜まったイベントの扱い。`interrupt()` が返った直後に session のイベントを
  `try_recv` で読み切り、`TurnCompleted` は状態遷移なしで記録、`PermissionRequested` は破棄（WaitingPermission にしない）、
  `Exited`（またはチャネル切断）は送信を中止してメインループで failed にする。その他は通常どおり記録。actor の `select!` は
  `biased;` でプロバイダイベントを先に処理する（先に届いた TurnCompleted を後のコマンドより先に適用するため。イベントが
  途切れなく来る間はコマンドが待たされる）。アダプタ契約（`provider/mod.rs`）に「interrupt 後に中断ターンの
  TurnCompleted/PermissionRequested を出さない」「全メソッドは有限時間で終わる」を追記。

- 2026-11-25 (M1 Task 9): 実プロバイダの wire 形式を記録した（`crates/dgc-daemon/tests/fixtures/`、詳細と全形状は同 README の
  Wire notes）。Claude 2.1.332 は stdio で権限要求を出せたので、設計書 §8.1 のフォールバック（`dgc-next permission-bridge`）は不要。
  計画の「予想」との差分（実測を正とする）:
  - Claude: `--permission-prompt-tool stdio` は `--help` に無いが**必須**。無いと承認が必要なツールはホストに問い合わせず自動拒否
    （`system/permission_denied`）。`system/init` は起動時 1 回ではなく**ターンごと**に出る。`assistant` は content ブロック
    ごとに 1 行。SessionStart フックの `system/hook_*` 行が initialize 応答より先に出うる。中断は `control_response`
    `{"still_queued":[]}` → `[Request interrupted by user]` の user 行 → `result{subtype:"error_during_execution",is_error:true}`
    で、プロセスは次のターンを受け付ける。不正モデルの `result` は **`subtype:"success"` かつ `is_error:true`**（成否は `is_error`
    で判定）、stdin を閉じた後の終了コード 1。`--resume` は同じ `session_id` を継続。レート制限は `rate_limit_event`
    （`rate_limit_info.status` 等）が API 呼び出しごとに出る。観測値は `allowed` のみ。
  - Codex: メッセージに `jsonrpc` フィールドは無い（送らなくてよい）。通知はトップレベルに `emittedAtMs` を持つ。サーバー要求
    （承認）の id は 0 始まりで dgc の要求 id と番号空間が別（`method` の有無で区別）。`on-request`+`workspace-write` では
    ワークスペース内コマンドに承認要求が出ない（コマンド承認は `untrusted` で、ファイル変更承認は `on-request`+`read-only` で記録）。
    コマンド承認の `availableDecisions` は `accept` / `acceptWithExecpolicyAmendment` / `cancel` で、`decline` と
    `acceptForSession` は提示されなかった（Task 12 の Deny→`decline`、AllowForSession→`acceptForSession` はコマンドでは未検証）。
    `item/fileChange/requestApproval` の params に差分は無く、同じ `itemId` の `item/started`（fileChange）の `changes` にある。
    使用量は `turn/completed` ではなく `thread/tokenUsage/updated`。`account/rateLimits/updated` は制限に達していなくても毎応答出る
    （到達判定は `rateLimitReachedType` 等で行う必要があるが、到達時の値は未観測）。中断・steer で途中の agentMessage には
    `item/completed` が来ない。`thread/resume` は `excludeTurns: true` 推奨（無いと `deprecationNotice`）。不正モデルは
    thread/start・turn/start とも成功し、ターン中に `error{codexErrorInfo:"modelNotFound",willRetry:false}` →
    `turn/completed{status:"failed"}`。アクティブターン無しの steer/interrupt は JSON-RPC エラー `-32600`。
  - 未観測: 両プロバイダのレート制限到達時の値、未ログイン時のエラー（ユーザーのログアウトはしない方針）、Claude の
    `--permission-mode` 指定時と initialize 省略時の挙動、Codex のコマンド承認への `decline`/`acceptForSession`。
- 2026-11-25 (M1 Task 10): `dgc-fake-provider`（`src/bin/`、同期、serde_json のみ）の形式は `crates/dgc-daemon/tests/fixtures/fake/README.md`。
  ブリーフ外の追加: `expect_args`（argv の順序付き部分列、`${ANY}` 可、先頭行のみ、不一致 exit 3）、`expect_eof`、`//` コメント行、
  環境変数 `DGC_FAKE_TIMEOUT_MS`（既定 10 s、超過 exit 4）。exit 2 は形式エラー、5 は stdio 失敗。
  アダプタ用シナリオは Task 11/12 で `tests/fixtures/` 配下に書く（ブリーフの `*/scenarios/` ではなく、本タスクの自前シナリオは `fake/`）。

## 検証記録

実行したコマンド、結果、環境で実行できなかったチェックを日付付きで追記する。

- 2026-11-25 M0 Task 1（Linux）:
  - `python3 -m unittest ... test_development_checks.py`: 新テスト 2 件が RED → 実装後 50 件 PASS。
  - `cargo test -p dgc-cli`: `dgc_next_reports_its_name_and_version` RED → PASS。
  - `cargo run --bin dgc -- --version` → `doge-code 0.1.0`、`cargo run -p dgc-cli --bin dgc-next -- --version` → `dgc-next 0.1.0`。
  - `cargo tree -p dgc-daemon` に sea-orm/reqwest/tree-sitter なし。
  - `verify.sh guidance`: PASS（192 tests）。`verify.sh msrv`（1.94.0）: PASS。`verify.sh tui-deps`: PASS。
  - `verify.sh rust`（固定ツールチェーン 1.99.0）: fmt PASS、clippy PASS、test は旧 `src/` の 3 件が失敗
    （`--no-fail-fast` で 2443 passed / 3 failed）。M0 は `src/` を変更していないので、この変更が原因ではない:
    - `utils::tests::test_is_git_repository_without_git_dir`、`test_get_git_repository_root_without_git`:
      この端末に空の `/tmp/.git` ディレクトリがあり（11-25 16:32 作成、本作業とは無関係、削除していない）、
      `env::temp_dir()` から親を辿ると git リポジトリと判定される。環境要因。
    - `logging::tests::retention_bounds_reserve_active_runs_and_preserve_unmanaged_files`: 全体並列実行時のみ失敗、
      単独では 3/3 PASS。既存の不安定テスト。
  - 環境の注意: この端末の PATH 上の `cargo` は stable 1.100 で、`rust-toolchain.toml` の 1.99.0 ではない。
    1.100 の clippy は旧 `src/execution/policy.rs:463` を警告する（CI の 1.99.0 では出ない）。
    検証は `PATH=$HOME/.rustup/toolchains/1.99.0-x86_64-unknown-linux-gnu/bin:$PATH` を付けて実行する。
  - 最終レビュー（別エージェント）: 判定「修正後マージ可」。Important 1 件（`docs/ai/workflow.md` の
    Rust ゲートのコマンド記述に `--workspace` が無い）を修正し、`check-agent-guidance.py` に
    `check_workflow_doc` を追加して再発を防止（`test_workflow_doc_lists_wrapper_rust_commands` RED→GREEN、
    `verify.sh guidance` PASS 193 tests）。Minor は下の「保留中の小さな改善」に記録。

- 2026-11-25 M1 Task 1（Linux、固定ツールチェーン 1.99.0）:
  - `cargo test --locked -p dgc-protocol`: 実装前は未定義型で RED（compile error）→ 28 件 PASS。
    `verify.sh test` はテスト名で絞るため `dgc_protocol` では単体テスト（`ids::tests::...`）に一致しない。
    crate 単位は `cargo test -p dgc-protocol` を使い、`verify.sh test <テスト名>` の動作も確認した。
  - `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings`: clean。
- 2026-11-25 M1 Task 2（Linux、固定ツールチェーン 1.99.0）:
  - `cargo test --locked -p dgc-daemon --test store`: 実装前はモジュール未定義で RED（compile error）→ 22 件 PASS。
    `verify.sh test store`: PASS（106 executed）。`cargo test --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli`: PASS。
  - `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings`: clean。
  - 修正後: `list_skips_unreadable_entries` が RED（list が Err）→ 実装後 23 件 PASS、clippy clean、`verify.sh test store` PASS。
  - 未検証: 他ユーザー所有ディレクトリの拒否（root なしでは再現できない）。
- 2026-11-25 M1 Task 3（Linux、固定ツールチェーン 1.99.0）:
  - `cargo test --locked -p dgc-daemon --test events_log`: 実装前はモジュール未定義で RED（compile error）→ 10 件 PASS。
    `cargo test --locked -p dgc-daemon --lib time`: 5 件 PASS。clippy clean。
    注: `verify.sh test events_log` はファイル名ではなくテスト名で絞るため 0 件で拒否される（`verify.sh test rotation_keeps` は PASS）。
- 2026-11-25 M1 Task 4（Linux、固定ツールチェーン 1.99.0）:
  - `cargo test --locked -p dgc-daemon --test process`: 実装前は未定義型で RED（compile error）→ 14 件 PASS（5 回連続 PASS）。
    ミューテーション確認: `finish` の残存グループへの SIGKILL を外すと `terminate_kills_descendants_that_outlive_the_leader` が FAIL。
    `verify.sh test terminate_kills_grandchildren`: PASS。clippy clean、新 crate 全テスト PASS。
    注: この端末の `/bin/sh` は bash。TERM を無視する子孫のテストは trap 設定後に pid を出力する（SIGTERM との競合回避）。
- 2026-11-25 M1 Task 5（Linux、固定ツールチェーン 1.99.0）:
  - `cargo test --locked -p dgc-daemon --test server`: 実装前は `run_daemon` / `DaemonError` 未定義で RED（compile error）→ 18 件 PASS（5 回連続 PASS）。
    `cargo test --locked -p dgc-daemon --lib`: 15 件 PASS（`server::tests` 6 件）。clippy clean、新 crate 全テスト PASS。
    `verify.sh test shutdown_removes_socket_file`: PASS。
  - 未検証: 他 UID のピア拒否（別ユーザーで接続できないため。`peer_cred` と `geteuid` の比較コードのみ）。
- 2026-11-25 M1 Task 6（Linux、固定ツールチェーン 1.99.0）:
  - `cargo test --locked -p dgc-cli --test daemon_cli`: 実装前は `daemon` サブコマンド未定義で 7 件 FAIL（RED）→ 7 件 PASS（8 回連続 PASS、
    終了後に `dgc-next` プロセス残存なし）。`cargo test --locked -p dgc-client`: 12 件 PASS（偽サーバーによる単体、実デーモンとの往復を含む）。
  - 未検証: `subscribe` の実デーモンでの動作（サーバー側 `subscribe` は Task 13。偽サーバーでの単体のみ）。
    `daemon stop` のロック待ちは、待たない版でもテストが通る（競合が速すぎて再現しない）ため、ミューテーションでは検出できない。
- 2026-11-25 M1 Task 7（Linux、固定ツールチェーン 1.99.0）:
  - `cargo test --locked -p dgc-daemon --test workspace`: 実装前は `dgc_daemon::workspace` 未定義で RED（compile error）→ 26 件 PASS。
    `--lib`（`is_managed_path`、名前規則、タイムアウト kill ほか `workspace::tests` 15 件、`handlers::tests` 4 件）PASS。
    `cargo test --locked -p dgc-cli --test workspace_cli`: 3 件 PASS。新 crate 全テストを 3 回連続 PASS、終了後に `dgc-next` 残存なし。
    clippy（`-D warnings`）clean。`verify.sh test archive_removes_owned_worktree`: PASS。
  - ミューテーション確認: `is_managed_path` を常に true にすると `archive_refuses_{path_outside_managed_dir,traversal_path,symlink_escape}`
    が FAIL、archive の `owned` 条件を外すと `archive_keeps_unowned_directory_inside_managed_dir` が FAIL。
  - 注: `repo_key` は仕様どおり uuid v7 の先頭 12 桁（ミリ秒タイムスタンプ部）なので、同名ディレクトリのプロジェクトを同一ミリ秒に
    作ると衝突しうる（実用上は無視できる）。→ 下の修正で末尾 12 桁に変更。
- 2026-11-25 M1 Task 7 修正 1（Linux、1.99.0）: `cargo test --locked -p dgc-daemon --test workspace` 29 件 PASS、
  `--lib workspace` 15 件 PASS、`-p dgc-cli --test workspace_cli` 3 件 PASS、clippy clean。ミューテーション（ブランチを無条件に
  削除）で新テスト 3 件が FAIL。

- 2026-11-25 M1 Task 8（Linux、固定ツールチェーン 1.99.0）:
  - `cargo test --locked -p dgc-daemon --test agent_manager`: 実装前は `dgc_daemon::agent` / `dgc_daemon::provider` 未定義で
    RED（compile error）→ 15 件 PASS（5 回連続 PASS）。新 crate 全テスト PASS、clippy（`-D warnings`）clean。
    `verify.sh test queue_is_drained_after_turn`: PASS（`verify.sh test agent_manager` はテスト名で絞るため 0 件で拒否される）。
  - ミューテーション確認: resume で `resume_session_id` を渡さない → `resume_uses_provider_session_id` FAIL、interrupt で保留権限を
    cancel しない → `interrupt_cancels_pending_permissions` FAIL、failed 時に queue を消す → `crash_marks_failed_and_keeps_queue` FAIL。

- 2026-11-25 M1 Task 8 修正 1（Linux、1.99.0）: `cargo test --locked -p dgc-daemon --test agent_manager` 20 件 PASS（5 回連続）、
  `--lib` 34 件 PASS、新 crate 全テスト PASS、clippy clean。ミューテーション: interrupt 後の読み切りを外す →
  `stale_turn_completed_after_interrupt_is_ignored` と `stale_permission_request_after_interrupt_is_dropped` が FAIL、
  timeout を外す → `wedged_interrupt_fails_agent_and_archive_succeeds` と `hung_start_fails_agent` が FAIL、
  `biased;` を外す → `buffered_turn_completed_is_applied_before_a_later_command` が FAIL。

- 2026-11-25 M1 Task 9（Linux、手動記録。Rust 変更なし）: Claude Code 2.1.332（`--model haiku`）で 8 ターン、Codex 0.172.0
  （`gpt-6-luna`、`effort: low`）で 9 ターンを使い捨て git リポジトリ上で記録。自動承認系 mode は不使用。permission_deny /
  file_change_decline で hello.txt が作られないこと、allow / accept で作られることを確認。記録後 `pgrep -af 'claude -p|codex app-server'`
  で記録器が起動したプロセスが残っていないことを確認（残っていたのは記録前から動いていたユーザーの Codex 常駐デーモンのみ）。
  フィクスチャをユーザー名・メール・`sk-`・`Bearer`・`/home/<実ユーザー>`・組織名・プラン名で grep し該当なし。

## 保留中の小さな改善

- M0 最終レビュー由来: `Cargo.toml` の `[workspace.package].rust-version` とルート `[package].rust-version`
  の一致を検査していない。M1 で crate を増やす前に `check_ci` へ一致検査を足すとよい。
- M0 最終レビュー由来: `test_rust_and_test_modes_cover_workspace` は rust モードの clippy が
  消えても通る（空リストに `all`）。`assertTrue(clippy)` を足す。
- M0 最終レビュー由来: `crates/dgc-cli/tests/version.rs` は版を直書き。`env!("CARGO_PKG_VERSION")` を使う。
- M0 最終レビュー由来: `crates/dgc-protocol/Cargo.toml` 末尾の空行。
