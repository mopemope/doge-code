# dgc オーケストレーター化 設計書

- 日付: 2026-11-25
- 状態: 承認済み（2026-11-25）
- 進捗: [docs/orchestrator/PROGRESS.md](../../orchestrator/PROGRESS.md)（実装計画承認後に作成）

## 1. 意図と理由

dgc を「LLM API を直接叩く自前エージェント」から、「既存の Claude Code / Codex CLI
をワーカーとして起動・監督するオーケストレーター」に作り変える。機能面の参考は
Paseo（デーモン、ワークスペース、エージェント、追送プロンプト、プロファイル、
スケジュール）。

理由（ユーザー発言）: 他者が作ったオーケストレーター（例: t3）には信頼性・
セキュリティ上の懸念がある。エージェント群にファイル書き込み・コマンド実行・
認証情報への広い権限を持たせる層は、自分で監査・管理できるものにしたい。
自作の方が将来のリスクが少ない。

この理由から導く設計原則（設計者判断）:

1. **小さく監査可能**: 依存 crate を最小化し、各 crate の責務を狭くする。
2. **既定で閉じる**: ネットワーク非公開、権限緩和は明示設定時のみ。
3. **認証情報に触れない**: LLM の認証は各 CLI が自分で持つ。dgc は API キーや
   OAuth トークンを読まない・保存しない・中継しない。
4. **CLI の上流変更に備える**: プロバイダ CLI のプロトコルはアダプタ層に閉じ込め、
   動作確認したバージョンを記録する。

## 2. ゴール / 非ゴール / アンチゴール

### ゴール（フェーズ A: ローカル専用）

- 常駐デーモンが Claude Code / Codex のエージェントを複数同時に管理する。
  TUI/CLI を閉じてもエージェントは動き続ける。
- プロジェクト登録、ワークスペース（ローカルディレクトリ / git worktree）作成。
- エージェント作成、追送プロンプト（queue / steer / interrupt）、中断、アーカイブ。
- エージェントの権限要求（コマンド実行、ファイル変更など）をユーザーへ中継し、
  許可/拒否を返す。
- CLI と TUI からの操作。CLI は `--json` で機械可読出力。
- プロファイル、スケジュール、heartbeat、ワークスペーススクリプト（M4）。
- エージェントが dgc 経由でサブエージェントを起動できる MCP（M5）。

### 非ゴール（今回はやらない）

- フェーズ B（リモート操作: スマホ / 別 PC）と複数ホスト連携。A 完成後に別設計。
  ただし A のソケットプロトコルは、B で SSH トンネル等の上に載せられる形にする。
- 直接 LLM API を叩く機能の維持。旧 `src/` の LLM 実行系は最終的に削除する。
- Claude / Codex 以外のプロバイダ対応（アダプタ境界は作るが実装しない）。
- GUI / Web UI / モバイルアプリ。

### アンチゴール（動いても失敗とみなすもの）

- dgc がネットワークポートを開く。
- dgc が LLM の認証情報を読む、ログやスナップショットに書き出す。
- 別ユーザーがデーモンソケットに接続できる。
- 権限要求を dgc が勝手に自動承認する（ユーザーが明示したプロファイル設定を除く）。
- 外部の未監査コード（npm パッケージ、スクリプト）を dgc が取得・実行する。
- 移行中に旧 dgc が使えなくなる期間が生じる。

## 3. 制約

- Rust Edition 2024、MSRV は `Cargo.toml` の `rust-version`（現在 1.94）。
- 対象 OS: Linux と macOS（Unix ソケット前提）。Windows は対象外。
- 動作確認時点の CLI: Claude Code 2.1.332、codex-cli 0.172.0。
- 既存の検証ラッパー `scripts/verify.sh` と CI（`.github/workflows/ci.yml`）を使い続ける。
- 開発は Claude Code と Codex を途中で交代しながら進める可能性がある（§11）。

## 4. 検討した方式

| 案 | 内容 | 評価 |
|---|---|---|
| 1. 既存 crate を段階置換 | `src/` に追加し旧系を少しずつ削除 | 移行中は新旧混在、重い依存（sea-orm、reqwest、tree-sitter 群）が長く残り監査性に反する |
| **2. 同一リポジトリで workspace 再構成（採用）** | `crates/` に新構成を作り、必要部品だけ移植。完成後に旧 `src/` を一括削除 | 依存を最初から最小化でき境界も明確。旧 dgc は移行完了まで動く |
| 3. 新リポジトリ | ゼロから | 「このプロジェクトを作り変える」依頼から外れ、履歴と CI を失う |

