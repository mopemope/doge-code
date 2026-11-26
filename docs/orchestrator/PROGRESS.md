# オーケストレーター移行 進捗

このファイルが進捗の唯一の正本。Claude Code / Codex どちらで作業する場合も、
最初にここを読み「現在の作業」から再開する。

- 設計書: [2026-11-25-orchestrator-design.md](../superpowers/specs/2026-11-25-orchestrator-design.md)
- 計画: [M0](../superpowers/plans/2026-11-25-orchestrator-m0-scaffold.md) / [M1](../superpowers/plans/2026-11-25-orchestrator-m1-daemon.md) / [M2](../superpowers/plans/2026-11-26-orchestrator-m2-tui.md)
  （M2 以降の計画は、前のマイルストーン完了時に作成する）

## 運用ルール

- タスクを 1 つ終えるごとに、コードとこのファイルを同じコミットに含める。
- 中断するとき（トークン切れが近いときを含む）は「現在の作業」を更新してコミットする。
  未完成のコードは `wip(scope): ...` コミットにしてよい。
- エージェント固有の記憶、TODO ツール、セッション状態に頼らない。必要なことはここに書く。
- 状態: `未着手` / `作業中` / `完了` / `保留`

## 現在の作業

- タスク: M2 Task 1（デーモン側の前提修正）
- 状態: 未着手
- 済んだこと: M0（PR #183）、M1（PR #184）マージ済み。M2 計画作成（画面構成はサイドバー + ストリームをユーザーが選択、2026-11-26）
- 次の一手: M2 計画の Task 1 Step 1 から始める（ブランチ `feat/orchestrator-m2`）
- 未コミットの変更: なし

## マイルストーン

| M | 内容 | 状態 | 計画 |
|---|---|---|---|
| M0 | workspace 足場、検証/CI 対応 | 完了 | m0-scaffold |
| M1 | デーモン中核と CLI（`dgc-next`） | 完了（PR #184 マージ済み） | m1-daemon |
| M2 | TUI | 作業中 | m2-tui |
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
| 11 | ClaudeAdapter | 完了 | `feat(daemon): drive Claude Code over stream-json` |
| 12 | CodexAdapter | 完了 | `feat(daemon): drive Codex over app-server` |
| 13 | エージェント操作の公開（ソケットと CLI） | 完了 | `feat(cli): expose agent and permission commands` |
| 14 | M1 受け入れ（実 CLI 手動確認とドキュメント） | 完了 | `docs(orchestrator): complete M1 acceptance` |

### M2

| # | タスク | 状態 | コミット |
|---|---|---|---|
| 1 | デーモン側の前提修正（EventLog ハンドル上限、TurnStarted 記録） | 未着手 | |
| 2 | TUI の状態と更新（純粋部分） | 未着手 | |
| 3 | 描画 | 未着手 | |
| 4 | ランタイムと起動コマンド | 未着手 | |
| 5 | M2 受け入れ（手動確認、画面記録、ドキュメント） | 未着手 | |

## 決定事項

設計書から外れた判断、追加依存とその理由を日付付きで追記する。

- 2026-11-26 (M2): TUI の画面構成はサイドバー（プロジェクト／ワークスペース／エージェントのツリー）+ 選択エージェントのストリームと入力欄（ユーザー決定）。`dgc-next tui` と引数なしの `dgc-next` で起動。

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
- 2026-11-25 (M1 Task 11): 依存クレートの追加なし。`provider/claude.rs` の `ClaudeAdapter`（wire 型は同ファイルの `mod wire`）。
  イベント対応はモジュール doc に記載: `system/init`（毎ターン）→ `SessionStarted`、`assistant` の text ブロック →
  `AssistantText{final:true}`、tool_use → `tool_call`（要約は `command`/`file_path`/`notebook_path`/`path` のいずれか、無ければ
  input の compact JSON）、`user` の tool_result → `tool_result`、`result` → `TurnCompleted{usage}`（`is_error:true` なら先に
  `Error`。文言は `result` 文字列、無ければ subtype/terminal_reason）、`rate_limit_event` は破棄（`RateLimited` は TODO(M4)）、
  16 MiB 超の行 → `Error` で継続、未知の type はログに type のみ。`can_use_tool` 以外の control_request には
  `{"subtype":"error"}` の control_response を返す（Agent SDK の形。未記録）。
  中断: 未回答の権限要求に deny を返してから `interrupt` を送り、応答時点で応答待ちのターン数だけ後続の `result` を捨てる
  （その間の `can_use_tool` は deny で自動回答し通知しない）。中断されたターンも必ず `result` を 1 つ出す前提（記録どおり）。
  修正 1: 応答待ちのターンが無ければ interrupt を送らずに Ok を返す（アイドル時の interrupt は未記録）。中断した全ターンが
  `system/init` 済みだったなら、次の `system/init` で破棄を打ち切る（CLI はターンを順に処理するので、中断したターンの
  `result` はそれより前に来るはず。来なかった分は応答待ちから外す）。未開始のターンを中断した場合は warn を出し従来の規則のまま。
  `start` は initialize を 20 秒で打ち切り、起動中にプロセスが終わったら stderr 末尾を含む `ProviderError::Spawn` を返す。
  `shutdown` は stdin を閉じて最大 1 秒待ち、残っていれば `terminate(2 秒)`。セッション drop でプロセスグループを kill する。
  シナリオ `tests/fixtures/claude/scenarios/*.jsonl` は記録から機械変換（dgc が振る control request id を `${ANY}` + capture）。
  `login_error.jsonl` と `crash_mid_turn.jsonl` の終わり方は合成（未ログインは未記録のため、initialize を読んで stderr に出して exit 1）。
