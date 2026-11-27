# オーケストレーター移行 進捗

このファイルが進捗の唯一の正本。Claude Code / Codex どちらで作業する場合も、
最初にここを読み「現在の作業」から再開する。

- 設計書: [2026-11-25-orchestrator-design.md](../superpowers/specs/2026-11-25-orchestrator-design.md)
- 計画: [M0](../superpowers/plans/2026-11-25-orchestrator-m0-scaffold.md) / [M1](../superpowers/plans/2026-11-25-orchestrator-m1-daemon.md) / [M2](../superpowers/plans/2026-11-26-orchestrator-m2-tui.md) / [M3](../superpowers/plans/2026-11-26-orchestrator-m3-cutover.md) / [M4](../superpowers/plans/2026-11-26-orchestrator-m4-profiles-handoff.md)
  （M2 以降の計画は、前のマイルストーン完了時に作成する）

## 運用ルール

- タスクを 1 つ終えるごとに、コードとこのファイルを同じコミットに含める。
- 中断するとき（トークン切れが近いときを含む）は「現在の作業」を更新してコミットする。
  未完成のコードは `wip(scope): ...` コミットにしてよい。
- エージェント固有の記憶、TODO ツール、セッション状態に頼らない。必要なことはここに書く。
- 状態: `未着手` / `作業中` / `完了` / `保留`

## 現在の作業

- タスク: M5 の計画作成
- 状態: 未着手
- 済んだこと: M0〜M3 マージ済み（#183〜#186）。M4 の実装、受け入れ、最終レビュー修正が完了。合成シナリオの提案・再確認と、実プロバイダ間の 1 回の引き継ぎを確認した。
- 次の一手: M4 の PR をマージ後、M5（スケジュール、heartbeat、ワークスペーススクリプト: dgc.json + 内容ハッシュでの信頼確認、停止中に過ぎたスケジュールはスキップ）の計画を作成する。M4 の保留指摘から前提になるものを選ぶ
- 未コミットの変更: なし

## マイルストーン

| M | 内容 | 状態 | 計画 |
|---|---|---|---|
| M0 | workspace 足場、検証/CI 対応 | 完了 | m0-scaffold |
| M1 | デーモン中核と CLI（`dgc-next`） | 完了（PR #184 マージ済み） | m1-daemon |
| M2 | TUI | 完了（PR #185 マージ済み） | m2-tui |
| M3 | 切替（旧 `src/` 削除、`dgc` に改名） | 完了（PR #186 マージ済み） | m3-cutover |
| M4 | プロファイル、使用量上限時の引き継ぎ（提案→確認）、前提修正 | 完了 | m4-profiles-handoff |
| M5 | スケジュール、heartbeat、ワークスペーススクリプト（dgc.json + 内容の信頼確認） | 未着手 | 未作成 |
| M6 | エージェント向け MCP（サブエージェント） | 未着手 | 未作成 |

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
| 1 | デーモン側の前提修正（EventLog ハンドル上限、TurnStarted 記録） | 完了 | `fix(daemon): bound event log handles and record turn starts` |
| 2 | TUI の状態と更新（純粋部分） | 完了 | `feat(cli): add TUI state and update logic` |
| 3 | 描画 | 完了 | `feat(cli): render the TUI layout` |
| 4 | ランタイムと起動コマンド | 完了 | `feat(cli): run the TUI against the daemon` |
| 5 | M2 受け入れ（手動確認、画面記録、ドキュメント） | 完了 | `docs(orchestrator): complete M2 acceptance` |

### M3

| # | タスク | 状態 | コミット |
|---|---|---|---|
| 1 | ビルドと検証の切替（旧ランタイム削除、`dgc` に改名、スクリプト・CI） | 完了 | `refactor!: replace the legacy runtime with the orchestrator as dgc` |
| 2 | ドキュメントとガイダンスの切替（README、AGENTS.md、docs/ai、skills） | 完了 | `docs: describe dgc as the orchestrator and drop legacy guidance` |
| 3 | M3 受け入れ | 完了 | `docs(orchestrator): complete M3 cutover acceptance` |

### M4

| # | タスク | 状態 | コミット |
|---|---|---|---|
| 1 | 前提修正（末尾読み込み、送信モード、スナップショット合流、起動失敗時の初期プロンプト） | 完了 | `fix: tail history reads and other prerequisites for handoff` |
| 2 | 上限検知（アダプタ） | 完了 | `feat(daemon): detect provider usage limits` |
| 3 | プロファイル | 完了 | `feat: launch agents from named profiles` |
| 4 | 引き継ぎ（デーモン、CLI、TUI） | 完了 | `feat: suggest and run a handoff to another provider` |
| 5 | M4 受け入れ | 完了 | `docs(orchestrator): complete M4 acceptance` |
| 6 | M4 最終レビュー修正 | 完了 | `fix: stop treating Claude overage as a limit and refuse mismatched daemons` |

## 決定事項

設計書から外れた判断、追加依存とその理由を日付付きで追記する。

- 2026-11-26 (M4 最終レビュー決定): Claude の `rate_limit_event` は `status` / `overageStatus` が `allowed` / `allowed_warning`、または `isUsingOverage` が `true` のとき通知を抑止する。HTTP 429 は引き続き検知する。
- 2026-11-26 (M4 最終レビュー決定): プロトコルを 2 に更新する。バージョン不一致は `dgc daemon stop` を案内し、停止コマンドはインスタンスロックの保持と有効な PID を確認して `SIGTERM` を送る。
- 2026-11-26 (M4 最終レビュー決定): Claude の assistant 行は `is_api_error_message: true`、またはモデルが `<synthetic>` かつ `error` が非 null の場合に `Error` とする。
- 2026-11-26 (M4 最終レビュー決定): 引き継ぎ文の回答抽出では、完了したターンの `TurnCompleted` までを評価し、その後の `RateLimited` や `Error` で回答を除外しない。

- 2026-11-26 (M4 Task 4 レビュー決定): TUI の起動 RPC は確認時の原文を `expected_prompt` として送る。デーモンが再生成した文と異なる場合は `INVALID_STATE`（`handoff_changed`）で作成を止め、TUI は新しいプレビューを取得して再確認を待つ。CLI は `expected_prompt` を省略する。
- 2026-11-26 (M4 Task 4 レビュー決定): 冒頭の指示は先頭 16 KiB と省略表示に収める。引き継ぎ文の回答は上限到達やエラーのないターンから取る。Claude の API エラー行と合成エラー行は、4 KiB 以下の `Error` として記録する。
- 2026-11-26 (M4 Task 4 レビュー決定): 引き継ぎの Git 出力は強制カラー設定を上書きし、失敗したコマンド名を表示する。ログ取得はブロッキングスレッドでエージェント単位のロックを使い、未オープンのログも読み取り専用で扱う。サーバーテストの起動待ちは共通の 10 秒を使い、デーモンが途中終了した場合はそのエラーを即時表示する。

- 2026-11-26 (M4 Task 4): 引き継ぎ文の直近ログは最大 1000 件かつ既存の末尾ページのバイト予算内。
  冒頭の読み取りにはプロトコルの最大入力 1 件分を確保する。初回指示がログのローテーションで失われた場合は欠落を明示する。
  プレビューは読み取り専用で、実行時に引き継ぎ文を作り直す。
  TUI はプレビュー取得後、選択変更後およびリサイズ後に確認キーの猶予を設ける。選んだプロファイルは Task 3 と同様に画面上の設定へ展開する。

- 2026-11-26 (M4 Task 3): 起動プロファイルを明示的に選ぶと、その自動承認モードが適用される。
  設定検証と一覧の判定にはアダプタと同じモード表を使う。TUI は新規エージェントフォームを開くたびに取得し、
  プロファイルを選んだ時点で欄に展開する。作成 RPC には画面上の値を送るため、
  モード欄を空にしてプロバイダ既定へ戻した場合も、選択済みプロファイルから再継承されない。

- 2026-11-26 (M4 Task 2、レビュー決定): Claude の `allowed` と `allowed_warning` は通知を抑止し、`rejected`、未知の文字列ステータス、HTTP 429 を検知する。
  Codex の再試行するエラーは通知を抑止する。リセット時刻は `primary` / `secondary` の 100% 以上のウィンドウから最小値を選び、該当がなければ両方を対象にする。
  両アダプタの理由は ASCII 英数字と `_` の最大 48 文字とし、記録済みシナリオで `RateLimited` が出ないことを明示的に検証する。

- 2026-11-26 (M4 Task 1): TUI は選択時に `tail=2000` を 1 ページ要求し、RPC では直近 1000 件かつ 768 KiB に収まる範囲を返す。
  最後の 1 件だけで予算を超える場合はその 1 件を返す。初回取得までのライブイベントが保留上限を超えた場合は、
  取得済みの末尾から順方向に補う。再接続・lagged 後も初回の取得後は順方向に継続する。
  `logs --tail N --follow` は末尾を表示した直後の seq から購読する。0 件指定では出力を始める位置だけを取得する。
- 2026-11-26 (M4 Task 1): lagged がスナップショット取得中に届いた場合は、再購読後の更新要求を 1 件にまとめ、取得完了後に送る。
  起動エラー・タイムアウト時は M1 Task 8 の起動失敗時の方針を更新し、初期プロンプトを通常のキュー項目として保存してから
  `failed` に遷移する。resume は同じ項目を送信してキューから除く。

- 2026-11-26 (M4 計画): ユーザー決定。M4 はプロファイル・使用量上限時の引き継ぎ・前提修正、スケジュール／heartbeat／ワークスペーススクリプトは M5、エージェント向け MCP は M6。引き継ぎは「上限を検知したら提案し、ユーザーが確認して起動」（自動切替しない）。ワークスペーススクリプトはリポジトリの `dgc.json` に置き、初回と内容変更時に内容（ハッシュ）単位で信頼を確認する。デーモン停止中に過ぎたスケジュールは実行しない（記録のみ）。

- 2026-11-26 (M3): Emacs 連携（`elisp/`、旧 `dgc exec` に依存）は削除する（ユーザー決定）。旧エージェント評価・修復受け入れツールと関連文書は削除し、AGENTS.md の Product priority をオーケストレーター向けに書き直す（ユーザー決定）。
- 2026-11-26 (M3 Task 1, 実装者判断): `scripts/check-test-integrity.py` とそのテストは削除した。検査 3 種（`DOGE_SKIP_HTTPTEST`、
  `Skipping httptest-based test` の文言、`src/llm/...` の HTTP 回帰テストへの `#[ignore]`）はすべて旧ランタイム固有で、新 crate に流用できる
  汎用検査が無かったため（計画の「汎用検査が無ければ checker ごと削除」）。
- 2026-11-26 (M3 Task 1, 実装者判断): `scripts/check-sensitive-logging.py` は削除せず付け替えた。対象は `crates/dgc-daemon/src/provider/`、
  `crates/dgc-daemon/src/agent.rs`、`crates/dgc-daemon/src/agent/`、`crates/dgc-cli/src/tui/`（プロンプト・エージェント出力・権限要求を扱う範囲）。
  禁止識別子と抽出規則は従来のまま。付け替え時点で 13 ファイル・tracing 呼び出し 76 件に誤検出なし。対象が 0 ファイルになった場合は
  合格ではなく失敗にした（モジュール移動で無言の no-op にならないように）。
- 2026-11-26 (M3 Task 1, 実装者判断): `check-agent-guidance.py` のリンク検査は、ルートが git のトップレベルなら追跡済みの Markdown だけを
  対象にする（CI が見るのは追跡済みファイルだけ。ユーザーの未追跡メモ `docs/ai/audit-2026-10-29.md` は削除した `src/` へリンクしており、
  触らない約束なので検査対象から外す必要があった）。git でない場合（テストの一時ディレクトリ）は従来どおり全ファイル。
  skills は 0 件でも合格、`docs/tool-output-contract.md` は存在すれば検査。旧ツールカタログ検査は削除。
- 2026-11-26 (M3 Task 1): 削除したファイルへのリンクが残る文書（README の Emacs 節、`docs/ai/evaluation.md`、`docs/ai/repair-acceptance.md`、
  `docs/ai/develop-investigation-2026-10-11.md`）は、リンク記法だけをコード表記に変えた。文書の書き直し・削除は Task 2。