ユーザーが案 2 を選択。運用方式はフェーズ A（ローカル専用）を先に完成させ、その後
B に進むことをユーザーが決定。

## 5. 全体構成

```
┌──────────────┐  Unix socket (0600, 同一UIDのみ)   ┌──────────────────────────┐
│ dgc CLI / TUI │ ─── 改行区切り JSON-RPC 2.0 ───── │ dgc daemon               │
└──────────────┘     （要求/応答 + 購読イベント）     │  ├ Store（JSON, 0600）    │
                                                       │  ├ WorkspaceManager       │
                                                       │  ├ AgentManager           │
                                                       │  │   ├ ClaudeAdapter ──── stdio ── claude -p (stream-json)
                                                       │  │   └ CodexAdapter  ──── stdio ── codex app-server
                                                       │  └ Scheduler（M4）        │
                                                       └──────────────────────────┘
```

### 5.1 Crate 構成

ルート `Cargo.toml` を Cargo workspace にする。旧パッケージ（ルートの `doge-code`）は
削除まで workspace メンバーとして残す。

| crate | 種別 | 責務 | 主な依存 |
|---|---|---|---|
| `crates/dgc-protocol` | lib | ソケットプロトコルの型、エラーコード、正規化イベント型。I/O を持たない | serde, serde_json |
| `crates/dgc-daemon` | lib | Store、ワークスペース、エージェント管理、プロバイダアダプタ、ソケットサーバー、プロセス管理 | tokio, serde, thiserror, tracing, uuid, libc |
| `crates/dgc-client` | lib | ソケット接続、要求送信、イベント購読 | tokio, dgc-protocol |
| `crates/dgc-cli` | bin | CLI サブコマンド、`daemon run`、TUI（M3） | clap, tracing-subscriber, ratatui, crossterm |

- 新しい依存の追加は理由を PROGRESS.md の「決定事項」に記録する。
- 旧 `src/` からの移植候補: `src/execution/lifecycle.rs`（プロセスグループ終了）、
  `src/features/worktree_manager.rs`（worktree 作成と安全な削除）、`src/jobs/`
  の考え方。コピーして必要部分だけ残し、旧コードへの依存は作らない。

### 5.2 デーモンの起動（設計者判断）

- CLI はソケットに接続できなければ、その場でデーモンを起動してから接続する
  （`daemon run` を子として切り離して起動）。明示的な `daemon start` / `stop` も持つ。
- systemd / launchd のユーザーユニット例はドキュメントで示すだけにし、既定にはしない
  （起動元シェルと環境が変わると CLI が認証できない事故が起きやすいため）。
- 単一インスタンス: 状態ディレクトリのロックファイルで二重起動を防ぐ。

### 5.3 バイナリ名

移行期間中、新バイナリは `dgc-next`、旧バイナリは `dgc` のまま。切替（M3）で
新バイナリを `dgc` に改名し旧 `src/` を削除する。

## 6. データモデルと保存

保存先（XDG に従う。macOS も同じ環境変数を優先し、未設定時は `dirs` の既定）:

| 種類 | 場所 |
|---|---|
| ソケット | `$XDG_RUNTIME_DIR/dgc/daemon.sock`（無ければ `$XDG_STATE_HOME/dgc/run/`）。ディレクトリ 0700、ソケット 0600 |
| 状態 | `$XDG_STATE_HOME/dgc/`（projects/、workspaces/、agents/、daemon.log） |
| 設定 | `$XDG_CONFIG_HOME/dgc/config.toml`（プロバイダ実行パス、プロファイル） |
| 管理 worktree | `$XDG_DATA_HOME/dgc/worktrees/<repo-key>/<slug>` |

- 各エンティティは 1 ファイル 1 JSON。一時ファイルへの書き込み後に rename する
  （atomic replace）。ファイル 0600。
- エージェントのイベント履歴は `agents/<id>/events.jsonl` に追記する。1 ファイル
  上限（初期値 32 MiB）を超えたらローテーションし、上限は設定可能にする。