- 2026-11-25 (M1 Task 12): 依存クレートの追加なし。`provider/codex.rs` の `CodexAdapter`（`new` / `with_program`、起動は
  `<prefix_args> app-server`）。対応の詳細はモジュール doc。コントローラ裁定に従ったブリーフからの変更:
  - mode に `untrusted`（approvalPolicy `untrusted` / sandbox `workspace-write`）を追加（コマンド承認を得るため）。
    `full-access` だけが auto_approve。`workspace-write` + `on-request` ではワークスペース内コマンドは承認なしでサンドボックス実行（Codex 既定）。
  - Decision は `availableDecisions` があればそれに従う: Allow→`accept`、AllowForSession→提示されていれば `acceptForSession`、
    無ければ `accept`、Deny→提示されていれば `decline`、無ければ `cancel`（`cancel` は Codex 側でターンも中断する）。
    fileChange 承認にはリストが無いので Deny は `decline`。
  - `account/rateLimits/updated` は毎応答出るので `RateLimited` にせず破棄（TODO(M4): `rateLimitReachedType != null` が候補だが未観測）。
    テスト名は `codex_rate_limit_event_routine_update_is_dropped`。
  - 受信行は形で分類（`method`+`id` = サーバー要求、`method` のみ = 通知、`result`/`error` のみ = dgc の要求への応答）。
    dgc の要求 id は 1 から単調増加、各要求は `RPC_TIMEOUT`（20 秒）で打ち切り。
  - ターンは id で追跡（`turn/start` の `result.turn.id` / `turn/started`）。steer は実行中ターンがあれば
    `turn/steer{expectedTurnId}` で true、無ければ送らず false（ターン終了で拒否された場合も false。-32600 は `turn/completed` より先に来うるので、コードで判定する。修正 1）。
    interrupt は実行中ターンが無ければ何も送らない。あればそのターンの未回答承認に Deny 対応の値（`cancel`/`decline`）を返してから
    `turn/interrupt{turnId}`。応答（`{}`、またはターンが先に終わった場合の -32600）後はそのターン id の `turn/completed`・通知・承認を
    破棄する（承認は同様に自動回答）。他のターンには影響しない。
  - 本文は `itemId` ごとに集約し `item/completed` で `AssistantText{final:true}`。完了しない item（steer で打ち切られた旧 item、
    中断されたもの）は次の agentMessage 開始・中断応答・ターン終了時に `final:false` で出す。1 MiB を超えたら `final:false` で分割。
  - commandExecution / fileChange の item は `tool_call`（名前は item type、要約はコマンド / `<kind> <path>`）と `tool_result`
    （出力または status。`completed` かつ exit 0 以外はエラー）にする（Claude アダプタと揃えるためのブリーフ外の追加）。
    承認要求の要約はコマンド文字列、fileChange は先行する `item/started` の changes。`provider_request_id` はサーバー要求 id の JSON 文字列（`"0"`）。
  - `TurnCompleted.usage` はそのターン id の最新 `thread/tokenUsage/updated`。`status:"failed"` は直前の `error` 通知
    （`willRetry:false`）の文言で `Error` を先に出す。`turn/completed` の `items` は使わない。
  - resume は新プロセスで `thread/resume{threadId, model?, excludeTurns:true, approvalPolicy, sandbox}`（deprecationNotice と
    旧ターンの再送を避ける）。`approvalPolicy`/`sandbox` は mode の値を常に送る（未指定は workspace-write / on-request。0.172.0 の
    スキーマにあるが未記録。2026-11-26 最終レビュー修正 5 で「mode 指定時のみ」から変更）。
  - シナリオ `tests/fixtures/codex/scenarios/`: simple_turn / command_approval_accept / file_change_decline / interrupt / steer は記録から
    機械変換（dgc の要求 id は `${ANY}` + capture、`turn/start` の `effort` と `thread/start` の `cwd` は照合しない）。error は記録の
    17〜20 行目（アクティブターン無しの steer/interrupt）を除外。resume は resume_exclude_turns 全体 + resume の 21〜50 行目の合成。
    unknown_server_request / crash_mid_turn / interrupt_then_next_turn / interrupt_cancels_pending_approval は合成（ヘッダに明記）。

- 2026-11-25 (M1 Task 13): 依存クレートの追加なし。`dgc-protocol` に `AgentIdParams{id}`（agent.get / interrupt / archive / resume）、
  `AgentListParams{include_archived}`（既定は archived を除く）、`PermissionListParams{agent_id?}`、`UnsubscribeParams{agent_id?}`
  （指定なしでその接続の全購読を止める）を追加。`LaggedParams.agent_id` を `Option` に変更（全エージェント購読の `lagged` は
  `{agent_id:null, next_seq:0}`）。結果は `agent.list` → `{"agents":[..]}`、`permission.list` → `{"permissions":[..]}`、
  `agent.events` → `{"events":[..]}`、`agent.send` → `{"outcome":..}`、`agent.interrupt` / `permission.respond` / `subscribe` /
  `unsubscribe` → `{}`、create / get / archive / resume → `Agent`。`AgentError` → `RpcError` は `From`（`code()` と表示文言）。
- 2026-11-25 (M1 Task 13): 購読（`subscription.rs`）: 接続ごとに agent フィルタ（`None` = 全体）をキーとする task。同じキーの再購読は置換。
  broadcast の受信口を取ってから backfill し、`since_seq`（agent 指定必須、無ければ INVALID_PARAMS）の backfill は `events_page` を
  ページングして送信キューの空きを待って送る（backfill は lag にしない）。ライブは `try_send`、エージェントごとに送信済み seq 以下を捨てる。
  キュー満杯または broadcast の `Lagged` で `lagged` を 1 回（空きを待って）送り、その購読は終了する（クライアントは `agent.events` /
  `since_seq` 付き再購読で回復）。`next_seq` は最後に送った seq + 1（何も送っていないライブのみの購読では 1）。task は接続の
  writer が消えたとき、`unsubscribe`、接続終了時の `clear()` で終わる。購読 task は `subscribe` の応答をキューに入れた後に送信を
  始める（`Subscriptions::release`。クライアントが応答待ち中の通知を 256 件しか保持しないため）。
- 2026-11-25 (M1 Task 13): プロバイダ解決（`provider/resolve.rs`）: 設定のパス（相対パスや実行可能ファイルでなければ warn して利用不可。
  PATH 探索にはフォールバックしない）→ 無ければデーモンの `PATH` の絶対ディレクトリだけを探索（相対エントリは飛ばす）。
  `--version` は 5 秒で打ち切り、最初の「数字で始まり `.` を含むトークン」を版とする。版が取れなくても見つかったプロバイダは登録する
  （tested=false）。解決は bind（と chmod）の後・accept 開始前に Claude と Codex を並行で行い、続けて `recover_on_startup`（失敗は warn のみ）。
  （修正 1 で変更。下記。）
  終了時は accept 停止・接続 flush の後に `shutdown_all(5 秒)`、その後ソケット削除。サーバーテストは存在しない絶対パスを設定して
  ホストの claude/codex を実行しない。