- 2026-11-26 (M3 Task 1): macOS の集中テスト（`verify.sh macos`）のフィルタは `process_group`、`terminate_`（プロセスグループと
  SIGTERM→SIGKILL）、`socket`（unix ソケットのモード・残骸・パス長）、`hello`（全接続が通る peer credential 検査を経る RPC）、
  `autostart`（デーモンの自動起動）、`tui::`。各フィルタが 1 件以上に一致することを Linux で確認済み（下の検証記録）。
  （最終レビュー修正で置き換え。下の 2026-11-26 M3 最終レビュー修正 2 を参照）
- 2026-11-26 (M3 Task 2): README は `dgc` オーケストレーターだけを説明する（約 430 行。旧ランタイムの節はすべて削除）。M1/M2 でレビュー済みの
  節を土台にし、各コマンド・フラグは `target/debug/dgc --help` と各サブコマンドの help、キーと挙動は `crates/dgc-cli` の実装で確認した。
  追加した記述: 目的（ローカル専用・認証情報に触れない）、要件と動作確認版、`cargo install --locked --path crates/dgc-cli`（実行して確認）、
  `daemon run` のフォアグラウンドは stderr にログ、オンデマンド起動のデーモンは起動したコマンドの環境を引き継ぐ、デーモン停止で
  プロバイダのプロセスが終わり再起動後は `stopped`、ロードマップ（M4/M5/フェーズ B）。Codex の権限ダイアログの要約に「コマンド」を足した
  （`provider/codex.rs` の doc どおり。M2 の文はコマンド承認を書き漏らしていた）。
- 2026-11-26 (M3 Task 2): AGENTS.md・workflow.md・architecture.md は新 crate の規則と構成に書き直した（skills なし。ガイダンスの表は
  architecture、workflow、設計書、fixtures README を指す）。削除: 計画の一覧どおりの旧 `docs/ai/` 文書、`docs/tool-output-contract.md`、
  `docs/mcp-3x-migration.md`、`docs/ai/skills/`（5 skills）と `.agents/skills`・`.claude/skills`・`.opencode/skill` のリンク
  （ホスト側のディレクトリも残らない）。`docs/ai/fixtures/` には無視対象の `__pycache__` だけが残ったので消した。
  （修正 1 で、`.gitignore` の skills 用の un-ignore 行と `check-agent-guidance.py` の skills 検査も削除。下記。）
- 2026-11-26 (M3 Task 2 修正 1、コントローラ裁定): `docs/orchestrator/m2-tui-screen.txt` は説明文（見出し・キャプチャの説明行）だけ
  `dgc-next` を `dgc` にし、キャプチャした画面は一切書き換えない（キャプチャ 16 のステータス行は `dgc-next daemon start` のまま）。
  ファイル先頭に、キャプチャは改名前のもので画面はそのままであることを注記した。改名漏れの `rg` 検査はこのファイルのキャプチャを除外する。
- 2026-11-26 (M3 Task 2 修正 1): ガイダンスの不変条件の記述を実装に合わせた。(a) プロンプト・エージェント本文の保存先は
  `events.jsonl` だけではなく、エージェントの 0600 の状態ファイル（`events.jsonl`、キューの `queue.json`、既定でプロンプト 1 行目になる
  title を持つエージェントレコード）。tracing と `daemon.log` には出ない。(b) 権限: dgc は利用者の代わりに承認しない。プロバイダが出した
  要求は利用者を待ち、interrupt / shutdown のときだけ deny / cancel で取り消す。要求なしで何が動くかはプロバイダの mode と利用者の
  プロバイダ設定が決める（Codex `workspace-write`、Claude の allow ルール）。(c) オンデマンド起動のデーモンは起動したコマンドの環境を
  引き継ぎ、クライアントの環境はソケットで送らない。
- 2026-11-26 (M3 Task 2 修正 1): skills の残骸を削除。`.gitignore` の `.claude/skills/`・`.opencode/skill/`・`.agents/skills/` の
  un-ignore をやめ、`.claude/`・`.opencode/`・`.agents/` を丸ごと無視する（個人の skills や設定が追跡対象にならないように。`!CLAUDE.md` は残す）。
  `check-agent-guidance.py` から skills 検査（HOSTS、`valid_description`、`check_skills`）と `docs/tool-output-contract.md` の任意検査を
  削除し、リンク検査と CLAUDE.md 検査は `check_guidance` に改名して残した。CI・ラッパー一致と workflow.md のコマンド検査はそのまま。

- 2026-11-26 (M3 Task 1): ルート `Cargo.toml` は仮想ワークスペース。`[workspace.dependencies]` の既存エントリは全て新 crate が使っており
  削除なし。`[profile.*]` はもともと無い。`.gitignore` は追跡していた `.doge/config.toml` の例外を外し、開発機に残る旧状態のために
  `.doge/` の無視は残した。CI の ripgrep 導入ステップ（旧 search ツールのテスト用）は削除。

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
  起動失敗時の初期プロンプトの扱いは M4 Task 1 の決定事項を参照。ユーザーの send がプロバイダエラーになった場合は状態を変えずに
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
- 2026-11-26 (M2 Task 1): `AgentManager` の EventLog ハンドルキャッシュ（`Shared::logs`）を `LogCache` にして上限を設けた。
  生きているエージェント（actor あり）のハンドルは常に保持し、非 live のエージェントは直近に使った最大 64 件
  （`MAX_IDLE_LOG_HANDLES`）まで。超過分は最も古いものから閉じ、archive 時はそのエージェントのハンドルを閉じる。
  他で使用中（`Arc::strong_count > 1`）のハンドルは追い出さない。`emit`（追記+broadcast）と `events_page`（読み出し）が
  同じログロックを使う不変条件は、追い出しが「誰も持っていないハンドルだけ」を `logs` ロック下で外すことで保たれる。
  テスト用に `AgentManager::open_log_handles()`（`#[doc(hidden)] pub`）を追加。
- 2026-11-26 (M2 Task 1): `EventKind::TurnStarted` を、ターンを始める `UserMessage` の直後に 1 回記録する（初期プロンプト、
  Idle からの送信、キュー先頭の送信（ターン完了時・resume 後）、interrupt_and_send）。steer（実行中ターンへの合流）では記録しない。
  長文でチャンク分割された `UserMessage` は最後のチャンクの後に 1 回。
- 2026-11-26 (M2 Task 2): 新依存なし。`tui/mod.rs` に `#![allow(dead_code)]`（Task 4 で外す）。`AppEvent`/`Effect`/`RequestTag`/`ConnectionState` は
  `tui/mod.rs`、`AppState` ほかは `tui::state`、`update` は `tui::update`、`KeyInput`/`LineEditor` は `tui::input`。
  `ConnectionState::Reconnecting` が `AppState` の初期接続状態。`RequestTag` は Send/Respond/CreateAgent/CreateWorkspace/Interrupt/Resume/Archive。
  設計判断: (1) 履歴読み込み中（選択直後・再接続・lagged 後）の生イベントは保留し、履歴完了（空ページ）後に適用する。
  seq だけで重複排除すると、先着の生イベントが古い履歴を隠すため。(2) 権限ダイアログは Tree フォーカス、または入力欄が空の Input
  フォーカスでだけ自動で開く（書きかけのメッセージの `a`/`d` が権限に答えないため）。Esc で閉じた権限は `dismissed` に入り自動では
  再表示されない（`p` で再表示）。(3) 選択エージェントが Snapshot/archive で変わったら、Input フォーカスは Tree に戻す。
  (4) 作成ダイアログは RpcDone まで開いたまま（`submitting` で二重送信防止、失敗はダイアログとステータスに表示）。成功したら
  結果の Agent/Workspace を木に挿入して選択する。(5) 送信失敗時は入力欄が空なら送ったテキストを戻す。(6) Ctrl-C はどのフォーカスでも終了
  （README のキー表に載せる）。(7) 未知エージェントのイベントによる `LoadSnapshot` は応答待ちの間は 1 回だけ（20 Tick で再試行可）。
  (8) フォーム内の移動は ↑↓、Tab は provider（ワークスペースは local/worktree）切替。
- 2026-11-26 (M2 Task 2 レビュー修正、コントローラ裁定): 上の (2)(3) を次で置き換える。
  (a) 権限ダイアログは Input フォーカス中・他のダイアログ表示中には決して自動で開かない。開くのは Tree フォーカスで、選択中
  エージェントに未 dismiss の権限があり、入力欄が空のときの自動と、Tree での明示的な `p`（入力欄が空でなくても開く）だけ。
  Input 中は `AppState::permission_hint()`（`⚠ permission waiting — Esc then p to review`）を入力欄タイトルとステータスに出す。
  (b) ダイアログが開く／権限が差し替わるたびに a/s/d を 3 Tick（M2 Task 4 で 2 から変更。開いた直後に Tick が来ても残り 2 Tick は
  250 ms の完全な周期なので 500 ms 以上）無視する（`Dialog::permission_armed()`、
  ダイアログに「keys enabled shortly」を表示）。Esc とナビゲーションは有効。
  Input の送信先は入力欄を開いた時点のエージェント（`input_target`）。そのエージェントが archive／snapshot で消えても Input
  フォーカスは維持し、「送信先なし」（`input_target_missing()`、送信は `select an agent first`、入力テキストは保持、ダイアログは開かない）。
  履歴読み込みに世代（generation）を導入: `Effect::LoadHistory` と `AppEvent::History` に `generation: u64` を追加し、現在の世代
  でないページは破棄する（選択 A→B→A、Lagged/Connected 中の再読み込みで古い連鎖が `last_seq` を進めて穴を作るのを防ぐ）。Task 4 の
  backend はページの `generation` を `LoadHistory` からそのまま返す。`agent.events` が失敗したら、空の `done: true` ページ
  （同じ generation）と `RpcDone{ request: RequestTag::LoadHistory{agent_id}, result: Err }` を返す（読み込み中のまま固まらず、
  ステータスに出る）。Snapshot は、要求中に届いたライブの権限イベント（追加・解決）を適用し直す（`permission_log`、要求から応答
  まで、または 20 Tick／切断まで）。
- 2026-11-26 (M2 Task 3): 新依存 `ratatui = { version = "0.30.2", default-features = false, features = ["crossterm_0_29"] }` と
  `crossterm = "0.29"` を `[workspace.dependencies]` と `crates/dgc-cli` に追加（バージョンはルートパッケージと同一。Cargo.lock は dgc-cli
  の依存 2 行のみ変化、新規クレートなし）。`tui::render::render(&AppState, &mut Frame)` は `AppState` だけを読む。
  設計判断: (1) レイアウトは 左ツリー（幅 30%・18〜40 桁、端末幅 < 50 なら 16 桁で、狭いときはプロバイダ名を省き名前を `…` で切る。
  `[auto]` は必ず残す）/ 右上ストリーム / 右下入力欄（権限ヒント表示中は 4 行）/ 最下行ステータス。ダイアログは右ペイン中央に重ねる。
  (2) エスケープ: `Row.label`・ストリーム行・`Status.error`・`Disconnected.reason`・フォーム `error`・`ConfirmArchive.title` は state 側で
  済んでおり二重にかけない。render 側でかけるのは、ストリーム見出しとダイアログ見出しのエージェント名（`escape_line`）、
  権限の `summary`（`escape_text`、改行は行区切りとして保持）、入力欄・フォームの編集テキスト（`escape_line`）。`PendingPermission.raw` は
  表示しない（利用者が答える対象は summary。raw を出すとさらに長大な未整形テキストを表示することになる）。
  (3) 権限ダイアログは、選択中でないエージェントの要求でも「エージェント短縮 id・provider・title」、kind、summary 全文を折り返して表示し、
  選択中でなければ黄色で `(not the selected agent)` を出す。（レビュー修正で置き換え。下の 2026-11-26 M2 Task 3 レビュー修正を参照）
  (4) `!dialog.permission_armed()` の間は `keys enabled shortly` と、淡色（DIM）の `[a]llow [s]ession [d]eny` を出す。armed で通常表示。
  (5) Input フォーカスで権限待ちなら `permission_hint()` を入力欄の最初の行に黒地・黄背景で、ステータスバーにも出す。
  `input_target_missing()` のとき入力欄タイトルに赤字で `no target — select an agent`。
  (6) `output::kind_name` を `pub` にした（権限ダイアログの kind 表示に再利用）。新規ファイル `render.rs`（レビュー修正後は幾何・折り返しを `tui/layout.rs` に分離）。
