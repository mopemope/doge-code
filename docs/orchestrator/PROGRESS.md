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

- タスク: M1 Task 3（イベントログ）
- 状態: 未着手
- 済んだこと: M0 完了、M1 Task 1（プロトコル型）、Task 2（パス、設定、ロック、Store）完了（ブランチ `feat/orchestrator-m1`）
- 次の一手: M1 計画の Task 3 Step 1 から始める
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
| 3 | イベントログ | 未着手 | |
| 4 | 子プロセス管理 | 未着手 | |
| 5 | ソケットサーバー | 未着手 | |
| 6 | クライアントと `dgc-next daemon` | 未着手 | |
| 7 | プロジェクトとワークスペース | 未着手 | |
| 8 | ProviderAdapter と AgentManager | 未着手 | |
| 9 | 実プロバイダの記録（手動・トークン消費あり） | 未着手 | |
| 10 | 偽プロバイダ | 未着手 | |
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
  - 未検証: 他ユーザー所有ディレクトリの拒否（root なしでは再現できない）。

## 保留中の小さな改善

- M0 最終レビュー由来: `Cargo.toml` の `[workspace.package].rust-version` とルート `[package].rust-version`
  の一致を検査していない。M1 で crate を増やす前に `check_ci` へ一致検査を足すとよい。
- M0 最終レビュー由来: `test_rust_and_test_modes_cover_workspace` は rust モードの clippy が
  消えても通る（空リストに `all`）。`assertTrue(clippy)` を足す。
- M0 最終レビュー由来: `crates/dgc-cli/tests/version.rs` は版を直書き。`env!("CARGO_PKG_VERSION")` を使う。
- M0 最終レビュー由来: `crates/dgc-protocol/Cargo.toml` 末尾の空行。