- 2026-11-25 (M1 Task 13): `workspace.archive` は handler で先にその workspace の非 archived エージェントを archive してから
  workspace を archive する（Task 7 の TODO を解消。途中で workspace 側が失敗してもエージェントの archive は戻さない）。
- 2026-11-25 (M1 Task 13): CLI: `run` / `ls` / `show` / `send` / `interrupt` / `archive` / `resume` / `logs` はトップレベル
  （`commands/agent.rs`、clap の flatten）、`permit {ls|allow|deny}` は `commands/permit.rs`。`run` の title 省略時はプロンプト 1 行目の
  先頭 60 文字。`run --json` は 1 行目に Agent JSON、`--follow` なら続けて AgentEvent JSONL。自動承認 mode は stderr に `warning:` 行を
  常に出す。`--follow` は seq 0 から購読し idle/stopped/failed/archived で終了（idle だけ exit 0）。`lagged` を受けたら最後に表示した
  seq から再購読する。人間向けイベント行の形式は README。CLI テストは `tests/fixtures/scenarios/` の最小シナリオ（記録形状から手書き）を
  `--version` に自前で答えるラッパー（0700、`DGC_FAKE_TIMEOUT_MS=60000`）経由で偽プロバイダに再生させる。
- 2026-11-25 (M1 Task 13 修正 1): 起動順を lock → Store → bind・chmod → プロバイダ解決（並行、各 `VERSION_TIMEOUT` 5 秒）→
  `recover_on_startup` → accept 開始に変更（遅い/ハングする `--version` で自動起動のクライアントが待ち切れなくなるため）。解決中に
  接続したクライアントは listen backlog で待つ。hello の時間切れを防ぐ方法は「accept は解決後に始め、クライアントの `HELLO_TIMEOUT`
  を 15 秒に上げる」（クライアント側の hello タイマーは connect 直後に始まり、accept の時点を知り得ないため）。`SPAWN_WAIT` も 15 秒
  （Task 6 の「最大 5 秒再接続」を置き換え）。両定数と `VERSION_TIMEOUT` の doc に「VERSION_TIMEOUT + 起動の残りより十分大きいこと」を記載。
  パラメータ解析エラーの echo 上限を 200 → `ECHO_LIMIT`（64 文字）に統一（裁定どおり）。
- 2026-11-26 (M1 最終レビュー修正 1、ユーザー決定): `workspace.archive` は所有 worktree に未コミット/未追跡の変更、または
  記録した `base_ref` から到達できないコミット（worktree の HEAD と、削除対象のブランチ）があると INVALID_STATE で拒否する
  （理由に `uncommitted changes` / `N commit(s) not in base` を含む）。`WorkspaceArchiveParams.force`（serde 既定 false）と
  `dgc-next workspace archive --force` で従来どおり `worktree remove --force` と `branch -D` を行う。force なしは
  `git worktree remove <path>`（--force なし）と `git branch -d`（拒否されたらブランチを残す）。`--force` と `-D` は
  `discard_worktree` の中だけに置き、`if force` からしか呼ばない（構造テスト `destructive_git_flags_appear_only_in_the_force_path`）。
  handler は `check_archive` を先に行い、通ったときだけエージェントを archive する（拒否なら何も変えない）。
  `workspace.archive` の結果は `WorkspaceArchiveResult`（Workspace のフィールドを flatten し、`worktree_removed` と
  `branch_kept` を足す。既に archived なら両方 false）。人間向け出力は workspace 行に続けて `branch kept: NAME` /
  `worktree kept on disk: PATH`。作成時のロールバック（store 失敗）も安全な経路を使う。
  注: base からの到達で判定するため、ブランチを main へマージ済みでも base より先のコミットがあれば拒否される（`--force` が必要）。
- 2026-11-26 (M1 最終レビュー修正 2): `agent.events` のページは直列化した配列が `MAX_EVENTS_PAGE_BYTES`（768 KiB、
  `dgc-protocol`）を超える手前で切る（必ず 1 件は返す。保存行の長さ＝直列化長で数える）。このため `limit` 未満のページでも続きが
  あり得るので、`dgc-next logs` と購読の backfill は空ページまでページングする（backfill は符号化できないイベントでも cursor を進める）。
  1 イベントの本文は `MAX_EVENT_TEXT_BYTES`（256 KiB、JSON エスケープ後の長さで数える）以下: `dgc_protocol::chunk_text` で
  Claude の assistant テキスト（最後以外 `final_: false`）、Codex の集約メッセージ（`MAX_MESSAGE_BUFFER_BYTES` も 256 KiB、
  完了時の item 本文も分割）、`UserMessage`（初期プロンプトと send。同じ behavior で複数イベント）を分割する。
- 2026-11-26 (M1 最終レビュー修正 3): CLI の人間向け出力（イベント行、`permit ls`、`ls`/`show`、project/workspace 行、
  `daemon status`、`error:` 行、auto-approve 警告）は `output::escape_text`（`\n` は残す）/ `escape_line`（`\n` も `\\n`。1 行の
  欄で別の行を装えないように）で制御文字を `\r` `\t` `\x1b` `\u{9b}` に、双方向制御文字（U+200E/F、U+061C、U+202A–E、
  U+2066–9）を `\u{..}` にする（双方向制御は brief の範囲外だが、表示と承認内容のずれを防ぐ同じ目的のため含めた）。
  `truncate_summary` は切ったとき末尾に `…(truncated, N bytes total)` を付け、印を含めて 4 KiB 以内に収める。