- スキーマに `version` を持たせ、未知の版は読み込みを拒否してエラーを出す。

エンティティ:

- **Project**: `id`、`path`（正規化済み絶対パス）、`name`、`created_at`。
- **Workspace**: `id`、`project_id`、`isolation`（`local` | `worktree`）、`path`、
  `branch`、`base_ref`（解決済み）、`state`（`active` | `archived`）、`owned`
  （dgc が作った worktree なら true。削除はこれが true の場合だけ）。
- **Agent**: `id`、`workspace_id`、`title`、`provider`（`claude` | `codex`）、
  `model`、`mode`、`provider_session_id`（Claude の session id / Codex の thread id）、
  `state`（`starting` | `idle` | `running` | `waiting_permission` | `stopped` |
  `failed` | `archived`）、`created_at`、`labels`。
- **PendingPermission**: `request_id`、`agent_id`、`kind`（command / file_change /
  tool / other）、`summary`、`raw`（プロバイダ原文、表示用に上限あり）、`created_at`。

## 7. ソケットプロトコル

- 改行区切りの JSON-RPC 2.0。1 メッセージの上限は 1 MiB（超過は接続を切断）。
- 接続直後に `hello`（`protocol_version`、クライアント名）。メジャー版の不一致は拒否。
- 接続受付時に peer credential（Linux: `SO_PEERCRED`、macOS: `getpeereid`）を
  確認し、デーモンと同じ UID 以外は即切断する。
- 主なメソッド（M1）:
  - `project.add` / `project.list` / `project.remove`
  - `workspace.create` / `workspace.list` / `workspace.archive`
  - `agent.create` / `agent.list` / `agent.get` / `agent.send` / `agent.interrupt` /
    `agent.archive` / `agent.resume`
  - `agent.events`（過去イベントのページ取得）
  - `permission.list` / `permission.respond`
  - `subscribe`（エージェント ID 指定または全体。サーバーから `event` 通知が流れる）
  - `daemon.status` / `daemon.shutdown`
- 正規化イベント（`dgc-protocol` で定義、プロバイダ非依存）: `turn_started`、
  `assistant_text`（差分または確定）、`tool_call`、`tool_result`（要約、上限あり）、
  `permission_requested`、`permission_resolved`、`turn_completed`（使用量があれば
  含める）、`rate_limited`、`error`、`state_changed`。各プロバイダの原文は
  デバッグ用に `raw` として任意で保持する（上限あり）。

## 8. プロバイダアダプタ

共通トレイト（概略）:

```rust
trait ProviderAdapter {
    async fn start(&self, spec: AgentSpec) -> Result<AgentHandle>;   // 新規または resume
    async fn send(&self, h: &AgentHandle, msg: UserMessage, behavior: SendBehavior) -> Result<SendOutcome>;
    async fn interrupt(&self, h: &AgentHandle) -> Result<()>;
    async fn respond_permission(&self, h: &AgentHandle, id: &str, decision: Decision) -> Result<()>;
    async fn shutdown(&self, h: AgentHandle) -> Result<()>;
    // イベントは AgentHandle が持つ mpsc から正規化イベントとして流れる
}
```

### 8.1 Claude Code

- 起動: `claude -p --input-format stream-json --output-format stream-json --verbose`
  に、モデル、`--permission-mode`、権限要求をホスト（dgc）へ返す設定、resume 時は
  `--resume <session-id>` を付ける。作業ディレクトリはワークスペースのパス。
- 追送: stdin へ user メッセージを書く。turn 実行中の `queue` は dgc 側で保持し、
  turn 完了後に送る。`steer` は Claude にネイティブ手段が無い場合 `interrupt` に
  フォールバックし、その旨を `SendOutcome` で返す（Paseo と同じ挙動）。
- 権限要求: stream-json の制御メッセージで受け、`permission_requested` に変換する。
- **要確認（実装者）**: 権限要求の制御メッセージ形式、interrupt の制御メッセージ、
  レート制限の通知形式は、インストール済み 2.1.332 で実測し、記録したフィクスチャ
  をテストに使う。ヘルプ上は `--permission-prompts host` と
  `--permission-prompt-tool` が候補。