- 2026-11-26 (M2 Task 3 レビュー修正、コントローラ裁定): 「権限の確認画面は承認する内容をそのまま見せる」を守るため次を変更。
  (a) 長い summary の中央を隠す head/tail 表示と、誤った `permit ls` の「全文」案内を廃止（`permit ls` も同じ 4 KiB に切られた文字列を出す）。
  summary は常に全体を折り返して持ち、スクロールで読む（↑↓ 1 行、PgUp/PgDn 1 ページ、Home/End）。
  (b) 配置計算を純粋関数 `tui::layout::permission_layout(size, summary)` に一本化し、描画（`render`）と a/s の判定（`update`）が同じ結果を使う。
  ヘッダ（3 行: エージェント・選択外の注記・kind）とフッタ（armed ヒント 1 行、スクロール表示、短縮警告、凡例）を先に確保し、残りが summary の
  表示行数。確保量は armed/スクロール状態に依存しない。1 行も取れなければ `too_small`（超過扱い）で、`window too small — enlarge or [d]eny` と凡例だけを出す。
  (c) `Dialog::Permission` に `scroll` / `seen_rows` / `seen_end` を追加。`a`/`s` は armed かつ `seen_end` のときだけ受け付け、`d`（armed が必要）と
  Esc は常に使える。`seen_end` は summary の全行が（上からのスクロールで連続して）画面に出たときだけ真。裁定の文言（「最後の行が一度見えた」）より
  厳しくした: End/Home で末尾へ跳ぶと中間の行が一度も見えないまま許可できてしまい、中間に細工したコマンドを隠せるため。そのため End だけでは
  a/s は有効にならず、上から PgDn/↓ で読み進める（末尾から PgUp で戻って先頭側の既読範囲に繋ぐ読み方は数えない）。収まる場合は即 true。
  端末サイズが変わる（`Resize`）と折り返しが変わるので先頭から読み直し（収まるようになれば即 true）。表示中は
  `rows X–Y of N — scroll to the end to enable [a]/[s]`、読了後は `rows X–Y of N`、読了までは a/s の凡例を淡色にする。
  (d) 折り返しの継続行（要求の改行ではなく幅で折れた行）には淡色の `↪` を左端 1 桁に付ける（シェルでは改行がコマンド区切りのため）。
  全行に 1 桁のガターを取るため本文幅は dialog 内幅 - 1。
  (e) ヘッダのエージェント行は 1 行に切る（id・provider が先頭、長い title は `…`）。
  (f) 文字幅は grapheme 単位（ratatui の `styled_graphemes` と `Span::width`）で測る。char 単位の合計では `⚠\u{fe0f}`（2 桁）等が 1 桁と数えられて
  行末で最後の grapheme が描画されずに消えた。折り返し・切り詰め・末尾取りの全部が同じ関数群（`tui/layout.rs`）を使う。
  (g) 既知の制限（プラン水準の裁定）: プロトコルは summary を 4 KiB で切る（`…(truncated, N bytes total)`）。切られた要求には赤字で
  `this request was shortened by dgc — the full command is not shown` を出す。a/s は読了後に使える。完全なコマンドは表示できない。
  (h) 契約（Task 4 へ）: `permission_layout` は `AppState.size` を使うので、ランタイムは起動直後に実端末サイズの `AppEvent::Resize` を 1 回送り、
  以後のリサイズごとに送ること（既定の 80x24 のままだと、描画側の幅と a/s 判定の幅がずれる）。描画は実際のフレームサイズで配置する。
- 2026-11-26 (M2 Task 3 レビュー修正 2、コントローラ裁定): 権限ダイアログが「描かれていない文字を承認させる」経路を塞ぐ。
  (i) 幅は ratatui が描画に使う `CellWidth::cell_width()`（半角濁点・半濁点 U+FF9E/U+FF9F を 1 桁と数える）で grapheme 単位に測る
  （`unicode-width` との差で、行が収まると判断したのに ratatui が行を途中で切ることがあった）。
  (ii) `permission_layout` は「どの行も 1 桁のガター + 本文が内幅に収まる」ことを保証できなければ `too_small`（a/s 無効、`d` は有効）にする。
  本文幅は `内幅 - 1`（以前の `.max(1)` を廃止）。内幅が凡例の最長項目（`[Esc] close`）以下のときも `too_small`。端末 19〜20 桁（右ペイン 3〜4 桁）で
  何も描かれないのに a が通る経路と、2 桁幅の CJK が落ちる経路を閉じる。
  (iii) summary 中の `↪`（U+21AA）は `\u{21aa}` にエスケープする。本物の行頭の `↪` が折り返しの継続行に見えないようにする（ガターの `↪` だけが折り返しを意味する）。
  (iv) ダイアログの幅の上限（72）と内側の高さは `permission_layout` の値を描画が使う（二重定義を除去）。

- 2026-11-26 (M2 Task 4): 新依存なし（`dgc-client` の tokio に既存 crate の feature `macros` を追加）。
  (1) `tui::backend::Backend::start(paths, exe)` は 1 タスクで RPC 接続と全エージェント購読接続を持つ。Effect は RPC 接続で順に実行
  （`LoadSnapshot` = project.list + workspace.list + agent.list{include_archived:false} + permission.list、`LoadHistory` = agent.events
  を 1 ページだけ取り `done = ページが空`、次のページは `update` が要求する。generation は Effect からそのまま返す。失敗は空の done ページ +
  `RpcDone{LoadHistory, Err}`）。どちらかの接続が切れたら `Disconnected` を 1 回、1 秒ごとに再接続（購読も張り直してから）して `Connected`。
  切断中の `Call`/`LoadHistory` はすぐ失敗を返す（再接続後に古い操作を実行しない）。`connect_or_spawn` は最初の 1 回だけで、以後は
  `connect` のみ（ユーザーの `daemon stop` を TUI が覆さない）。R キーでの再起動は入れず、理由文に「`dgc-next daemon start` starts it」を出す。
  (2) インターフェースの変更: Effect の送信側は `mpsc::Sender` ではなく `mpsc::UnboundedSender`（UI 側が送信で待たないため。UI→backend
  と backend→UI が両方満杯で止まる経路をなくす。イベント側は容量 256 の有界のままで、溢れたら購読がデーモン側で lagged になる）。
  (3) 購読のデコードできない event は lagged と同じ扱い（再購読と再読込）。`Resubscribe` は古い購読タスクを abort して新しい接続で購読する。
  `dgc-client` の購読タスクは受信側が落ちたらすぐ接続を閉じるよう修正（以前は次の通知が来るまで残り、lagged 後は通知が来ないので
  接続が漏れていた）。
  (4) `tui::Runtime<B: ratatui Backend, U>` が状態・端末・Effect 送信を持ち、最初に実サイズの `Resize` を `update` に渡す。各イベントの前に
  `terminal.size()` と `state.size` を比べ、違えば `Resize` を先に渡す。描画したフレームの大きさが `state.size` と違えば `Resize` を渡して描き直す。
  デーモンイベントは最大 64 件まとめて処理してから 1 回描画。Tick は `TICK = 250 ms`、`MissedTickBehavior::Delay`。
  `PERMISSION_ARM_TICKS` を 3 に上げた（上の Task 2 レビュー修正 (b)）。
  (5) `tui::run`: 標準出力が端末でなければ `the TUI needs a terminal`。raw mode 直後に Drop ガード、パニックフック（復元してから既存フック）、
  代替画面、マウスなし。SIGTERM/SIGHUP/SIGINT でも通常終了経路で復元。入力は専用スレッドが `crossterm::event::poll(100 ms)` + `read` し、
  チャネルが閉じたら終わる（join しない）。tracing subscriber は入れない。終了してもデーモンとエージェントは止めない。
  (6) `dgc-next tui` と引数なしの `dgc-next` で起動。`--json` 付きは `the TUI has no JSON mode`。
  (7) テスト構成: dgc-cli はライブラリを持たないので `tests/tui_backend.rs` は `#[path]` で `src/output.rs` と `src/tui/mod.rs` を取り込む
  （そのため tui/output の単体テストがこのテストバイナリでも走る）。偽プロバイダのラッパー生成（`fake_provider`/`scenario`/`write_wrapper`）は
  `tests/common` に移した。テストの `exe` は `DGC_STATE_ROOT` を固定する `dgc-next` のラッパーで、spawn されたデーモンも一時ディレクトリに置く。
- 2026-11-26 (M2 Task 4 レビュー修正、コントローラ裁定): (a) Snapshot が読めない（エラー応答、結果がデコードできない。例: 版の違う古いデーモンが
  `permission.list` を知らない）ときは接続断として扱う: `Disconnected{reason: "cannot load the snapshot: …"}`（update がエスケープ）を出して
  1 秒ごとに再接続・再読込する。`RequestTag::LoadSnapshot` を足してステータスのエラーに出す案は採らなかった（エラー表示は 10 Tick で消え、
  空のツリーが「Connected」のまま残るため）。直後の再接続で `Connected` が一瞬出るが、同じイベントのまとまりで処理されるので表示は理由付きの切断のまま。
  (b) 端末サイズが変わる `Resize`（同じサイズなら何もしない）で権限ダイアログを再び 3 Tick 無効にする（`AppState::rearm_permission`）。
  大きくなった直後は、まだ描画していない配置で `seen_end` が真になり得て、リサイズ前に打った `a` が通ってしまうため。
  (c) RPC 呼び出し（`subscribe` を含む）は 1 回ごとに 15 s（`backend::CALL_TIMEOUT`）で打ち切る。超えたら待っていた要求は `RpcDone Err`
  （`the daemon did not answer within 15 s`）になり、接続断（同じ理由の `Disconnected`）→ 再接続。遅れた応答が次の要求の応答と取り違えられないよう、
  接続は使い続けない。テスト用に `Backend::start_with_timeout(paths, exe, timeout)`。dgc-cli の dev-dependencies に tokio の `net`/`io-util`
  feature を追加（テストの偽デーモン用。新 crate なし、Cargo.lock 変化なし）。
- 2026-11-26 (M2 最終レビュー修正 1): 接続が `serve` 中に失われた（snapshot が読めない、呼び出し失敗、購読が閉じた）後も、`Disconnected` を
  出してから `RETRY_INTERVAL`（1 s）待ってから再接続する（待つ間も Effect は即失敗で答え、UI が閉じれば終わる）。以前は即再接続しており、
  hello には答えるが list 呼び出しに失敗するデーモンとは 2 s で 7000 回以上接続し直していた。
- 2026-11-26 (M2 最終レビュー修正 2、コントローラ裁定): bracketed paste を有効にする（`run` の `enter_screen` で `EnableBracketedPaste`、
  `restore_terminal` の `leave_screen` で `DisableBracketedPaste`。Drop ガード・パニックフック・シグナル終了のどの経路も `restore_terminal` を通る）。
  `TermEvent::Paste(String)` / `AppEvent::Paste(String)` を追加。貼り付けはフォーカス中のテキスト欄（入力欄、またはフォームの編集中の欄）だけに
  1 行として入る（`LineEditor::paste`: `\r\n`・`\n`・`\r`・`\t` はそれぞれ空白 1 個、他の制御文字は捨てる）。ツリー、権限ダイアログ、archive 確認、
  プロバイダ／isolation の選択欄では無視する（貼り付けがキーとして働く経路はない）。以前は貼り付けがキー入力として再生され、複数行は 1 行ずつ
  `agent.send`、タブで送信モードが変わり、ツリーでは `x`/`q`/`A`…`y` が実行されていた。
- 2026-11-26 (M2 最終レビュー修正 3): `agent.send` の成功応答の `outcome`（`SendOutcome`）をステータスバーに出す: `queued — delivered when the turn ends` /
  `sent` / `steered` / `interrupted and sent`（`Status.notice`、アクセント色）。エラーと同じく 10 Tick で消え、エラーが出ると置き換わり、次の成功で消える
  （`clear_error` が両方を消す）。デコードできない outcome は何も出さない。実行中のエージェントへの queue 送信が受理されたか分からず再送して重複する問題への対処。