- 2026-11-26 (M1 最終レビュー修正 4): Codex の `item/permissions/requestApproval` を `PermissionKind::Other` として中継する
  （要約は `permissions: network access, write <path>, <access> <entry> (reason: ...)`、`truncate_summary` で上限付き、表示は修正 3 の
  エスケープ）。応答はスキーマどおり Allow → `{permissions:<要求どおり>, scope:"turn"}`、AllowForSession → `scope:"session"`、
  Deny → `{permissions:{}, scope:"turn"}`（interrupt 時の自動拒否も同じ）。形は codex-cli 0.172.0 の
  `app-server generate-json-schema` 由来で実機記録なし（合成シナリオ 2 本）。`mcpServer/elicitation/request` は M1 では
  従来どおり -32601 で拒否し `Error` イベントを出す。
- 2026-11-26 (M1 最終レビュー修正 5): Codex の `thread/resume` は mode の有無に関わらず `approvalPolicy` / `sandbox` を送る
  （`--mode` なしは既定の `workspace-write` / `on-request`。保存された policy を黙って引き継がない）。記録由来の
  `scenarios/resume.jsonl` の `thread/resume` 期待行に両フィールドを足し、ヘッダに EDITED 注記を付けた。
- 2026-11-26 (M1 最終レビュー修正 6、ユーザー決定): interrupt はキューを捨てる。`agent.interrupt` と `interrupt_and_send`
  （`send --behavior interrupt`、および steer 非対応時のフォールバック。どちらもターンを中断するため同じ扱い）は、interrupt 成功後・
  新しいメッセージ送信前に `queue.json` を空で保存し、捨てた 1 件ごとに `EventKind::QueueDiscarded { text, behavior }` を記録する
  （本文は `UserMessage` と同じく 256 KiB ごとに分割。件数付きの 1 イベントにはしない）。人間向け表示は
  `[seq] queued message discarded: TEXT`。archive / shutdown / 失敗ではキューは従来どおり残る（resume で送られる）。
- 2026-11-26 (M1 最終レビュー修正 7): 所有 worktree の archive で project ディレクトリが存在しない（削除・移動）ときは git を
  実行せず記録だけ archive する（`--force` 不要、`tracing::warn` に workspace/project の id、結果は `worktree_removed: false`・
  `branch_kept: true`）。managed ディレクトリ下に残ったファイルは手動で消す（README に記載）。これで project も `project rm` できる。
- 2026-11-26 (M1 最終レビュー修正 1 の追加修正): 未コミット判定の `git status` に `--untracked-files=all --ignore-submodules=none`
  を付け、安全経路の `git worktree remove` を `-c status.showUntrackedFiles=all` 付きで実行する（git 内部の清浄性チェックの
  `git status` にも `-c` が伝わるため、判定を経ずに呼ばれても未追跡ファイルを消さない）。`status.showUntrackedFiles=no` の設定下で
  未追跡ファイルだけの worktree が消えた穴を塞ぐ。gitignore されたファイルは git 上「作業」ではないため force なしでも消える（README に記載）。
  コントローラ裁定: main へマージ済みでも base より先のコミットがあれば `--force` が必要（README に明記）。

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

- 2026-11-25 M1 Task 11（Linux）: `cargo fmt --all`、clippy（新 crate 4 つ、`--all-targets --all-features -D warnings`）clean。
  `cargo test --locked -p dgc-daemon --test claude_adapter` 11 件 PASS（15 回連続 PASS）、`verify.sh test claude_` PASS、
  新 crate 全テスト PASS。ミューテーション: 中断後の result 破棄を外す → `claude_interrupt` が FAIL、deny 文言を変える →
  `claude_permission_deny` が FAIL（偽プロバイダの差分が stderr に出る）。
- 2026-11-25 M1 Task 11 修正 1（Linux）: 中断で result が来ない場合の対策後、`claude_adapter` 14 件 PASS（15 回連続 PASS）、
  `--test agent_manager` 20 件 PASS、新 crate 全テスト 221 件 PASS、fmt/clippy clean。RED: 修正前のアダプタで
  `claude_interrupt_when_idle_sends_nothing` と `claude_interrupt_without_result_does_not_swallow_next_turn` が FAIL。
  ミューテーション: 未回答の権限要求への deny ループを外す → `claude_interrupt_denies_pending_permission` が FAIL、
  `system/init` での破棄解除を外す → `..._without_result_...` が FAIL、アイドル時の送信省略を外す → `..._when_idle_...` が FAIL。
- 2026-11-25 M1 Task 12（Linux、固定ツールチェーン 1.99.0）: `cargo test --locked -p dgc-daemon --test codex_adapter`: 実装前は
  `dgc_daemon::provider::codex` 未定義で RED（compile error）→ 16 件 PASS（15 回連続 PASS）。`verify.sh test codex_`: PASS（16 executed）。
  新 crate 全テスト PASS、`cargo fmt --all`、clippy（新 crate 4 つ、`--all-targets --all-features -D warnings`）clean。
  ミューテーション: Deny を常に `cancel` → `codex_file_change_decline` FAIL、中断ターンの抑止を外す → interrupt 系 3 件 FAIL、
  `expectedTurnId` を変える → `codex_steer_uses_expected_turn_id` FAIL、`excludeTurns:false` → `codex_resume_calls_thread_resume` FAIL、
  新 item 開始時の旧 item の flush を外す → steer テスト FAIL、未回答承認の cancel を外す → `codex_interrupt_cancels_pending_approval` FAIL、
  エラーコードを -32600 にする → `codex_unknown_server_request_is_rejected` FAIL。
  未検出（記録に該当する並びが無い）: 使用量のターン id 照合、interrupt への -32600 応答（ターンが先に終わった競合）、steer の競合時の false。
- 2026-11-25 M1 Task 12 修正 1（Linux、1.99.0）: steer への -32600 が `turn/completed` より先に来た場合に Err になっていたのを
  `Ok(false)` に修正。合成シナリオ `steer_rejected_turn_ended`（steer と interrupt に -32600、その後で遅れた `turn/completed`、
  steer 文を新ターンとして開始）で `codex_steer_rejected_on_ended_turn_returns_false` と
  `codex_manager_steer_on_ended_turn_sends_new_turn`（behavior=Steer → `InterruptedAndSent`、failed にならず idle、TurnCompleted は 1 件）が
  修正前に FAIL（`turn/steer request failed (-32600)`）→ 修正後 PASS。`--test codex_adapter` 18 件 PASS（10 回連続）、
  `--test agent_manager` 20 件 PASS、`verify.sh test codex_` PASS（18 executed）、fmt/clippy clean。interrupt への -32600 経路も
  このシナリオで通るようになった。