- **stdio で権限要求を受けられなかった場合（設計者判断）**: `--permission-prompt-tool`
  に dgc の小さな MCP ブリッジ（`dgc-next permission-bridge`、stdio MCP サーバー）を
  指定し、ブリッジがデーモンへ要求を転送する。権限中継なしで Claude を出荷する
  選択はしない。ブリッジは権限要求の転送だけを行い、他の操作はできない。

### 8.2 Codex

- 起動: `codex app-server`（stdio JSON-RPC）。`initialize` → `thread/start`
  （resume 時は `thread/resume`）→ `turn/start`。
- 追送: `steer` は `turn/steer`、`interrupt` は `turn/interrupt`、`queue` は dgc 側で
  保持して turn 完了後に `turn/start`。
- 権限要求: サーバー要求 `item/commandExecution/requestApproval`、
  `item/fileChange/requestApproval`、`item/permissions/requestApproval` などを
  `permission_requested` に変換し、ユーザー応答で JSON-RPC 応答を返す。未対応の
  サーバー要求は拒否応答を返し、`error` イベントを出す。
- レート制限: `account/rateLimits/updated` を `rate_limited` に変換する。
- プロトコル型は `codex app-server generate-json-schema` の出力から、必要な
  メソッドだけを手書きの serde 型にする（スキーマ全体の自動生成はしない）。
  app-server は experimental 扱いなので、対応バージョンを記録し、起動時に
  `codex --version` が既知範囲外なら警告する。

### 8.3 共通事項

- 実行ファイルは設定の絶対パス、または起動時に PATH から解決した絶対パスを
  記録して使う。解決結果とバージョンを `daemon.status` で見せる。
- 子プロセスはプロセスグループで起動し、エージェント停止・デーモン終了時に
  グループごと終了する（`lifecycle.rs` から移植）。
- 子プロセスの環境変数は**デーモン起動時の環境**をそのまま引き継ぐ。クライアントの
  環境変数をソケットで送ることはしない（認証情報を中継しない原則のため）。
  環境を変えたい場合はデーモンを再起動する。`daemon.status` に起動時の PATH と
  解決済み CLI パスを表示する。dgc 自身は認証関連の環境変数を読まない。
- 権限要求は、クライアントが接続していなくても期限なしで保留する（エージェントは
  `waiting_permission` のまま）。次に接続したクライアントが `permission.list` で
  見て応答できる。デーモン再起動で保留中の要求は失われ、エージェントは `stopped`
  になる。
- デーモン再起動時、実行中だったエージェントは `stopped` にし、
  `provider_session_id` を使った `agent.resume` で再開できるようにする。

## 9. セキュリティ要件（受け入れ条件）

1. デーモンはネットワークソケットを一切開かない（テストで確認）。
2. ソケットディレクトリ 0700、ソケット 0600、peer UID 不一致は切断。
   **信頼境界は UID**（ユーザー決定）: dgc が起動したエージェント自身は信頼する。
   同じ UID の接続はすべて同等の権限を持ち、クライアントの役割やトークンは設けない。
   したがって、エージェントがソケット経由で権限要求に応答することは防がない。
   防ぐ対象は他ユーザーとネットワークからのアクセス。
3. 状態ファイルはすべて 0600。シンボリックリンクを辿って書き込まない。
4. 権限要求は既定でユーザーへ中継する。自動承認は、ユーザーが `agent.create` の
   `--mode` 指定（M1 から）またはプロファイル（M4 から）で自動承認系の mode
   （例: Codex の full-access、Claude の bypassPermissions）を明示した場合だけ。
   その場合は `agent.create` の応答・一覧・TUI に明示する。
5. worktree の削除は `owned = true` かつ dgc 管理領域内のパスに限る
   （`worktree_manager.rs` の `is_generated_path` の考え方を移植）。
6. ログに prompt 本文・エージェント出力本文を出さない（tracing は ID と状態のみ）。
   本文は events.jsonl（0600）にだけ保存する。
7. 外部からのコード取得・自動更新・テレメトリーを持たない。

## 10. マイルストーン

各マイルストーンの完了条件は「受け入れ条件を満たし、`scripts/verify.sh` の該当
チェックが通る」こと。実プロバイダを使う確認は手動（認証とトークン消費が必要）。