- 2026-11-26 (M2 最終レビュー修正 4、コントローラ裁定): 接続中は snapshot 要求が無い状態で 20 Tick（5 s、`SNAPSHOT_REFRESH_TICKS`）ごとに
  `LoadSnapshot` を要求する（要求中は数えない。要求すると数え直し。切断中は数えず、切断でリセット）。別のクライアントが追加・archive した
  プロジェクト／ワークスペースはライブイベントが無く、以前は Connected/Lagged/未知のエージェントのときしか読み直さなかった。ツリーで `R`
  （Shift-r）は今すぐ要求する（要求中なら何もしない。未接続なら `not connected to the daemon`）。プロジェクトが無いときツリー枠に
  `no projects — add one with: dgc-next project add <path>` を出す。ステータスバーのキー案内に `R refresh`。
  定期 snapshot は通常の snapshot と同じ処理を通る: 開いている権限ダイアログの arming（`ticks_until_armed`）・scroll・seen_rows・seen_end は
  変えない（テスト `a_periodic_snapshot_leaves_an_open_dialog_as_it_was`）。要求中のライブの権限イベントは従来どおり `permission_log` で再適用される。
  副次効果: 保留していた「重なった snapshot（Connected と Lagged の 2 つが飛んでいて、1 つ目の応答の後に届いた権限を、それより前に読まれた
  2 つ目の応答が消す）」は、次の定期 snapshot（5 s 以内）で権限が戻ることで解消する（テスト
  `the_periodic_snapshot_brings_back_a_permission_an_overlapping_snapshot_erased`。戻ったダイアログは改めて 3 Tick 無効）。
- 2026-11-26 (M2 最終レビュー修正 5、コントローラ裁定): RPC の打ち切りを呼び出しごとに分ける（`backend::CallTimeouts{read, mutating}`、
  既定 `CALL_TIMEOUTS` = 読み取り 15 s / 変更 90 s）。変更系は `agent.send` / `agent.create` / `agent.interrupt` / `agent.resume` / `agent.archive` /
  `permission.respond` / `workspace.create` / `workspace.archive`（`backend::is_mutating`）、それ以外（snapshot の 4 つの list、`agent.events`、
  `subscribe`）は 15 s。デーモン側の上限（Claude の control 20 s、プロバイダ呼び出し 30 s、git 30 s）より短い 15 s で健全な変更呼び出しを
  切断扱いにし、再送で二重配送・worktree 再作成の失敗を招いていた。テスト用の `Backend::start_with_timeout(paths, exe, timeout)` は
  `start_with_timeouts(paths, exe, CallTimeouts)` に置き換え（上の M2 Task 4 レビュー修正 (c) の 15 s は読み取りのみに変更）。
- 2026-11-26 (M2 最終レビュー修正 6、コントローラ裁定): `agent.create` の成功は、送信したフォーム（`submitting` 中の NewAgent）がまだ開いている
  ときだけそれを閉じて新しいエージェントを選択する。Esc で閉じた後なら、ツリーに追加するだけでカーソルは動かさない（以前は選択が新しい
  エージェントに飛び、利用者の `x`/`r`/`A` がそちらに当たった）。その間に開いた別のフォーム（submitting でない）は閉じない。
  同じ扱いを `workspace.create` にも適用した（ブリーフは agent のみ。カーソルが飛ぶと次の `n` が意図しないワークスペースに作るため、同じ誤対象の類）。
- 2026-11-26 (M3 最終レビュー修正、コントローラ裁定): 7 項目を修正。(1) CLI テストは `dgc-daemon` の `dgc-fake-provider`（裁定 R3、
  `dgc` の隣のバイナリ）を使う。`-p dgc-cli` や `--test` 指定ではビルドされず、クリーンビルドでは "dgc-fake-provider is missing" で落ち、
  古い target では古い偽プロバイダが黙って使われる。workflow.md の CLI 行を「先に `cargo build --locked -p dgc-daemon --bin
  dgc-fake-provider`、または `bash scripts/verify.sh test <name>`（ワークスペース全体をビルド）」に変え、理由を明記。テストの panic
  メッセージにもビルドコマンドを書いた。AGENTS.md と README の開発者向け節に同じ落とし穴はなかった（README は verify.sh のみ）。
  (2) `verify.sh macos` は `MACOS_FILTERS` をやめて `cargo test --locked --workspace` 全体を実行する（0 件拒否は維持）。フィルタでは
  store の権限・symlink、flock、`--test process` の大半、`no_tcp_listener`、`second_daemon_is_refused`、`foreground_run_stops_on_sigterm`、
  `run_detach…`、ワークスペースの symlink/パス安全性（macOS では `/var` → `/private/var`）が実行されなかった。上の Task 1 の
  フィルタ決定を置き換える。workflow.md・AGENTS.md の説明も合わせた。CI の macOS ジョブは `bash scripts/verify.sh macos` のまま。
  (3) `check-sensitive-logging.py` の対象を `crates/*/src` 全体（glob で発見するので新 crate も入る）に広げた（41 ファイル、既存コードに
  誤検出なし）。`EXPECTED_CRATES`（protocol/daemon/client/cli）のどれかの `src` がなければ失敗（全部ないときだけでなく）。上の Task 1 の
  対象範囲の決定を置き換える。AGENTS.md・architecture.md・workflow.md を更新。(4) README の権限ダイアログの無効時間を "about 0.75 s" から
  "0.5-0.75 s" に。(5) README の保存先を AGENTS.md と同じ記述（`events.jsonl`、`queue.json`、エージェント記録のタイトル）に。
  (6) README 冒頭の安全性の要約に、interrupt/デーモン停止での要求のキャンセル（deny/cancel）と、`--mode` なしでは provider の既定モードが
  確認なしに動く範囲を決めることを追記し、Security properties and limits へリンク。(7) 上記の保留項目（M0 の rust-version）を解決済みにした。

## 検証記録

実行したコマンド、結果、環境で実行できなかったチェックを日付付きで追記する。

- 2026-11-27 M4 最終レビュー修正（Linux、Rust 1.99.0）
  - RED で Claude の追加使用の誤検知と合成回答の誤分類、両方の hello 不一致、旧デーモンの停止エラー、完了済み回答の欠落、
    アダプタ確認前のログ読み取りを再現した。GREEN で偽デーモンへの `SIGTERM` による停止と、空きロックの古い PID への送信拒否を確認した。
  - 偽プロバイダをビルドし、`cargo test --locked` で `claude_adapter` 22 件、`agent_manager` 45 件、`server` 28 件、
    `dgc-client` 14 件、`daemon_cli` 10 件、`agent_cli` 17 件、`tui_backend` 158 件が PASS。
    `verify.sh test` の名前フィルタは `server` が 7 件で、残り 5 名は 0 件で終了コード 2。対象の全件は上記の直接指定で検証した。
  - `cargo fmt --all`、`bash scripts/verify.sh rust`（Clippy、Rust 649 件）、`guidance`（Python 40 件）、
    `tui-deps`、`msrv`（Rust 1.94.0）が PASS。実プロバイダで追加使用したターンと macOS は未検証。

- 2026-11-26 M4 Task 5 受け入れ（Linux、Rust 1.99.0）:
  - 合成シナリオ PASS。`profile ls`、CLI と TUI の通常・自動承認プロファイル起動、警告、上限時の提案と読み取り専用プレビューを確認。
    `H` のプレビュー後にファイルを追加すると作成が止まり、再確認後に偽 Codex が同じワークスペースで 1 ターンを完了した。
    TUI 起動前の上限イベントは、対象の履歴を読むまでツリーに `⇄` が出ない。[画面記録](m4-tui-screen.txt)の 3・4 を参照。
  - 実機 Claude Code 2.1.332 / Haiku（`manual`）PASS。短い計算を 1 ターンで完了し、idle に戻った。
  - 実機 Codex CLI 0.172.0 / GPT-6 Luna（既定モード）PASS。`dgc handoff ID --to codex --model gpt-6-luna --follow` で、
    同じワークスペースで前回答を使って 1 ターンを完了した。両ターンで権限要求は発生しなかった。上限到達の検知は合成シナリオで確認した。
  - `cargo fmt --all --check`、`bash scripts/verify.sh rust`（Clippy、Rust 641 件）、`guidance`（Python 40 件）、
    `msrv`（Rust 1.94.0）、`tui-deps` PASS。macOS は未検証。

- 2026-11-26 M4 Task 4 レビュー修正（Linux、Rust 1.99.0）:
  - RED で古いプレビューによる新規作成、回答の欠落、100 KiB の最初の指示の拒否、Git の色と失敗名、未オープンのログの読み取りの待機、起動失敗の報告遅延を再現。
  - GREEN は `agent_manager` 43 件、`claude_adapter` 20 件、`server` 28 件、`tui::` 138 件。
    偽プロバイダのビルド後の `agent_cli` 17 件と `tui_backend` 158 件も通過。
  - フォーマット、4 crate の全機能と全ターゲットの Clippy、`verify.sh rust`（641 件）、`guidance`、`tui-deps` PASS。
    tmux でプレビュー後にファイルを作ると起動を拒否し、更新されたプレビューを確認してから偽 Codex が同じワークスペースで応答することを確認。
    実プロバイダと macOS は未検証。詳細は Task 4 の作業レポートに追記。

- 2026-11-26 M4 Task 4（Linux、Rust 1.99.0）:
  - RED: `agent.handoff` が未定義。CLI は未登録のサブコマンドと上限提案の欠落で 2 件失敗し、TUI の対象 4 件と履歴からの提案テストも失敗。
  - GREEN: `cargo test --locked -p dgc-daemon --test agent_manager --test events_log` 50 件、
    `cargo test --locked -p dgc-cli --bin dgc tui::` 137 件、偽プロバイダのビルド後の
    `cargo test --locked -p dgc-cli --test agent_cli --test tui_backend` 174 件 PASS。
  - `cargo fmt --all`、4 crate の全ターゲット・全機能の Clippy、`bash scripts/verify.sh rust`（631 件）、
    `guidance`、`tui-deps` PASS。既存のデーモン再起動テストは全体実行で一度ソケット待ちで失敗し、単独実行と続く全体実行では通過。
  - 112×32 の tmux で合成 HTTP 429 の CLI と TUI の提案、読み取り専用プレビュー、自動承認の赤い警告、
    通常モードの偽 Codex が同じワークスペースでターンを完了することを確認。端末記録は Task 4 の作業レポートを参照。
    実プロバイダと macOS は未検証。

- 2026-11-26 M4 Task 3（Linux、Rust 1.99.0）:
  - RED: 新規の 7 テストが、設定の `profiles` キー、CLI の `--profile`、フォームを開いたときの一覧要求が未実装のため失敗。
  - GREEN: `cargo test --locked -p dgc-daemon --test store --test server --test agent_manager` で計 80 件、
    `cargo test --locked -p dgc-cli --bin dgc tui::` で 133 件、偽プロバイダのビルド後の
    `cargo test --locked -p dgc-cli --test agent_cli` で 14 件 PASS。
  - `cargo fmt --all`、4 crate の全ターゲット・全機能の Clippy、`bash scripts/verify.sh rust`（612 件）、
    `guidance`、`tui-deps` PASS。
  - tmux でプロファイルを選んだ直後の赤い警告、通常プロファイルへの切り替え、偽プロバイダによる応答を確認。
    端末記録とテスト出力は Task 3 の作業レポートを参照。実プロバイダと macOS での確認は未実施。

- 2026-11-26 M4 Task 2 レビュー修正（Linux、Rust 1.99.0）:
  - RED: Claude の警告、両プロバイダの未正規化の理由、Codex の再試行エラーとリセット時刻の選択を再現。
    通常の通知を誤検知させる一時的な変更で、記録済みの Claude resume と Codex steer のアサーションが失敗することも確認。
  - GREEN: `cargo test --locked -p dgc-daemon --test claude_adapter --test codex_adapter` で Claude 20 件、Codex 28 件 PASS。
    `cargo fmt --all`、4 crate の全ターゲット・全機能の Clippy、`bash scripts/verify.sh rust`（604 件）、`guidance` PASS。