- 2026-11-25 M1 Task 13（Linux、固定ツールチェーン 1.99.0）: RED: `cargo test --locked -p dgc-cli --test agent_cli` 8 件 FAIL
  （`run` 等が未定義で exit 2、`providers: []`）、`cargo test --locked -p dgc-daemon --test server` の追加 4 件 FAIL
  （`unknown method: agent.create`）。GREEN: `--test agent_cli` 8 件 PASS、`--test server` 22 件 PASS（各 10 回連続 PASS）、
  `dgc-daemon --lib` 40 件 PASS（resolve / handlers の単体テストを含む）。`verify.sh test permit_allow_unblocks_agent` PASS（1 executed）、
  `verify.sh test lagged_subscriber` PASS（1 executed）（`verify.sh test agent_cli` はテスト名に一致しないため 0 件として拒否される）。
  新 crate 全テスト PASS、`cargo fmt --all`、clippy（新 crate 4 つ、`--all-targets --all-features -D warnings`）clean。
  ミューテーション: `lagged` 送信を外す → `lagged_subscriber_receives_lagged_notice` FAIL、workspace archive でのエージェント archive の
  絞り込みを壊す → `archive_workspace_archives_agents` FAIL。`subscribe` 応答より前に通知が出ないこと（`release` のゲート）の RED は
  current_thread / multi_thread のどちらでも再現できなかった（応答のキュー投入が backfill の読み出しより常に先に終わる）。
  回帰検出用のアサーションだけ `subscribe_backfills_then_streams_live` に置いた。
- 2026-11-25 M1 Task 13 修正 1（Linux、1.99.0）: 新テスト `slow_provider_probe_does_not_break_autostart`（`--version` が 6 秒 sleep する
  claude、空の PATH）: 修正前は「ソケットが 3 秒以内に現れない」で FAIL（修正前でも status 自体は 5.05 秒付近で辛うじて成功する競合だったため、
  ソケット出現の早さで判定）→ 修正後 PASS（約 5 秒、claude は path あり・version null・tested false）。`--test agent_cli` 9 件、
  `--test daemon_cli` 7 件、`dgc-daemon --test server` 22 件、`-p dgc-client` 12 件、`dgc-daemon --lib` 41 件（echo 上限の単体テスト追加）PASS、
  新 crate 全テスト 261 件 PASS、fmt / clippy（新 crate 4 つ、`-D warnings`）clean。

- 2026-11-25 M1 Task 14 受け入れ（Linux、Claude Code 2.1.332 `--model haiku`、codex-cli 0.172.0 `gpt-6-luna`、固定ツールチェーン 1.99.0）。
  手動確認は TDD の対象外（実 CLI の観察）。`DGC_STATE_ROOT` を作業用ディレクトリ配下に置き、使い捨て git リポジトリ 2 つ（README とファイル 1 つ）で
  実行。自動承認系 mode は不使用、Codex は `--mode untrusted`。プロバイダのターンは Claude 約 12、Codex 約 12（中断を含む）。
  Codex に "mini" は無く（`codex debug models` は luna/terra/sol/astra 系のみ）、Task 9 と同じ最小の `gpt-6-luna` を使った。
  `DGC_STATE_ROOT` を長いパスにするとソケットが `sockaddr_un` の上限を超え `I/O error: path must be shorter than SUN_LEN` で起動できない
  （`/tmp/dgc14` の短い symlink で回避。下の保留中に記録）。
  - A. 主要フロー（本文は記録しない）:

    | 手順 | Claude | Codex |
    |---|---|---|
    | project add → `workspace create --worktree --branch dgc-accept-<p>` | PASS | PASS |
    | `run --follow`（権限要求）→ 別呼び出しの `permit ls` / `permit allow` | PASS（Write の file_change 要求、許可後 hello.txt 作成・idle） | PASS（command 要求、許可後 hello.txt 作成・idle） |
    | `send --behavior queue`（実行中のターンに送る） | PASS（`queued`、ターン終了後に自動送信され 2 ターン目完了） | PASS（同左） |
    | `interrupt`（実行中） | PASS（`interrupted`、idle） | PASS（idle） |
    | `daemon stop` → `daemon start` → `resume` → 追加ターン | PASS（stopped → starting → idle、直前の会話内容を覚えている） | PASS（stopped → starting → idle、追加ターン完了） |
    | `archive` → `workspace archive` | PASS（worktree ディレクトリと `dgc-accept-claude` ブランチが消え、`git worktree list` は本体のみ） | PASS（同左） |

    `run --follow` の終了コードは背景実行のため取得していない（出力は idle で終了）。`daemon stop` 後に dgc が起動した `claude` / `codex app-server`
    は残らなかった（残っていたのは作業前からあるユーザー自身の Claude セッションと Codex 常駐デーモンのみ）。
  - B. 保留していた手動確認:

    | 項目 | 結果 | 観察 |
    |---|---|---|
    | Claude: send 直後（ターン開始前）の interrupt | PASS | idle になり、warn `interrupted a turn that had not started` が出た。次のターンは通常どおり完了し TurnCompleted は 1 件 |
    | Claude: interrupt 直後に `send --behavior interrupt` | PASS | idle 後の送信は `sent`、実行中への送信は `interrupted_and_sent`。どちらも次のターンが完了し TurnCompleted は 1 件 |
    | Codex: 承認待ち中に interrupt（cancel → `turn/interrupt`） | PASS | `tool error: declined`、権限は `cancelled`、idle。ファイルは作られず、保留権限なし |
    | Codex: `permit deny` | PASS | 提示された決定は `accept` / `acceptWithExecpolicyAmendment` / `cancel`（`decline` は無い）ので cancel を送る。`declined`、ターン完了、idle、ファイル無し |
    | Codex: `permit allow --session` | PASS（縮退） | `acceptForSession` は提示されず `accept` にフォールバック（記録は `allow_for_session`）。同じセッションの次のコマンドは再び承認を求めた |
    | Codex: `untrusted` で作った agent の resume で policy が保たれるか | PASS | daemon 再起動と resume の後でもコマンドが承認待ちになった（承認なしで実行されない） |

  - C. ソケットと状態ファイル: ソケット 0600（`run/` は 0700）、`state/` `agents/` `projects/` `workspaces/` `data/` は 0700、
    `daemon.lock` `daemon.log` と agent / project / workspace の json、`events.jsonl`、`queue.json` はすべて 0600。
    `ss -ltnp` / `ss -lunp` にデーモンの pid は無く、`ss -lxp` に `daemon.sock` の LISTEN のみ。`daemon.log`（22 行）に作業中のプロンプト語は 0 件。
  - D. 全体検証（固定ツールチェーン 1.99.0）:

    | コマンド | 結果 |
    |---|---|
    | `cargo fmt --all` | 差分なし |
    | `bash scripts/verify.sh rust` | fmt PASS、clippy PASS、test は旧 `src/` の既知の不安定テスト 1 件で FAIL（`logging::tests::retention_bounds_reserve_active_runs_and_preserve_unmanaged_files`、2398 passed / 1 failed。この時点で後続のテスト実行は止まる） |
    | `cargo test --locked --workspace --no-fail-fast --config 'env.TMPDIR.value="/var/tmp"' --config 'env.TMPDIR.force=true' -- --test-threads=1` | PASS（2706 passed / 0 failed。stray `/tmp/.git` と並列の不安定を避けた） |
    | `bash scripts/verify.sh guidance` | PASS（193 tests） |
    | `bash scripts/verify.sh msrv`（1.94.0） | PASS |