| M | 内容 | 受け入れ条件（概略） |
|---|---|---|
| M0 | 足場: workspace 化、新 crate の空実装、PROGRESS.md、AGENTS.md 更新、verify/CI を新 crate に対応 | `cargo build --workspace` と既存テストが通る。旧 `dgc` が従来通り動く |
| M1 | デーモン中核と CLI: Store、ソケット、プロジェクト、ワークスペース、Claude/Codex アダプタ、権限中継、`dgc-next` の CLI | 偽プロバイダ（記録済みフィクスチャを再生するスタブ）で全メソッドを自動テスト。実 CLI で「作成→追送→権限許可→中断→再開」を手動確認し記録 |
| M2 | TUI: エージェント一覧、ストリーム表示、追送、権限ダイアログ、ワークスペース作成 | TUI 操作をスクリーンショットか端末録画で記録 |
| M3 | 切替: 新バイナリを `dgc` に改名、旧 `src/` と旧専用依存・ドキュメント・skills を削除、README 更新 | 旧コードへの参照ゼロ、`verify.sh rust` が通る |
| M4 | プロファイル、スケジュール（cron で新エージェント起動）、heartbeat（既存エージェントへ定期追送）、ワークスペーススクリプト（`dgc.json` のコマンドを監督実行）、レート制限時の引き継ぎ提案 | 各機能の自動テストと手動確認記録 |
| M5 | エージェント向け MCP サーバー（`dgc mcp` を stdio で起動し、ソケット経由でデーモンを操作。サブエージェント作成・追送・状態取得。エージェントは信頼する前提なので操作範囲は CLI と同等。サブエージェントには親子関係を記録し、入れ子の深さ上限（初期値 3）だけ設ける） | Claude / Codex から MCP 経由でサブエージェントを起動する手動確認 |

**レート制限時の引き継ぎ（M4）**: `rate_limited` を受けたら、同じワークスペースで
別プロバイダのエージェントを起動する操作を提案する（自動実行しない）。引き継ぎ
プロンプトは dgc が機械的に組み立てる（dgc は LLM を呼ばない）: 直近のユーザー指示、
最終応答の末尾（上限付きの抜粋）、ワークスペースの `git status` / `git diff --stat`、
元エージェントの events.jsonl のパス（新エージェントが必要なら自分で読む）。

## 11. 進捗管理と開発の引き継ぎ

開発中に Claude Code のトークンを使い切った場合、Codex で開発を続ける（ユーザー
発言）。そのため、進捗と次の作業はリポジトリ内のファイルだけで分かるようにする。

- `docs/orchestrator/PROGRESS.md`: 唯一の進捗の正本。内容:
  - マイルストーンとタスクの一覧（状態: 未着手 / 作業中 / 完了 / 保留）
  - 「現在の作業」: 作業中タスク、どこまで済んだか、次の一手、未コミットの変更有無
  - 「決定事項」: 設計から外れた判断、追加依存とその理由
  - 「検証記録」: 実行したコマンドと結果、手動確認の記録
- 運用ルール:
  - タスクを 1 つ終えるごとに、コードと PROGRESS.md を同じコミットに含める。
  - 作業を中断するとき（トークン切れが近い場合を含む）は、「現在の作業」を更新して
    コミットする。未完成コードは `wip(scope): ...` コミットにしてよい。
  - 特定エージェントの記憶、TODO ツール、セッション状態に頼らない。
- `AGENTS.md`（Codex と Claude の両方が読む）に「オーケストレーター移行中。まず
  PROGRESS.md を読み、『現在の作業』から再開する」という入口を置く。また、
  「全面移行は未承認」という既存の記述を今回の決定に合わせて書き換える。
- 実装計画（`docs/superpowers/plans/2026-11-25-orchestrator-*.md`）はマイルストーン
  単位で作成し、PROGRESS.md から参照する。

## 12. 実装者に任せること

- 各 crate の内部モジュール分割、エラー型の細部。
- 偽プロバイダの実装方法（テスト用バイナリか、テスト内のパイプか）。
- TUI のレイアウト詳細（M2 計画時にスケッチを示す）。
- events.jsonl のローテーション方式の詳細。
- cron 式の解析を自作するか crate を使うか（M4 で依存追加の理由を記録）。