- 2026-11-26 M4 Task 2（Linux、Rust 1.99.0）:
  - RED: `cargo test --locked -p dgc-daemon --test claude_adapter --test codex_adapter --no-fail-fast` で
    上限イベントが生成されず Claude 3 件、Codex 4 件が失敗。通常通知を無視するテストは通過。
  - GREEN: `cargo test --locked -p dgc-daemon --test claude_adapter --test codex_adapter` で Claude 18 件、Codex 25 件 PASS。
  - `cargo fmt --all`、`cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings`、
    `bash scripts/verify.sh rust`（599 件）、`guidance` PASS。
  - Claude Code 2.1.332 の記録と Codex CLI 0.172.0 の app-server 生成スキーマを根拠に、上限到達は合成シナリオで検証。
    実プロバイダの上限到達は未観測。macOS での確認は未実施。

- 2026-11-26 M4 Task 1（Linux、Rust 1.99.0）:
  - RED: 末尾読み取り API の未定義、RPC での `tail` 無視、起動失敗後の `queue.json` 不在、TUI の末尾要求不在・送信モード残留・
    スナップショット重複、CLI の `--tail` 未定義を確認。
  - GREEN: `cargo test --locked -p dgc-daemon --test events_log` 14 件、`--test agent_manager` 29 件、
    `cargo test --locked -p dgc-cli --bin dgc tui::` 132 件、偽プロバイダをビルド後の
    `cargo test --locked -p dgc-cli --test agent_cli --test tui_backend` 164 件 PASS。
  - `cargo fmt --all`、`cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings`、
    `bash scripts/verify.sh rust`（590 件）、`guidance`、`tui-deps` PASS。
  - 隔離した状態ディレクトリと偽プロバイダを使い、tmux で履歴の表示、steer 送信後の queue 復帰、ライブ応答を記録。
    同じ応答が `logs --tail 2 --follow` に連続した seq で届くことを確認。端末記録と RED/GREEN の出力は Task 1 の作業レポートを参照。
    実プロバイダと macOS での確認は未実施。

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
- 2026-11-26 M2 Task 1（固定ツールチェーン 1.99.0）。RED: `cargo test --locked -p dgc-daemon --test agent_manager` が新規 4 テスト
  と更新した `create_runs_initial_prompt_then_idle` で FAIL（TurnStarted 欠落、ハンドル 103 件）。GREEN 後:
    | コマンド | 結果 |
    |---|---|
    | `cargo test --locked -p dgc-daemon --test agent_manager` | PASS（27 passed） |
    | `cargo test --locked -p dgc-daemon --test server` | PASS（23 passed、変更なし） |
    | `cargo test --locked -p dgc-cli --test agent_cli` | PASS（12 passed、変更なし） |
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
- 2026-11-26 M2 Task 2（固定ツールチェーン 1.99.0）。実装を先に書いたため RED は `update` を空 Vec 返しのスタブにして確認
  （35/39 が FAIL。残り 4 件は `update` に依存しない input/tree の 3 件と空振りで通る 1 件）、実装に戻して GREEN:
    | コマンド | 結果 |
    |---|---|
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::` | PASS（39 passed） |
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
- 2026-11-26 M2 Task 2 レビュー修正（固定ツールチェーン 1.99.0）。新規テストを先に書き、現行コードで RED を確認（13 件が実行時 FAIL、
  `generation` 系は未実装でコンパイルエラー 23 件、API だけ足すと 18 件 FAIL、世代カウンタだけで 17 件 FAIL）してから実装:
    | コマンド | 結果 |
    |---|---|
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::` | PASS（61 passed） |
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
- 2026-11-26 M2 Task 3（固定ツールチェーン 1.99.0）。テストを先に書き（`render` は何も描かないスタブ）、RED: `cargo test --locked -p dgc-cli
  --bin dgc-next tui::render` が 16 件すべて FAIL（0 passed / 16 failed、バッファが空）。実装後 GREEN:
    | コマンド | 結果 |
    |---|---|
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::render` | PASS（17 passed） |
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::` | PASS（78 passed） |
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
    | `bash scripts/verify.sh tui-deps` | PASS |
    | `bash scripts/verify.sh msrv` | PASS（rustup run 1.94.0 cargo check --locked --workspace --all-targets --all-features） |
- 2026-11-26 M2 Task 3 レビュー修正（固定ツールチェーン 1.99.0）。新規テストを先に書き、旧コードで RED（10 件が FAIL: 未読の要求に a/s が通る、
  Resize で有効/無効にならない、End で中間を飛ばして許可できる、スクロール表示なし、小端末でフッタ欠落、長い title でフッタ欠落、`↪` なし、
  短縮警告なし、`⚠\u{fe0f}` が 40 個中 24 個しか描画されない）。実装後:
    | コマンド | 結果 |
    |---|---|
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::` | PASS（89 passed） |
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
    | `bash scripts/verify.sh msrv` | PASS |
- 2026-11-26 M2 Task 3 レビュー修正 2（固定ツールチェーン 1.99.0）。新規テストを先に書き、旧コードで RED（4 件 FAIL: 行が ratatui の描画幅より広い、
  19〜24 桁でテキストが描かれないのに a が通る、行頭 `↪` が画面に出る、レイアウト不変条件）。実装後:
    | コマンド | 結果 |
    |---|---|
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::` | PASS（94 passed） |
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
    | `bash scripts/verify.sh msrv` | PASS |

- 2026-11-26 M2 Task 4（固定ツールチェーン 1.99.0）。テストを先に書き RED を確認: `cargo test --locked -p dgc-client --lib
  dropping_the_subscription` が FAIL（5 s 待っても接続が閉じない）。`cargo test --locked -p dgc-cli --bin dgc-next tui::` と `--test tui_backend`
  は未実装 API でコンパイルエラー（`Backend`/`Runtime`/`run_loop`/`key_input`/`tick_interval`/`TerminalGuard` など）。API を入れた後
  `PERMISSION_ARM_TICKS = 2` のまま 2 件 FAIL（500 ms を満たさない）。実装後 GREEN。加えて故意の改変で該当テストが落ちることを確認
  （サイズ同期を外す、描画時の補正を外す、短いページを done 扱い、再接続で spawn、Resubscribe を無視）:
    | コマンド | 結果 |
    |---|---|
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::` | PASS（104 passed） |
    | `cargo test --locked -p dgc-cli --test tui_backend` | PASS（117 passed = 新規 7 + 取り込んだ単体テスト。5 回連続 PASS、デーモン・偽プロバイダの残存なし） |
    | `cargo test --locked -p dgc-cli -p dgc-client` | PASS（dgc-next 110、agent_cli 12、daemon_cli 7、tui_backend 117、version 1、workspace_cli 3、dgc-client 13） |
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
    | `bash scripts/verify.sh msrv` | PASS（dgc-client の Cargo.toml を変更したため） |
    | `bash scripts/verify.sh guidance` | PASS（193 tests） |
- 2026-11-26 M2 Task 4 レビュー修正（固定ツールチェーン 1.99.0）。新規テストを先に書き RED を確認: `growing_the_terminal_rearms_the_permission_dialog`
  が FAIL（リサイズ前に打った `a` が `permission.respond` を送る）。`backend_reports_a_snapshot_it_cannot_load` と
  `backend_gives_up_on_a_daemon_that_does_not_answer` は `start_with_timeout` 未実装でコンパイルエラー、タイムアウトを無視するスタブでは
  どちらも 30 s 待ってもイベントが来ず FAIL。実装後:
    | コマンド | 結果 |
    |---|---|
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::` | PASS（105 passed） |
    | `cargo test --locked -p dgc-cli --test tui_backend` | PASS（120 passed、6 回連続、残存プロセスなし） |
    | `cargo test --locked -p dgc-cli` / `-p dgc-client` | PASS |
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
    | `bash scripts/verify.sh msrv` | PASS（dgc-cli の Cargo.toml を変更したため） |
- 2026-11-26 M2 Task 5 受け入れ（Linux、固定ツールチェーン 1.99.0）。手動確認なので TDD は適用しない。コードは変更していない。
  Step 1（偽プロバイダ、PASS）: tmux（`-L dgcm2`、120x32）の実 pty で `dgc-next tui` を操作。`claude_path` は `dgc-fake-provider` を実行する
  ラッパー（手書きの合成シナリオ。長い複数行 Bash 権限要求 43 行 → allow → ターン継続 → queue/interrupt/steer の各送信に対応）。使い捨て git
  リポジトリ、スクラッチの `DGC_STATE_ROOT`。画面は `docs/orchestrator/m2-tui-screen.txt`（19 枚）。確認できたこと:
  空のツリー（`w` はプロジェクト未選択で `select a project first`）、`w` でローカルと worktree ワークスペース作成、`n` でエージェント作成、
  権限ダイアログ（開いた直後の `a` は無視 = `keys enabled shortly`、未スクロールの `a` は無視、↓/End で全行が一度見えた後の `a` は許可）、
  ストリーム（許可・ツール結果・応答・turn completed）、`i` で入力、送信 3 モード（idle への queue = sent、実行中の queue 2 件、steer、
  interrupt。steer/interrupt で `queued message discarded` と `user (steer)` / `user (interrupt)` がストリームに出る）、Input フォーカスの権限ヒント
  （ダイアログは自動で開かず、Esc 後 `p` で再表示）、`A` の確認ダイアログ、60x20 の狭い端末、`q` で終了しエージェントは running のまま、
  再起動後にツリーとストリームが再表示、TUI を開いたまま `daemon stop` → ステータスバーに `daemon disconnected: ...retrying every second`、
  `daemon start` → 自動再接続し、エージェントは stopped 表示。バグは見つからず。
  Step 2（実 Claude 2.1.332、`--model haiku`、`--mode manual`（自動承認ではない）、使い捨てリポジトリ、PASS）: TUI の `w` → `n`（model/mode/prompt を
  フォームに入力、プロンプトは hello.txt を Write ツールで作らせる短文）→ file_change の権限ダイアログが出た → `a` → `tool result` → `assistant: done` →
  `turn completed` → idle まで 1 ターン。観察: haiku は Write の絶対パスの途中の `/` を `-` に取り違え（`.../doge-code-e2584448...` のように）、
  リポジトリの外の別ディレクトリに hello.txt を作った（ダイアログがそのパスを表示していたので利用者は気付ける。dgc の不具合ではない）。
  後始末として、その余分なディレクトリを削除。TUI/デーモン/tmux は停止し、残存する `dgc-next`/`dgc-fake-provider`/`claude -p` なし。
  Step 3（固定ツールチェーン cargo/rustc 1.99.0）:
    | コマンド | 結果 |
    |---|---|
    | `cargo fmt --all` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
    | `bash scripts/verify.sh guidance` | PASS（193 tests） |
    | `bash scripts/verify.sh tui-deps` | PASS |
    | `bash scripts/verify.sh msrv` | PASS（1.94.0） |
    | `cargo test --locked --workspace --no-fail-fast --config 'env.TMPDIR.value="/var/tmp"' --config 'env.TMPDIR.force=true' -- --test-threads=1` | PASS（2972 passed / 0 failed / 0 ignored、残存プロセスなし） |
  README を書いた後に実行した。コミット後（21c110a）に `bash scripts/verify.sh guidance` を再実行して PASS（193 tests）。