- 2026-11-26 M1 最終レビュー修正（Linux、固定ツールチェーン 1.99.0）: 8 項目を TDD で修正（各項目の RED/GREEN は
  `.superpowers/sdd/2026-11-25-orchestrator-m1-daemon/final-fix-report.md`）。最終確認:
    | コマンド | 結果 |
    |---|---|
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
    | `cargo test --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli`（2 回） | 2 回とも PASS（295 passed / 0 failed） |
    | `bash scripts/verify.sh guidance` | PASS（193 tests） |
  旧 `src/` の全体テストと実 CLI での手動確認（Codex の `item/permissions/requestApproval` は実機未記録）は未実施。

## 保留中の小さな改善

- M1 Task 14 受け入れ由来（すべて小さく、受け入れは通った。M2 前か M2 の最初に直す）:
  - `DGC_STATE_ROOT` 配下のソケットパス長は `DaemonPaths::under` で検査されず、長いと `path must be shorter than SUN_LEN` という
    原因の分かりにくい I/O エラーになる（XDG 経由は `SocketPathTooLong` で明示される）。
  - `dgc-next logs ID | head` のようにパイプが閉じられると `println!` が panic する（Broken pipe）。
  - 中断後、Claude の最後の `assistant_text`（final）が `state: idle` より後に記録される（順序が逆転して見える）。
  - Claude の `turn started` イベントが出ない（README のイベント一覧には `turn started` がある。Codex も未観測）。
  - ユーザー送信の表示が idle への送信でも `user (queue)` になる（`behavior` の既定値が表示されるだけ）。
  - workspace archive 後に空の親ディレクトリ `<worktrees>/<repo>-<id>/` が残る。
- （Task 14 で確認済み）M1 Task 11 / 12 由来の手動確認（Claude の send 直後の interrupt、Codex の承認待ち interrupt・deny・
  `allow --session`・resume 後の policy）は検証記録 B に記録。未検証で残るのは Codex の `thread/resume` の `excludeTurns:true`
  （コードは `true` を送る。記録 `resume_exclude_turns.jsonl` は resume 応答までで、その後のターンは未記録。2026-11-26 に
  「現状は `false`」の誤記を訂正）と、`decline` が提示される承認種別（コマンド承認では提示されなかった）のみ。
- M0 最終レビュー由来: `Cargo.toml` の `[workspace.package].rust-version` とルート `[package].rust-version`
  の一致を検査していない。M1 で crate を増やす前に `check_ci` へ一致検査を足すとよい。
- M0 最終レビュー由来: `test_rust_and_test_modes_cover_workspace` は rust モードの clippy が
  消えても通る（空リストに `all`）。`assertTrue(clippy)` を足す。
- M0 最終レビュー由来: `crates/dgc-cli/tests/version.rs` は版を直書き。`env!("CARGO_PKG_VERSION")` を使う。
- M0 最終レビュー由来: `crates/dgc-protocol/Cargo.toml` 末尾の空行。

## M1 レビューで保留した指摘と判断（2026-11-26、実行台帳から移記）

M1 はタスクごとのレビューと全体の最終レビューを通過した。以下は各レビューで Minor として保留した指摘と、
コントローラが下した判断の全量。M2 以降の計画時に、ここから優先度を付けて取り込む。

### 保留した Minor 指摘

- Task 1: minor (deferred): Response/Notification lack jsonrpc field (plan-mandated); limit_raw boundary test at exactly 64 KiB; PROGRESS.md list blank line
- Task 2: minor (deferred): file modes set via open(2) mode only (umask can make stricter, never looser); list_* order untested; post-rename dir fsync failure returns Err; write_atomic bare relative path; ensure_private_dir creates parents 0700; test gaps (symlink entry in list, dangling target, temp cleanup, events_max_bytes=0)
- Task 3: minor (deferred): corrupt-line warn repeats per read/open and open scans whole file; 0600 not asserted on events.1.jsonl, pre-existing looser mode not tightened; oversized test doesn't assert fresh file
- Task 3: minor (deferred): size undercount after double fault (truncate fails then later write ok then fail+truncate ok); no test drives failure paths through append end to end
- Task 4: minor (deferred): line.clear() keeps 16 MiB capacity after a huge line; tokio macros feature only needed by tests; wait_gone counts zombies as alive (container pid1 flake risk)
- Task 5: minor (deferred): writer task outside JoinSet leaks after drain timeout; shutdown ignored if response send fails; no pre-hello timeout / connection cap; second_hello test doesn't assert INVALID_STATE code; responses_wait test relies on 50ms sleep; JoinError from connection task silently dropped
- Task 6: minor (deferred): test guard may miss a still-booting daemon after a failed run; VersionMismatch after spawn reported as SpawnTimeout; Client::call has no overall timeout (wedged daemon hangs CLI); dgc-cli uses tokio::sync without declaring `sync` feature (unification); missing tests (symlinked daemon.log, log 0600, id-0 error path, pending-notification overflow); pre-existing daemon.log mode not tightened; stop's lock probe can make a concurrent autostart lose
- Task 7: minor (deferred): missing project dir leaves workspace un-archivable → project never removable (consider re-grade at final review); worktree_requires_a_git_repository can't distinguish non-repo vs subdir and manager git inherits global config/hooks in tests; git timeout kills only git not its descendants; failed branch delete only warns (no response field)
- Task 7: minor (deferred): `git worktree list -z` needs git >= 2.36 (no fallback/doc; fails closed); locked worktree with missing dir makes archive fail permanently; lib test name says prefix but checks suffix
- Task 8: minor (deferred, consider re-grade at final review): unsupervised actor panic leaves live entry + leaked pending permissions until restart (adapter panics land here); queued items stall after interrupt; possible double send if save_queue fails after send; swallowed put_agent error in create cleanup; Shared::logs never evicts (fd growth); start failure drops initial prompt; AGENTS_DIR duplicates Store constant; test gaps (race paths, failed queue-head send, archive during WaitingPermission, Exited while Idle, futures Send check missing interrupt/respond, slow_subscriber asserts page clamp only)
- Task 8: minor (deferred): biased select can starve commands under a flooding provider (fairness cap suggested); drain loop has no yield/bound (use len snapshot); no test for Exited during drain; caller gets timeout error before stored state flips (≤5s)
- Task 9: minor (deferred): Codex command deny never recorded — Deny mapping on commands unexercised (ruling: cancel when decline not offered)
- Task 10: minor (deferred): tests hard-code sorted-key output (preserve_order unification risk); uncaptured ${name} in expect matches literally (should exit 2); capture line extra keys ignored; stdin read error treated as EOF; lossy UTF-8 on input
- Task 11: minor (deferred): deny sent before interrupt success known (manager leaves pending on interrupt Protocol error); interrupt internal bound can exceed manager 30s with many pending permissions; full event channel blocks control_response; plan's verify.sh filter name
- Task 11: minor (deferred, consider re-grade at final review): idle-skip in interrupt() races reader delivery — TurnCompleted can arrive after interrupt returns (contract breach, sub-µs window, affects Steer→interrupt_and_send); init-reset relies on unrecorded "result1 before init2" ordering
- Task 12: minor (deferred, consider re-grade at final review): interrupt failing with non-32600 error leaves agent Running (TurnCompleted already dropped); untested race paths (interrupt completed-then-32600, usage correlation by turn id) — testable with fake; chunked message with empty remainder emits no final:true; deltas/approvals without turnId stored under "" never cleared; duplicated process-session plumbing between claude.rs and codex.rs (codex.rs 1187 lines); timing-dependent quiet-period drains in tests
- Task 12: minor (deferred): steer fallback logs only debug (could hide a malformed steer); per-turn usage correlation untested
- Task 13: minor (deferred): lagged next_seq imprecise for live-only subscriptions; concurrent resume can leave an agent running in an archived workspace; --version probe buffers unbounded stdout and kills only direct child (reuse process-group kill); missing tests (unsubscribe stops delivery, all-agents subscription, follow exit 1 on Failed / WaitingPermission continues); resolve.rs ETXTBSY flake risk; idle subscription connections accumulate after lag
- Task 13: minor (deferred): non-autostart CLI calls during daemon startup block up to 15s then HelloTimeout; connect_or_spawn worst case can exceed SPAWN_WAIT; bind→chmod umask window (mitigated by 0700 dir; umask(0o177) around bind would harden)
- Task 14: minor (deferred): long DGC_STATE_ROOT → opaque SUN_LEN error (under() has no length check); `logs | head` panics on broken pipe; Claude final assistant_text after interrupt recorded after state idle; no TurnStarted event emitted though README lists it; idle sends print `user (queue)`; empty <repo>-<id> dir left after workspace archive; run --follow exit code not captured in acceptance
- Task 14: minor (deferred): README lists `turn started` (never emitted) and omits `assistant (partial)` line; README socket fallback wording; systemd example lacks cgroup-stop note; C/D evidence summary-only, no-TCP check covers daemon pid only
- Task 14: minor (deferred): README "does not log prompt text" true for daemon.log but events.jsonl stores prompts too — wording should say so
- Final: minor (deferred): startup error loses stderr tail end when >4 KiB; restart loses pending permissions without permission_resolved{cancelled}; Claude AllowForSession recorded as allow_for_session but sent as allow; daemon stop STOP_WAIT 5s < shutdown ~7.5s; Shared::logs never evicts (fix before M4); Task 11 idle-skip race; Task 12 non-32600 interrupt failure leaves Running; start failure drops initial prompt
- Final: minor (deferred): create_workspace rollback uses branch -d (fresh branch can be left behind with --base other); structural test only matches os("--force")/os("-D") form; second archive check after agents archived can refuse (no loss); discard_queue save failure only warns; ToolCall.name / usage unbounded at source; pre-fix oversized legacy events end logs with an error; gitignored files removed by non-force archive (documented)

### コントローラの判断（Ruling）