- 2026-11-26 M2 Task 5 修正ラウンド 1（固定ツールチェーン 1.99.0）。レビュー指摘: (1) README が権限ダイアログを「要求全体を表示」と書いていた
  のを、実際の「dgc の 1 行 summary（Claude のファイル系ツールはパスのみで内容は出ない）」に直した。a/s は「summary 全体が画面に出た後」に有効、
  4 KiB 短縮は summary が不完全になる場合の 1 つ、と明記。(2) 入力欄の書きかけが全エージェントで共有され、別のエージェントに送られる不具合を修正
  （コントローラ裁定でバグ）。`AppState.drafts: HashMap<AgentId, LineEditor>` に他エージェントの下書きを退避し、`state.input` は常に
  `input_target` の下書き。ツリーでの選択変更（`settle`）と `i` で `bind_draft` が入れ替える。自動で開く権限ダイアログの空判定は
  選択中エージェント自身の下書き（`draft_is_empty`）。送信失敗時の復元も対象エージェントの下書きへ。入力欄の題に `→ <短縮 id> <title>`（`escape_line`、24 桁まで）を
  出し、別のエージェントを選んだツリーでは他人の下書きを表示しない。新規エージェントのフォームのプロンプトは `(required)` に変更。
  README の入力節、狭いツリーの文、初回接続だけデーモンを spawn する文を修正。
  RED（実装前、`cargo test --locked -p dgc-cli --bin dgc-next tui::`）: 105 passed / 3 failed。`a_draft_stays_with_its_agent`（a2 の入力に a1 の文字が残る）、
  `another_agents_draft_does_not_block_the_permission_dialog`（a2 の権限が開かない）、`input_title_names_the_target_and_hides_another_agents_draft`（題に対象がない）。
    | コマンド | 結果 |
    |---|---|
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::` | PASS（108 passed） |
    | `cargo test --locked -p dgc-cli --test tui_backend` | PASS（123 passed、残存プロセスなし） |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
    | `cargo fmt --all --check` | 差分なし |
    | `bash scripts/verify.sh guidance` | PASS（193 tests、PROGRESS 更新後に実行） |
- 2026-11-26 M2 最終レビュー修正 1〜6（固定ツールチェーン 1.99.0）。各項目でテストを先に書き RED を確認してから実装（詳細は
  `.superpowers/sdd/2026-11-26-orchestrator-m2-tui/final-fix-report.md`）。RED: (1) `backend_waits_a_second_before_reconnecting_after_a_lost_snapshot`
  が FAIL（2 s で 7206 回 Connected）。(2) 貼り付けを従来どおりキーとして再生するスタブで 8 件 FAIL（入力欄で `agent.send` 3 回・タブで steer、
  ツリーで `agent.interrupt` + 送信、権限ダイアログで `permission.respond` allow、archive 確認で `agent.archive`、フォームで `agent.create`、
  貼り付けた `q` で終了、bracketed paste 未有効、1 行化なし）。(3) `Status.notice` を設定しないスタブで 3 件 FAIL。(4) 5 件 FAIL（定期／`R` の
  `LoadSnapshot` なし、空ツリーの案内なし。重なった snapshot が権限を消すことは再現）。(5) 全呼び出しが読み取りの上限を使うスタブで
  `backend_gives_mutating_calls_longer_than_reads` が FAIL（`did not answer within 0.3 s`）。(6) 2 件 FAIL（Esc 後の作成成功で選択が新しい
  エージェント／ワークスペースに移る）。実装後:
    | コマンド | 結果 |
    |---|---|
    | `cargo fmt --all --check` | 差分なし |
    | `cargo clippy --locked -p dgc-protocol -p dgc-daemon -p dgc-client -p dgc-cli --all-targets --all-features -- -D warnings` | PASS |
    | `cargo test --locked -p dgc-cli --bin dgc-next tui::` | PASS（128 passed） |
    | `cargo test --locked -p dgc-cli --test tui_backend` | PASS（146 passed、2 回連続、残存プロセスなし） |
    | `cargo test --locked -p dgc-cli` | PASS |
    | `bash scripts/verify.sh guidance` | PASS（193 tests） |
    | `bash scripts/verify.sh tui-deps` | PASS |
  依存の追加・Cargo.lock の変化なし（bracketed paste は crossterm の既定 feature）。手動確認（tmux の実 pty、120x30、`$HOME/.cache` 下の
  使い捨て `DGC_STATE_ROOT`）: 空のツリーに `no projects — add one with:` / `dgc-next project add <path>` が出る。`tmux paste-buffer -p`（bracketed）で
  `q\nx\tA y\n` を貼り付けても TUI は終了せず何も起きない（打鍵の `q` では終了）。TUI を開いたまま別のシェルで `project add` すると、キーを押さずに
  約 6 s 後のツリーに出た。入力欄・フォームへの貼り付けは単体テストのみ（実エージェントでは未確認）。デーモンは `daemon stop` で停止、残存プロセスなし。
- 2026-11-26 M3 Task 1（Linux、固定ツールチェーン 1.99.0）:
  - RED（スクリプトのテストを先に更新、`python3 -m unittest discover -s scripts/tests -p 'test_*.py'`）: 177 tests、15 failures / 14 errors
    （msrv・check_ci・check-tui-deps が `manifest["package"]` を読む、macOS フィルタが旧名、ガイダンスが test-integrity を実行、
    skills 0 件・未追跡メモの扱い、CI の ripgrep、sensitive-logging の対象が旧 `src/`）。
  - GREEN: 同コマンドで 40 tests OK（旧評価ツールのテストは削除）。
  - `cargo build --workspace --offline` で Cargo.lock を更新（510 パッケージ削除、追加・版変更なし）。`cargo build --locked --workspace` PASS。
  - `cargo tree --workspace | rg -c "sea-orm|reqwest|tree-sitter|rmcp|axum"` → 0 件。`rg -n "dgc-next" crates scripts .github` → 0 件。
    `rg -n 'CARGO_BIN_EXE_dgc\b|CARGO_BIN_EXE_dgc"' crates` → dgc-cli のテスト 4 箇所。`target/debug/dgc --version` → `dgc 0.1.0`。
  - 各 crate 単独の `cargo check --locked -p <crate> --all-targets`: 4 crate とも PASS（ルートパッケージの feature 統合が無くなっても成立）。
    | コマンド | 結果 |
    |---|---|
    | `cargo fmt --all` → `bash scripts/verify.sh rust` | PASS（fmt、clippy `-D warnings`、test 576 executed。旧 `/tmp/.git` 由来・並列 flake の失敗は消えた） |
    | `bash scripts/verify.sh guidance` | PASS（40 tests） |
    | `bash scripts/verify.sh msrv` | PASS（rustup 1.94.0） |
    | `bash scripts/verify.sh tui-deps` | PASS（`dgc-cli` の crossterm 0.29 のみ） |
    | `bash scripts/verify.sh macos` | 環境で実行不可（Linux。wrapper が拒否） |
    | macOS の 6 フィルタを Linux で wrapper の `run_check` に通す | 全 PASS（process_group 1、terminate_ 4、socket 8、hello 5、autostart 3、tui:: 256 executed） |
- 2026-11-26 M3 Task 2（Linux、固定ツールチェーン 1.99.0）:
  - `cargo build --locked -p dgc-cli` → `target/debug/dgc --help` と全サブコマンドの help で README のコマンド・フラグを照合。
    `cargo install --debug --locked --path crates/dgc-cli --root <scratch>` → `dgc` だけがインストールされることを確認。
  - workflow.md の集中テストのコマンドを実行: `-p dgc-protocol` 36、`-p dgc-daemon --test store --test events_log` 35、`--test workspace` 37、
    `--test agent_manager --test claude_adapter --test codex_adapter` 61、`--test process --test server` 37、`-p dgc-client` 13、
    `-p dgc-cli --test agent_cli --test daemon_cli --test workspace_cli` 22（いずれも全 PASS）。
  - `rg -n "dgc-next|src/llm|src/features|FsTools|MutationReceipt|JobManager|RepoMap|dgc exec|mcp-server" README.md AGENTS.md docs/ai
    docs/orchestrator/m2-tui-screen.txt`（未追跡の `docs/ai/audit-2026-10-29.md` を除く）→ 0 件。
  - `bash scripts/verify.sh guidance` → PASS（ガイダンス検査、sensitive-logging、40 tests）。Rust のコードとビルド入力は変更していない。
- 2026-11-26 M3 Task 2 修正 1（Linux）:
  - RED: 作業ツリーに未追跡の `.claude/skills/foo/SKILL.md` を置くと `bash scripts/verify.sh guidance` が
    `.claude/skills/foo: stale or noncanonical skill entry` で FAIL（`git status` では `?? .claude/`、追跡され得る状態）。
    先に追加したテスト `test_local_agent_host_skills_are_not_guidance` は `check_guidance` 未定義で ERROR。
  - GREEN: unittest 37 tests OK（skills のテスト 4 件を削除、新テスト 1 件）。同じ `.claude/skills/foo` を置いて `verify.sh guidance`
    PASS（37 tests）、`git status --ignored` は `!! .claude/`。確認後に `.claude/` を削除。
  - `rg -n "dgc-next|src/llm|src/features|FsTools|MutationReceipt|JobManager|RepoMap|dgc exec|mcp-server" README.md AGENTS.md docs/ai`
    （未追跡の audit を除く）→ 0 件。`m2-tui-screen.txt` の `dgc-next` はキャプチャ 16 の画面行 1 件だけ。
- 2026-11-26 M3 Task 3 受け入れ（Linux、固定ツールチェーン 1.99.0、実プロバイダなし）:
  - インストール: `cargo install --locked --path crates/dgc-cli --root <scratch>/inst` → `inst/bin/dgc` のみ、`dgc --version` は `dgc 0.1.0`。
    `cargo build --locked -p dgc-daemon` で `dgc-fake-provider` を用意。`config.toml` の `claude_path` / `codex_path` を、`--version` に
    テスト済みバージョンを返し、それ以外は偽プロバイダ（`claude_turn.jsonl` / `codex_untrusted.jsonl`）を exec するラッパーに向けた。
    `DGC_STATE_ROOT` はソケット長の上限のため短いシンボリックリンク `/tmp/dgcm3`（スクラッチ配下を指す）。
  - README のクイックスタート順に、インストールした `dgc` で確認（使い捨ての git リポジトリ。初回コミットは `-c user.name=dgc -c user.email=dgc@example.invalid`）:
    | 手順 | 結果 |
    |---|---|
    | `dgc daemon status`（自動起動） | PASS: pid、protocol_version 1、claude 2.1.332 (tested)、codex 0.172.0 (tested)、終了コード 0 |
    | `dgc project add <repo>` | PASS: `prj_…  repo  <path>` を表示 |
    | `dgc workspace create --project … --worktree --branch dgc-m3` | PASS: `<worktrees>/repo-<id>/dgc-m3` に worktree、`ls` に active で出る |
    | `dgc run --follow "ping the fake"` | PASS: starting → user → turn started → running → assistant: pong → turn completed → idle、終了コード 0 |
    | `dgc ls` / `dgc logs ID` / `dgc show ID` | PASS: `idle` の 1 行、7 件のイベントが `--follow` の出力と一致、provider_session が出る |
    | `dgc workspace archive`（クリーン） | PASS: archived、worktree ディレクトリとブランチ dgc-m3 が消え、エージェントも archived |
    | `dgc workspace archive`（未追跡ファイルあり、2 つ目の worktree） | PASS（拒否）: 終了コード 1、`has uncommitted changes; archiving would delete them`（-32009）。ファイルとブランチ、エージェントは idle のまま |
    | `dgc workspace archive --force` | PASS: archived、worktree とブランチが消え、`git worktree list` はメインだけ |
    | `dgc tui`（tmux 専用ソケット、120x30） | PASS: 左ペインに `▾ repo` / `▾ ws local` / `○ claude tui check`（archived の worktree は出ない）、右に stream、下に入力欄、フッター `daemon ok · 1 agents · 0 waiting permission`。`q` で終了コード 0 |
    | `dgc daemon stop` | PASS: `daemon stopped`、終了コード 0 |
    終了後 `tmux kill-server`、`pgrep -af 'dgc daemon|dgc-fake|dgcm3|tmux'` は 0 件（残存プロセスなし、tmux ソケットファイルも削除）。dgc のバグは見つからなかった。
  - 全検証（固定ツールチェーン 1.99.0）:
    | コマンド | 結果 |
    |---|---|
    | `cargo fmt --all --check` | PASS |
    | `bash scripts/verify.sh rust`（標準設定） | PASS（fmt、clippy `-D warnings`、test 576 executed） |
    | `bash scripts/verify.sh guidance` | PASS（ガイダンス検査、sensitive-logging、37 tests） |
    | `bash scripts/verify.sh msrv` | PASS（rustup 1.94.0、`cargo check --locked --workspace --all-targets --all-features`） |
    | `bash scripts/verify.sh tui-deps` | PASS |
    | `bash scripts/verify.sh macos` | 環境で実行不可（Linux。Task 1 で 6 フィルタを Linux で確認済み） |

- 2026-11-26 M3 最終レビュー修正（Linux、固定ツールチェーン 1.99.0）。スクリプトの変更はテストを先に書いて RED を確認:
  `python3 -m unittest discover -s scripts/tests` が 1 FAIL（`test_macos_mode_runs_the_whole_workspace_test_suite`: 旧コードは 6 フィルタ）
  + 12 ERROR（`EXPECTED_CRATES` が未定義）。GREEN: 40 tests OK。
  | コマンド | 結果 |
  |---|---|
  | 旧 CLI 集中コマンド（`CARGO_TARGET_DIR` を空の scratch にして `cargo test --locked -p dgc-cli --test agent_cli`） | 再現: `dgc-fake-provider is missing`（`tests/common/mod.rs:22` の panic） |
  | 新コマンド（空の scratch target で `cargo build --locked -p dgc-daemon --bin dgc-fake-provider && cargo test --locked -p dgc-cli --test agent_cli --test daemon_cli --test workspace_cli`） | PASS（12 + 7 + 3 passed） |
  | `python3 scripts/check-sensitive-logging.py` | PASS（41 files、既存コードに誤検出なし） |
  | `bash scripts/verify.sh guidance` | PASS（40 tests） |
  | `TMPDIR=/var/tmp bash scripts/verify.sh rust` | PASS（fmt、clippy `-D warnings`、test 576 executed） |
  | `bash scripts/verify.sh macos` | 環境で実行不可（Linux）。コマンドは `cargo test --locked --workspace` に変更（unit test で確認） |

## M2 受け入れで見つかった小さな指摘（2026-11-26、修正は未着手）

M2 最終レビューで扱いを決める。（3 件目は修正ラウンド 1 でバグと裁定され修正済み。他は使い勝手の指摘。）
- （修正済み、M2 最終レビュー修正 3）実行中のエージェントへ queue で送ってもストリームにもステータスにも何も出ない（`RpcDone` の `queued` 結果を表示していない。
  メッセージは配送時か破棄時にイベントとして出る）。受理されたか分からない。ステータスバーに送信結果を出すようにした。
- （修正済み、修正ラウンド 1）新規エージェントのフォームのプロンプト欄が `(optional)` と表示されるのに空だと `prompt is required` だった。表示を `(required)` に変更。
- （修正済み、修正ラウンド 1。バグと裁定）入力欄の書きかけの文字がエージェント間で共有され、A 宛てに打った文字が B を選んで `i` → Enter で B に送られた
  （steer/interrupt モードでは B を中断する）。下書きをエージェントごとに持つようにした。
- （仕様どおり）`End` で末尾に跳んでも、先頭の画面と重なって全行が一度見えていれば `a`/`s` が有効になる。厳格な経路の検証は単体テストにある。

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
- （M3 で解決）M0 最終レビュー由来: `Cargo.toml` の `[workspace.package].rust-version` とルート `[package].rust-version`
  の一致を検査していない。M3 でルートの `[package]` がなくなり（仮想ワークスペース）、MSRV は `[workspace.package]` の 1 か所になった。
  `check_ci` はその値と CI の msrv ジョブ、rust-toolchain を照合する。
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
- Task 3: minor (deferred): corrupt-line warn repeats per read/open; open scans whole file（M4 Task 1 で解消）; 0600 not asserted on events.1.jsonl, pre-existing looser mode not tightened; oversized test doesn't assert fresh file
- Task 3: minor (deferred): size undercount after double fault (truncate fails then later write ok then fail+truncate ok); no test drives failure paths through append end to end
- Task 4: minor (deferred): line.clear() keeps 16 MiB capacity after a huge line; tokio macros feature only needed by tests; wait_gone counts zombies as alive (container pid1 flake risk)
- Task 5: minor (deferred): writer task outside JoinSet leaks after drain timeout; shutdown ignored if response send fails; no pre-hello timeout / connection cap; second_hello test doesn't assert INVALID_STATE code; responses_wait test relies on 50ms sleep; JoinError from connection task silently dropped
- Task 6: minor (deferred): test guard may miss a still-booting daemon after a failed run; VersionMismatch after spawn reported as SpawnTimeout; Client::call has no overall timeout (wedged daemon hangs CLI); dgc-cli uses tokio::sync without declaring `sync` feature (unification); missing tests (symlinked daemon.log, log 0600, id-0 error path, pending-notification overflow); pre-existing daemon.log mode not tightened; stop's lock probe can make a concurrent autostart lose
- Task 7: minor (deferred): missing project dir leaves workspace un-archivable → project never removable (consider re-grade at final review); worktree_requires_a_git_repository can't distinguish non-repo vs subdir and manager git inherits global config/hooks in tests; git timeout kills only git not its descendants; failed branch delete only warns (no response field)
- Task 7: minor (deferred): `git worktree list -z` needs git >= 2.36 (no fallback/doc; fails closed); locked worktree with missing dir makes archive fail permanently; lib test name says prefix but checks suffix
- Task 8: minor (deferred, consider re-grade at final review): unsupervised actor panic leaves live entry + leaked pending permissions until restart (adapter panics land here); queued items stall after interrupt; possible double send if save_queue fails after send; swallowed put_agent error in create cleanup; Shared::logs never evicts (fd growth; M2 Task 1 で解消); start failure drops initial prompt（M4 Task 1 で解消）; AGENTS_DIR duplicates Store constant; test gaps (race paths, failed queue-head send, archive during WaitingPermission, Exited while Idle, futures Send check missing interrupt/respond, slow_subscriber asserts page clamp only)
- Task 8: minor (deferred): biased select can starve commands under a flooding provider (fairness cap suggested); drain loop has no yield/bound (use len snapshot); no test for Exited during drain; caller gets timeout error before stored state flips (≤5s)
- Task 9: minor (deferred): Codex command deny never recorded — Deny mapping on commands unexercised (ruling: cancel when decline not offered)
- Task 10: minor (deferred): tests hard-code sorted-key output (preserve_order unification risk); uncaptured ${name} in expect matches literally (should exit 2); capture line extra keys ignored; stdin read error treated as EOF; lossy UTF-8 on input
- Task 11: minor (deferred): deny sent before interrupt success known (manager leaves pending on interrupt Protocol error); interrupt internal bound can exceed manager 30s with many pending permissions; full event channel blocks control_response; plan's verify.sh filter name
- Task 11: minor (deferred, consider re-grade at final review): idle-skip in interrupt() races reader delivery — TurnCompleted can arrive after interrupt returns (contract breach, sub-µs window, affects Steer→interrupt_and_send); init-reset relies on unrecorded "result1 before init2" ordering
- Task 12: minor (deferred, consider re-grade at final review): interrupt failing with non-32600 error leaves agent Running (TurnCompleted already dropped); untested race paths (interrupt completed-then-32600, usage correlation by turn id) — testable with fake; chunked message with empty remainder emits no final:true; deltas/approvals without turnId stored under "" never cleared; duplicated process-session plumbing between claude.rs and codex.rs (codex.rs 1187 lines); timing-dependent quiet-period drains in tests
- Task 12: minor (deferred): steer fallback logs only debug (could hide a malformed steer); per-turn usage correlation untested
- Task 13: minor (deferred): lagged next_seq imprecise for live-only subscriptions; concurrent resume can leave an agent running in an archived workspace; --version probe buffers unbounded stdout and kills only direct child (reuse process-group kill); missing tests (unsubscribe stops delivery, all-agents subscription, follow exit 1 on Failed / WaitingPermission continues); resolve.rs ETXTBSY flake risk; idle subscription connections accumulate after lag
- Task 13: minor (deferred): non-autostart CLI calls during daemon startup block up to 15s then HelloTimeout; connect_or_spawn worst case can exceed SPAWN_WAIT; bind→chmod umask window (mitigated by 0700 dir; umask(0o177) around bind would harden)
- Task 14: minor (deferred): long DGC_STATE_ROOT → opaque SUN_LEN error (under() has no length check); `logs | head` panics on broken pipe; Claude final assistant_text after interrupt recorded after state idle; no TurnStarted event emitted though README lists it (M2 Task 1 で解消); idle sends print `user (queue)`; empty <repo>-<id> dir left after workspace archive; run --follow exit code not captured in acceptance
- Task 14: minor (deferred): README lists `turn started` (never emitted; M2 Task 1 で emit するようになった) and omits `assistant (partial)` line; README socket fallback wording; systemd example lacks cgroup-stop note; C/D evidence summary-only, no-TCP check covers daemon pid only
- Task 14: minor (deferred): README "does not log prompt text" true for daemon.log but events.jsonl stores prompts too — wording should say so
- Final: minor (deferred): startup error loses stderr tail end when >4 KiB; restart loses pending permissions without permission_resolved{cancelled}; Claude AllowForSession recorded as allow_for_session but sent as allow; daemon stop STOP_WAIT 5s < shutdown ~7.5s; Shared::logs never evicts (M2 Task 1 で解消); Task 11 idle-skip race; Task 12 non-32600 interrupt failure leaves Running; start failure drops initial prompt（M4 Task 1 で解消）
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

## M2 レビューで保留した指摘と判断（2026-11-26、実行台帳から移記）

M2 はタスクごとのレビューと全体の最終レビュー（修正の再レビューまで）を通過した。以下は保留した Minor 指摘と、コントローラの判断の全量。

### 保留した Minor 指摘

- Task 1: minor (deferred): steer-fallback TurnStarted untested; adjacency asserted only for initial prompt; LogCache eviction tests thin (held handle skip, >64 live, mark_not_live, stored-archive forget); EventLog::open (full scan) under global logs mutex on cold reopen stalls emits（M4 Task 1 で解消）; evict/mark_not_live O(n) per call
- Task 2: minor (deferred): pending-live overflow at 2000 drops all (restart load instead)（M4 Task 1 で解消）; single `unsent` slot can restore a possibly-sent message (double send); create-dialog success after Esc still jumps selection; Enter toggles collapse (brief says expand) — align README; selection fallback tests only next sibling; Tree::rows / agents_in_order duplicate filters
- Task 2: minor (deferred, re-grade at final): failed-send text restores into another agent's input (condition should be input_target == agent_id) — wrong-destination class; overlapping snapshot requests can still erase a fresh permission (count outstanding requests)（M4 Task 1 で解消）; permission id uniqueness assumption undocumented
- Task 3: minor (deferred): create-dialog WARNING pushed off in tiny terminals (Enter still works) — move it next to mode field; status bar error can push permission hint off-screen; no test for tree vertical overflow
- Task 3: minor (deferred): a single grapheme wider than 69 cols makes the dialog too_small at any size with an un-actionable "enlarge" message (fails safe); look-alike arrows (↳ ⤷ ⮡) not escaped; halfwidth render test doesn't guard N1 (layout test does)
- Task 4: minor (deferred): subtask panic treated as connection close (terminal restored by hook but loop keeps drawing on main screen) — treat JoinError::is_panic as fatal; input thread I/O error detail lost; draw stops after MAX_REDRAWS without final redraw; fixed sleeps in tests (1.5 s quit, 500 ms resubscribe); no backend-level test that Resubscribe closes old connection; effects queued just before quit dropped silently
- Task 4: minor (deferred): incompatible daemon → reconnect/snapshot-fail loop ~4 calls/s; 15 s timeout also hits slow healthy calls
- Task 5: minor (deferred, re-grade at final): input draft shared across agents → text typed for A can be sent to B (wrong-destination class); new-agent form labels prompt "(optional)" but requires it; queue send to running agent gives no feedback
- Task 5: minor (deferred): queue send to running agent gives no feedback (documented); failed send text dropped if user already typed new text; screen capture 1 caption mentions a message not captured; disconnected status bar truncates key hints
- Task 5: minor (deferred): drafts map never pruned (archived/removed agents' drafts stay); no tests for failed-send restore after selection change / draft kept when target archived
- Final: minor (deferred): reconnect race can skip part of the stream until reselect (display only); send mode global & sticky (interrupt persists across agents)（M4 Task 1 で解消）; history loaded from seq 0 each selection (O(n²) read_page, plan tail paging in protocol work)（M4 Task 1 で解消）; empty summary still allows a/s; auto-approve list duplicated vs daemon; single unsent slot; stream lines bounded by count not bytes
- Final: minor (deferred): periodic snapshots can pile up behind a slow daemon (pending expires at 5 s; merge queued LoadSnapshot)（M4 Task 1 で解消）; empty-tree hint shows before first snapshot / while disconnected; two creates in flight share RequestTag (first success can close the second form; error resets submitting → duplicate create); terminals without bracketed paste still replay keys (document); >1 MiB paste exceeds message limit (unchecked)

### コントローラの判断（Ruling）

- Ruling: none needed at pre-flight
- Task 1: Ruling: steer that falls back to interrupt_and_send records UserMessage{Steer} + TurnStarted — it really starts a new turn — cost if wrong: one extra marker in logs
- Task 2: dispatched implementer aead8da436e1389b1 (sonnet), BASE 10d07ff; Ruling: tui/mod.rs carries #![allow(dead_code)] until Task 4 wires it — keeps clippy -D warnings clean for an unwired module — cost if wrong: Task 4 must remove it
- Task 2: Ruling: accept post-hoc RED instead of discarding the code — the tests were shown to fail against a stub and the reviewer judges their strength; redoing costs a full re-implementation — cost if wrong: some tests may encode the implementation rather than the requirement
- Task 2: Ruling: permission dialog never auto-opens in Input focus (indicator instead) or over another dialog; opens via `p` or auto only in Tree focus with empty editor; decision keys ignored for 2 Ticks (≥500 ms) after any open/replacement — prevents typed text from approving unread commands — cost if wrong: half-second delay before answering
- Task 2: Ruling: when the selected agent disappears, Input focus stays with a "no target" state, text kept, no auto dialogs — cost if wrong: user must re-select before sending
- Task 2: Ruling: history loads carry a generation in Effect::LoadHistory / AppEvent::History; failed LoadHistory reported as History{events:[],done:true,generation} + status error — interface change carried to Task 4 — cost if wrong: none
- Task 3: Ruling: shared pure permission_layout used by render+update; Dialog::Permission gets scroll + seen_end; a/s only when armed && seen_end (d/Esc always); header 1 line, footer reserved first; "window too small" → a/s disabled; soft-wrap rows marked ↪ — the user must be able to read every line before allowing — cost if wrong: extra scrolling for long requests
- Task 3: Ruling: grapheme-width measurement folded into the fix (dropped graphemes alter the approved text) though reviewer graded Minor
- Task 3: Ruling: protocol 4 KiB summary cap is a known limitation — dialog shows a red "shortened by dgc — full command not shown" line; a/s still allowed after seen_end — cost if wrong: a >4 KiB request can be approved without full text (future: raise cap / show raw)
- Task 3: Ruling: accept stricter seen_end = every summary row has been on screen (End/Home only peek) — literal "last line visible" would let End enable a/s with the middle unread — cost if wrong: more scrolling
- Task 3: Ruling: fold N3 (escape U+21AA in summaries) into round 2 — same spoofing class as finding 3 — cost if wrong: none
- Task 4: dispatched implementer a596bdfa18caecc0c (opus), BASE 6f52669; Ruling: reconnects use plain connect (no respawn after user's daemon stop); first connect uses connect_or_spawn — cost if wrong: TUI won't auto-restart a stopped daemon
- Task 4: Ruling: fold Minor 1 (size change re-arms permission dialog) — same "approve unseen rows" class as Task 2/3 rulings — cost if wrong: extra 0.75 s after resize
- Task 4: Ruling: fold Minor 3 (15 s RPC timeout → Stop::Lost + RpcDone Err) — a wedged daemon otherwise freezes permission answers silently — cost if wrong: slow daemon ops (>15 s) reported as disconnect
- Task 5: Ruling: fix finding 3 in code (per-agent drafts + target shown in input title) rather than only disclose — wrong-destination sends can interrupt another agent — cost if wrong: small state-model change late in M2
- Task 5: Ruling: fold one-word "(optional)"→"(required)" label and README wording fixes into the round
- Final: Ruling: paste inserts into the focused editor only, newlines/tabs → spaces, ignored outside text fields — cost if wrong: multi-line messages flattened
- Final: Ruling: periodic snapshot every 5 s + R refresh + empty-tree hint — cost if wrong: small periodic load
- Final: Ruling: per-call timeouts (reads 15 s, mutating 90 s) folded in — prevents duplicate sends on slow healthy calls
- Final: Ruling: create success after Esc does not move selection (folded, wrong-target class)
- Final: fix wave done 8b24f22..b8795d7 (7 commits); Ruling: item 6 applied to workspace.create too (same class) — accepted

- 2026-11-26 M2 最終検証（固定ツールチェーン 1.99.0）: fmt / clippy -D warnings / ワークスペース全体のテスト（直列、TMPDIR=/var/tmp）3021 passed / 0 failed / guidance / tui-deps / msrv すべて PASS。

## M3 レビューで保留した指摘と判断（2026-11-26、実行台帳から移記）

M3 はタスクごとのレビューと全体の最終レビュー（修正の再レビューまで）を通過した。以下は保留した Minor 指摘と、コントローラの判断の全量。

### 保留した Minor 指摘

- Task 1: minor (deferred): sensitive-logging fails only if ALL scope entries missing (per-entry check stricter); scope could be crates/*/src at zero cost; macOS filters are name substrings covering ~5/13 process tests (run whole --test binaries instead); git-unavailable fallback untested; unstaged new docs skipped locally
- Task 2: minor (deferred): README "about 0.75 s" is really 0.5–0.75 s; AGENTS.md 133 lines vs ~120 target (accepted)
- Task 2: minor (deferred, consider at final): README:334 says prompts/output stored in events.jsonl (also queue.json + title); README:27 omits interrupt/shutdown cancel and provider-mode note
- Final: minor (deferred): README/AGENTS say daemon stop cancels pending requests "(answered deny or cancel)" — on stop the provider is terminated, only interrupt answers it (safety claim unaffected); workflow.md tui-deps sentence merged into macOS line (cosmetic); macOS full-workspace run only exercised in CI

### コントローラの判断（Ruling）

- Ruling: none at pre-flight
- Task 2: implementer DONE_WITH_CONCERNS 8b4b24d; Ruling: captured screen content must not be edited — restore capture 16's original 'dgc-next daemon start' line, keep the predates-rename note, exclude captures from the stale-name rg — records must reflect what was on screen — cost if wrong: one stale name in a historical capture
- Task 2: Ruling: fold Minor 3 (credentials wording: env inheritance; "not sent over the socket") and Minor 4 (remove .gitignore skills un-ignores — personal skills could become trackable — and the skills check/tests in check-agent-guidance) — security-wording and guidance debt left by this task — cost if wrong: none
- Final: Ruling: macOS mode runs the full workspace test suite (drop name filters) — platform coverage over speed — cost if wrong: longer macOS CI job
- Final: Ruling: sensitive-logging scope = all crate src dirs via glob, each must exist — reviewer found zero false positives — cost if wrong: future false positives need targeted exemptions

## M4 レビューで保留した指摘と判断（2026-11-27、実行台帳から移記）

M4 はタスクごとのレビューと全体の最終レビュー（修正 3be7f17 の再レビューまで）を通過した。以下は保留した Minor 指摘と、コントローラの判断の全量。台帳の行はそのまま転記したので、最終レビューの修正 3be7f17 で解消した項目（Claude の追加使用、プロトコル v2、`<synthetic>` 行、`last_answer` の区切り、プレビュー時の可用性確認、README の注記）も含む。

### 保留した Minor 指摘

- Task 1: minor (deferred): failed initial tail load clears initial_tail → later reconnect pages from seq 0 (efficiency); R / unknown-agent / decode-failure snapshot requests dropped while one is pending (set refresh_after_pending); send mode reset overwrites a mode changed during the in-flight RPC; old daemon ignores `tail` (no protocol bump) → document daemon restart; no test pinning a newline exactly at a 64 KiB boundary; initial prompt lost if start succeeds but first send_user fails
- Task 2: minor (deferred): rate_limited_detail_carries_no_account_data only in Codex test file (Claude covered by exact equality)
- Task 2: minor (deferred): no test for willRetry:true followed by terminal error/snapshot in same turn; float/missing usedPercent untested (treated as not blocked); empty reason stored as empty token
- Task 3: minor (deferred): empty/whitespace profile model/notes not rejected (CLI passes --model "" while TUI drops it); TUI model field logic overcomplicated; picker shows (none) while loading/no profiles and never shows notes; no test for safe profile + explicit auto-approve mode
- Task 4: minor (deferred): .gitattributes filter.clean/process can run during handoff git status/diff (same exposure as existing workspace git; user trusts own repo); kill_on_drop kills only direct child; recent-3 may be <3 after removing first instruction; profile load error lingers in form.error; ⇄ only from selected agent's history after TUI restart; Esc disabled while submitting (≤90 s); confirmation doesn't require reading preview to end (consider for auto-approve modes)
- Task 4: minor (deferred): every <synthetic> Claude row → Error (not only error rows); first instruction dropped silently if budget < marker (title+path near 32 KiB); invalid-model error shown twice; Codex non-fatal mid-turn Error (unsupported provider request) makes last_answer skip a completed turn; slow source-log read can block that source's own resume/finish
- Task 5: minor (deferred): README doesn't state ⇄ needs selecting an agent whose limit predates TUI start (only PROGRESS 検証記録 does); privacy paragraph could say plainly that prior instructions/output are included — read the dry-run; CLI-side evidence only in report/scratch
- final-fix 3be7f17 (Codex) re-review a019036ab379d56ee: All findings addressed, no new Critical/Important. minor (deferred): daemon stop SIGTERM fallback TOCTOU between pid read / flock probe / kill (needs PID reuse in ms; pidfd_open+pidfd_send_signal would harden); no pid != getpid() check; old daemon exiting by itself between mismatch and probe reports 'no daemon holds the instance lock'; stop test checks try_wait immediately; existing lock_free probe via InstanceLock::acquire creates/truncates daemon.lock and writes the CLI pid
- Final review: minor (deferred): Codex RateLimitSnapshot has limitId/limitName (per-model buckets) — a reached type on a bucket the agent is not using still raises RateLimited; record limitId in a debug log or accept the risk in the fixtures README
- Final review: minor (deferred): TUI handoff dialog — after a failed preview Enter does nothing (Esc and H again); let Enter or a key re-request the preview
- Final review: minor (deferred): `logs --tail 0 --follow` on an empty log replays from 0 (harmless)
- Final review: minor (deferred, M5 candidate): persist the last rate-limit state on the agent record so ⇄ survives TUI restarts without selecting the agent
- Final review: minor (deferred): some focused tests could resolve the host's claude/codex from PATH and run only `--version` (the fix wave made the CLI preview and TUI re-confirm tests use the fake Codex explicitly); keep tests independent of the host PATH

### コントローラの判断（Ruling）

- Ruling: none at pre-flight
- Task 2: Ruling: Claude limited only on status "rejected" or unknown (not allowed/allowed_warning); plan amended — reviewer cited CLI docs — cost if wrong: an unknown future "limited" status variant counts as reached (fails toward suggesting)
- Task 2: Ruling: fold Minors — reason normalized to [A-Za-z0-9_]{≤48}; Codex error with willRetry:true ignored; resets_at prefers windows at ≥100% — all three prevent wrong/unsafe suggestions — cost if wrong: none
- Task 4: Ruling: expected_prompt equality check (daemon rebuilds; mismatch → INVALID_STATE handoff_changed; TUI re-previews); daemon never uses client text — "approve exactly what is sent" — cost if wrong: an extra re-confirm when state changes
- Task 4: Ruling: last_answer skips turns with RateLimited/Error; Claude is_api_error_message rows → Error (Task 2 scope, folded) — cost if wrong: none
- Task 4: Ruling: first instruction cut at 16 KiB with marker (plan amended; full text via log path) — cost if wrong: long specs truncated in prompt (full log referenced)
- Task 4: Ruling: fold git color off, cold-log read off the global mutex (spawn_blocking), flaky restart test helper (fail fast + 10 s)
- Final review: Ruling: protocol v2; client maps both mismatch shapes to VersionMismatch with a `dgc daemon stop` hint; `daemon stop` on mismatch sends SIGTERM to the pid in daemon.lock only while another process holds the flock — a daemon left over from an older install must fail loudly and stay stoppable — cost if wrong: a stop right after a PID reuse could signal a same-UID process (ms window)
- Final review: Ruling: fold `<synthetic>` row narrowing, `last_answer` cut at the first TurnCompleted, dry-run adapter check and three README notes — each prevents a wrong suggestion or a confusing preview — cost if wrong: none