- Ruling: execute on branch feat/orchestrator-m1 in the main checkout, no separate worktree — legacy crate rebuild cost; keeps PR #183 reviewable — cost if wrong: none beyond shared checkout
- Ruling R1: Task 5 dispatch is a concrete DaemonDispatcher struct (no trait), connection context type chosen by implementer; Tasks 7/13 extend it — spec §7 only fixes methods, not internal shape — cost if wrong: small refactor if a trait is needed later
- Ruling R2: Task 10 adds scenario line {"expect_args": [..]} asserting the given strings appear in argv in order — needed by Task 11 resume test and Codex start — cost if wrong: none
- Ruling R3: dgc-cli tests locate dgc-fake-provider as a sibling of env!("CARGO_BIN_EXE_dgc-next") (same target dir) and fail with an explicit "run with --workspace" message if absent — verify.sh always passes --workspace — cost if wrong: running `cargo test -p dgc-cli` alone fails those tests with a clear message
- Ruling R4: Task 5 TCP check compares socket inodes from /proc/self/fd against LISTEN inodes in /proc/self/net/tcp{,6} (Linux only) — makes the assertion about this process — cost if wrong: none
- Ruling R5: git-using tests set GIT_CEILING_DIRECTORIES to the tempdir's parent so a stray /tmp/.git cannot leak in — host artifact must not decide results — cost if wrong: none
- Task 1: Ruling: RED/GREEN via `cargo test -p dgc-protocol` instead of `verify.sh test dgc_protocol` — unit test names are module paths without the crate name, so the plan's filter matches zero tests — cost if wrong: none
- Task 2: Ruling: Store::list_* skips unreadable/unknown-version entries with tracing::warn (id + error kind only), get_* still errors — one corrupt file must not block listing/restore at startup; spec's 'reject unknown version' holds per entity — cost if wrong: silent skip hides a corrupt agent from lists (warn in daemon.log)
- Task 3: Ruling: EventLog::open takes agent_id (needed to stamp AgentEvent); adds time.rs now_rfc3339 (no chrono) — plan signature lacked agent id — cost if wrong: none
- Ruling: plan's `verify.sh test <file-name>` filters match test names not files — dispatches use `cargo test -p <crate> --test <file>` for RED/GREEN — cost if wrong: none
- Task 4: Ruling: accept wait() SIGKILLing leftover group members after leader exit — agent tools must not outlive the provider process; avoids signalling a recycled PGID from Drop — cost if wrong: a provider that deliberately daemonizes children within its group loses them
- Task 5: Ruling: run_daemon takes shutdown: watch::Sender<bool> (not Receiver) so the daemon.shutdown handler can trigger it; caller wires SIGTERM/SIGINT to the same sender — plan said Receiver but also requires daemon.shutdown to send true — cost if wrong: none
- Task 6: dispatched implementer ad6a947725b096eac (sonnet), BASE 70a168f; Ruling: autostart spawns 'daemon run' detached via setsid + log redirect (no double re-exec), DGC_LOG level without env-filter feature — keeps deps minimal — cost if wrong: none
- Task 6: Ruling: accept dgc-client → dgc-daemon (+libc) dependency — plan mandated Client::connect(&DaemonPaths) and the autostart spawn reuses ensure_private_dir; dgc-daemon has no heavy deps — cost if wrong: moving DaemonPaths/spawn helpers to a shared crate before phase B (remote client)
- Task 7: Ruling: list results are objects ({"projects":[..]}, {"workspaces":[..]}) not bare arrays; project/workspace Params types (ProjectAddParams{path}, ProjectRemoveParams{id}, WorkspaceListParams{project_id?}, WorkspaceArchiveParams{id}) added to dgc-protocol — Task 1 brief omitted them — cost if wrong: none
- Task 7: Ruling: repo_key uses LAST 12 hex of project uuid (random bits) instead of plan's first 12 (v7 timestamp) — plan defect, collision only caused failed creates/stuck archives — cost if wrong: none
- Task 8: Ruling: provider session calls bounded by PROVIDER_CALL_TIMEOUT=30s, start by 60s, timeout → fail() — reviewer fix proposal; values are controller's choice — cost if wrong: slow provider ops (e.g. heavy interrupt) marked failed; resumable
- Task 9: Ruling (→Task 12): Codex decision mapping uses the request's offered decisions: Deny → `decline` if offered else `cancel`; AllowForSession → `acceptForSession` if offered else `accept`; Allow → `accept` — command approvals offered only accept/acceptWithExecpolicyAmendment/cancel — cost if wrong: Deny on commands interrupts the turn (cancel) instead of continuing
- Task 9: Ruling (→Task 12/14): add Codex dgc mode `untrusted` (approvalPolicy "untrusted", sandbox "workspace-write", auto_approve=false); default stays workspace-write/on-request (Codex's own default: in-workspace commands run sandboxed without approval). Task 14's permit step uses --mode untrusted — needed to exercise command approval — cost if wrong: one more mode string
- Task 9: Ruling (→Task 11/12): M1 adapters do not emit RateLimited from routine per-reply rate-limit updates; only an explicit provider error/result indicating a limit was hit becomes RateLimited; routine updates are dropped (debug log, no content). Precise trigger deferred to M4 handoff work — values at an actual limit were never observed — cost if wrong: M4 handoff proposal fires late/never until refined
- Task 10: Ruling: Task 10 delivers the fake provider + scenario format + its own small test scenarios; adapter scenario files (derived from the Task 9 fixtures, with dgc-generated fields loosened) are authored in Tasks 11/12 because they depend on each adapter's exact outbound messages — plan put the conversion in Task 10 before the adapters exist — cost if wrong: none (same files, later task)
- Task 13: Ruling: bind before probing providers (and widen SPAWN_WAIT to 15s) rather than only enlarging the client wait — keeps the socket reachable regardless of provider binaries — cost if wrong: early clients wait in backlog during probe
- Final: Ruling: archive refuses when branch has commits beyond recorded base even if already merged into main (user decision as written) — user resolves with --force after merging; safest reading of "protect unmerged work" — cost if wrong: extra --force step after merges
- Final: Ruling: steer fallback (interrupt_and_send) also clears the queue — consistent with "interrupt clears the queue" — cost if wrong: a queued message is dropped when a steer falls back
- Final: Ruling: item 1 residual is load-bearing (directly violates the user's archive decision) — resumed the fixer for this one-line fix + test, verified by the controller running the named test rather than a second review round — cost if wrong: an unreviewed small diff in the archive path
- 2026-11-26 M1 最終: 修正の再レビュー後、残り 1 件（未追跡ファイル検出が git 設定に左右される）を 02ae35c で修正し、コントローラが該当テスト 2 件を実行して確認。ワークスペース全体を直列・TMPDIR=/var/tmp で 2742 passed / 0 failed、guidance PASS、msrv PASS（固定ツールチェーン 1.99.0）。
