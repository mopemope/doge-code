# Orchestrator M6: MCP for Sub-agents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Codex など superpowers が無い環境では、各タスクを上から順に実行し、完了ごとに `docs/orchestrator/PROGRESS.md` を更新してコミットする。

**Goal:** dgc が起動したエージェントが MCP サーバー `dgc mcp` を通じてサブエージェントを作成・追送・待機・中断・アーカイブできるようにする。親子関係を記録し、入れ子の深さと子のモードを制限する。

**Architecture:**
- **デーモン:** `agent.create` の `parent_id` を検証する。項目は親の存在、深さの上限、子のモードが親より緩くないこと。
- **MCP の付与:** `mcp = true` のエージェントにだけ付ける。Claude は `--mcp-config`、Codex は `-c mcp_servers.dgc=…` で起動引数に MCP サーバー定義を足す。サーバーの中身は `<dgc 実行ファイル> mcp --agent <id>`。
- **`dgc mcp`:** stdio の MCP サーバー（JSON-RPC 2.0、依存追加なし）。既存のソケットクライアントでデーモンに接続する。操作できるのは呼び出し元エージェントの子孫だけ。
- **CLI と TUI:** `--mcp` とプロファイルの `mcp` を受け付け、親子関係を表示する。

**Tech Stack:** 既存の crates のみ（新しい依存なし）

**Spec:** `docs/superpowers/specs/2026-11-25-orchestrator-design.md` §10（M5 行。M4 計画時の分割で M6 に移った）、§9。

ユーザー決定（2026-11-27）:
- MCP に出すのはサブエージェントの操作だけ。権限への応答、スクリプト、スケジュール、プロジェクト、ワークスペースの操作は出さない。
- 子のモードは親より緩くしない。既定はプロバイダの既定モード。自動承認系のモードは、親が自動承認のときだけ指定できる。
- MCP はエージェントに明示した場合（`--mcp`、またはプロファイルの `mcp = true`）だけ付ける。
- 子は親と同じワークスペースで動く。worktree の作成は MCP に出さない。

## Global Constraints

### プロトコル
- `PROTOCOL_VERSION` を 4 に上げる。
- `AgentCreateParams` と `Agent` に `#[serde(default)] mcp: bool` を追加する。`Agent` には `depth: u32` も追加する（ルートは 0、子は親 + 1）。
- `ProfileConfig` に `mcp: Option<bool>` を追加する。明示した `--mcp` が優先する。

### 親子の規則（デーモンが `agent.create` で検証する）
- `parent_id` を指定する場合、親が存在し、archived でなく、同じ `workspace_id` であること。
- `depth = 親.depth + 1` が `max_agent_depth` 以下であること。`max_agent_depth` は `config.toml` のトップレベル設定で、既定 3、範囲 1–8。
- 子の `auto_approve` が true になるのは、親の `auto_approve` が true の場合だけ。違反は `INVALID_PARAMS`。
- 子の `mcp` は、`depth < max_agent_depth` の場合にだけ true にできる。上限の深さで `mcp = true` を要求したら `INVALID_PARAMS`。
- 親を archive しても子は残す（独立して動く）。README に記載する。

### MCP の付与
- サーバー名は `dgc`。コマンドはデーモンの実行ファイル（`std::env::current_exe()`。起動時に解決して保持する）。引数は `["mcp", "--agent", "<agent_id>"]`。
- 環境変数として、デーモンの状態ルート（`DGC_STATE_ROOT`）とソケットパスを MCP サーバーに渡し、同じデーモンに接続させる。
- Claude:
  - `--mcp-config '<JSON>'` を足す。`--strict-mcp-config` は付けない（ユーザー自身の MCP 設定を消さない）。
  - JSON は `{"mcpServers":{"dgc":{"type":"stdio","command":…,"args":[…],"env":{…}}}}`。
- Codex:
  - `app-server` の前に `-c mcp_servers.dgc.command=…`、`-c mcp_servers.dgc.args=[…]`、`-c mcp_servers.dgc.env={…}`、`-c mcp_servers.dgc.tool_timeout_sec=330` を足す。
  - 正確な書式は Task 1 で、公式ドキュメントと、トークンを使わない確認（`codex mcp list -c …`）で決め、PROGRESS に記録する。
- `mcp = false` のエージェントの起動引数は今と完全に同じであること（既存の記録済みフィクスチャとテストがそのまま通る）。

### `dgc mcp`（MCP サーバー）
- stdio の改行区切り JSON-RPC 2.0。受け付けるメソッドは次のとおり。
  - `initialize`
  - `notifications/initialized`
  - `ping`
  - `tools/list`
  - `tools/call`
  - 未知のメソッドは `-32601` を返す。未知の通知は無視する。
- `initialize` で、クライアントが要求した `protocolVersion` が対応表にあればそれを返し、無ければ対応表の最新を返す。
  - 対応表は Task 2 で公式仕様（modelcontextprotocol.io/specification）から決め、PROGRESS に記録する。
  - `capabilities` は `{"tools":{}}` だけ。
- デーモンには `Client::connect` で接続する。デーモンが無くても自動起動しない（エラーを返す）。
- ツール（名前は固定）:

  | ツール | 引数 | 結果 |
  |---|---|---|
  | `create_agent` | `{prompt, title?, provider?, model?, mode?, mcp?}` | `{agent_id, provider, mode, auto_approve}` |
  | `send_agent` | `{agent_id, text, behavior?: "queue"\|"steer"\|"interrupt"}` | — |
  | `get_agent` | `{agent_id}` | `{state, title, provider, last_answer, pending_permission: bool}` |
  | `wait_agent` | `{agent_id, timeout_secs?}` | `get_agent` と同じ。`timed_out: bool` を加える |
  | `list_agents` | `{}` | 呼び出し元の子孫の一覧 |
  | `interrupt_agent` | `{agent_id}` | — |
  | `archive_agent` | `{agent_id}` | — |

  - `create_agent`: provider の既定は親と同じ。workspace は親と同じ（引数に無い）。
  - `wait_agent`: 状態が `idle`/`stopped`/`failed`/`archived`/`waiting_permission` になるまで待つ。`timeout_secs` は既定 60、最大 300。
- `create_agent` 以外のツールは、対象が呼び出し元の子孫（親の連鎖に呼び出し元を含む）の場合だけ実行する。それ以外は `isError: true` で拒否する。
- `last_answer` は直近の完了したターンの回答で、最大 16 KiB（文字境界で切る）。ツール結果は `content: [{type: "text", text: <JSON>}]` で返す。
- エラーは `isError: true` の結果で返す。文面はエラー種別とデーモンのエラーコードだけにし、prompt 本文は含めない。
- stdout には JSON-RPC 以外を書かない。stderr と tracing に prompt、回答、ツール引数の本文を出さない。

### 共通
- 新しい依存なし。既存のセキュリティ不変条件と TUI の権限安全規則を弱めない。
- dgc は承認を代行しない。MCP ツール呼び出しへのプロバイダの承認（Claude の権限要求など）は、従来どおりユーザーに中継する。
- 子の権限要求も、従来どおりユーザーに中継する。
- 信頼境界は UID のまま。親子の規則は、同じ UID の任意のクライアントに対する安全柵であって、セキュリティ境界ではない。README と AGENTS.md にこの旨を書く。

## Review Focus

1. **auto-approve でない親が、自動承認の子を作れてしまう。**
   - 期待: `mode` を直接指定しても、プロファイル経由でも拒否される。
   - テスト: Task 1 の `child_cannot_be_more_permissive_than_parent`。
2. **深さの上限を超える入れ子と、上限での `mcp` 要求。**
   - 期待: どちらも `INVALID_PARAMS`。
   - テスト: Task 1 の `depth_limit_and_mcp_at_limit_are_refused`。
3. **子孫でないエージェントの操作。**
   - 対象: 兄弟、親、無関係なエージェントへの `send_agent`/`archive_agent`。
   - 期待: 拒否され、対象は変わらない。
   - テスト: Task 2 の `tools_refuse_non_descendants`。
4. **`mcp = false` のエージェントの起動引数が変わる回帰。**
   - 期待: 引数が 1 文字も変わらない。
   - テスト: Task 1 の `agents_without_mcp_keep_their_launch_args`。
5. **`wait_agent` 中にデーモンが止まる、または対象が archived になる。**
   - 期待: ハングせず、エラーまたは最終状態を返す。
   - テスト: Task 2 の `wait_agent_ends_on_daemon_stop_and_archive`。

---

### Task 1: 親子の規則と MCP の付与（デーモン）

**Files:**
- `crates/dgc-protocol/src/{lib.rs,methods.rs,model.rs}`
- `crates/dgc-daemon/src/{config.rs,agent.rs,provider/mod.rs,provider/claude.rs,provider/codex.rs,server.rs}`
- テスト: `crates/dgc-daemon/tests/{agent_manager.rs,claude_adapter.rs,codex_adapter.rs,server.rs}`
- `crates/dgc-daemon/tests/fixtures/README.md`、`AGENTS.md`、`docs/orchestrator/PROGRESS.md`

**Interfaces（Produces）:**
- `Agent { …, mcp: bool, depth: u32 }`、`AgentCreateParams { …, mcp: bool }`、`DaemonConfig.max_agent_depth: u32`、`ProfileConfig.mcp: Option<bool>`
- アダプタの起動指定に `mcp: Option<McpLaunch>` を渡す。`McpLaunch { command: PathBuf, args: Vec<String>, env: Vec<(String, String)> }` はデーモンが組み立てる。
- `AgentManager` が親子の規則を検証する。resume でも保存済みの `mcp` を使う。
- `scheduled` と `handoff` で作るエージェントは `mcp = false`、`depth = 0`、`parent_id = None` のまま（変更しない）。

- [ ] **Step 1: テスト**
  - `child_cannot_be_more_permissive_than_parent`: 自動承認でない親に対する `mode` 指定とプロファイル指定 → `INVALID_PARAMS`。親が自動承認なら許可。
  - `child_requires_live_parent_in_same_workspace`: 存在しない、archived、別ワークスペース → `INVALID_PARAMS`。
  - `depth_limit_and_mcp_at_limit_are_refused`: 既定 3。depth 3 の作成は可。depth 4 と、depth 3 での `mcp = true` は不可。
  - `max_agent_depth_config_is_validated`: 0 と 9 は起動エラー。
  - `profile_mcp_applies_and_explicit_flag_wins`
  - `claude_launch_args_include_mcp_config_only_when_enabled`: JSON を解析して command、args、env を検証する。
  - `codex_launch_args_include_mcp_overrides_only_when_enabled`
  - `agents_without_mcp_keep_their_launch_args`: 既存の引数と完全一致。
  - `resume_keeps_mcp`
  - `old_protocol_hello_is_refused`: v3 は拒否。
- [ ] **Step 2: 失敗を確認** — `cargo test --locked -p dgc-daemon --test agent_manager --test claude_adapter --test codex_adapter --test server`。期待: コンパイルエラーか FAIL。
- [ ] **Step 3: 実装**
  - Codex の `-c` 書式は、公式ドキュメントとトークンを使わない `codex mcp list -c …` で確かめ、PROGRESS 決定事項に書く（実行は作業用の `CODEX_HOME` で行い、ユーザーの設定を変えない）。
  - Claude の `--mcp-config` に JSON 文字列を渡せることも、公式ドキュメントで確かめる。
  - AGENTS.md の Provider adapters 節に、MCP 引数の規則を 1 行足す。
- [ ] **Step 4: 通過を確認** — Step 2 と同じコマンド。期待: PASS。
- [ ] **Step 5: コミット** `feat(daemon): record sub-agent parents and attach the dgc MCP server`

### Task 2: `dgc mcp`（MCP サーバー）

**Files:**
- 新規: `crates/dgc-cli/src/mcp/{mod.rs,rpc.rs,tools.rs}`
- 変更: `crates/dgc-cli/src/main.rs`（隠しサブコマンド `mcp --agent ID`）
- テスト: `crates/dgc-cli/tests/mcp.rs`（新規）

**Interfaces（Consumes）:** Task 1 の `Agent.depth`/`mcp`/`parent_id` と `agent.create` の規則。既存の `agent.get/list/send/interrupt/archive/events` と `subscribe`。

**Interfaces（Produces）:**
- `dgc mcp --agent <AGENT_ID>`: Global Constraints のツールを提供する。
- 子孫の判定は `agent.get` で親を連鎖的にたどり、`depth` 回で打ち切る。
- `wait_agent` は `subscribe` で状態の変化を待つ。待つ前と、通知が途切れたときに `agent.get` で確かめる。

- [ ] **Step 1: テスト**（テストは MCP クライアントとして stdio で話す。デーモンと偽プロバイダは実物を使う）
  - `initialize_negotiates_version_and_lists_tools`
  - `unknown_method_and_notifications`
  - `create_agent_makes_child_in_parent_workspace_with_parent_provider`
  - `create_agent_refusals_are_tool_errors_without_prompt_text`
  - `tools_refuse_non_descendants`
  - `wait_agent_returns_last_answer_bounded`
  - `wait_agent_times_out`
  - `wait_agent_ends_on_daemon_stop_and_archive`
  - `send_interrupt_archive_descendant`
  - `list_agents_returns_only_descendants`
  - `mcp_does_not_autostart_daemon`
  - `stdout_carries_only_json_rpc`
- [ ] **Step 2: 失敗を確認** — `cargo test --locked -p dgc-cli --test mcp`（先に偽プロバイダをビルドする）。
- [ ] **Step 3: 実装** — 対応する MCP の版を PROGRESS 決定事項に書く。
- [ ] **Step 4: 通過を確認** — 同じコマンド。sensitive-logging も PASS。
- [ ] **Step 5: コミット** `feat(cli): serve sub-agent tools over MCP`

### Task 3: CLI と TUI での表示と有効化

**Files:**
- `crates/dgc-cli/src/commands/agent.rs`（`run --mcp`）、`crates/dgc-cli/src/output.rs`、`crates/dgc-cli/src/commands/profile.rs`
- `crates/dgc-cli/src/tui/{state.rs,render.rs,update.rs}`
- テスト: `crates/dgc-cli/tests/agent_cli.rs`、TUI の単体テスト
- `README.md`、`docs/orchestrator/m6-tui-screen.txt`

**Interfaces（Produces）:**
- `dgc run --mcp`
- `dgc ls`: 子は親の下に字下げして表示し、`mcp` と深さを示す。`--json` には `parent_id`/`depth`/`mcp` が含まれる。
- `dgc show`: 親 ID を表示する。
- `dgc profile ls`: `mcp` を表示する。
- TUI:
  - ツリーで子を親の直下に字下げして表示する（同じワークスペース内）。`mcp` のエージェントには `[mcp]` を付ける。
  - 新規作成ダイアログは、選んだプロファイルの `mcp` に従う（専用のトグルは足さない）。
  - 既存の権限安全規則は変えない。
- README に「Sub-agents (MCP)」節を足す。内容は次のとおり。
  - 有効化の方法
  - ツール一覧
  - 親子の規則
  - 権限要求の扱い
  - 信頼境界（UID）
  - 親を archive しても子が残ること

- [ ] **Step 1: テスト**
  - `run_with_mcp_sets_flag`
  - `ls_nests_children_under_parent`
  - `ls_json_has_parent_depth_mcp`
  - `profile_ls_shows_mcp`
  - TUI `tree_nests_children_and_marks_mcp`
  - TUI `permission_dialog_rules_unchanged_with_children`: 子の権限要求も、既存の arming と `seen_end` の規則で開く。
- [ ] **Step 2–4** — 失敗を確認 → 実装 → 通過を確認。TUI の画面記録を `docs/orchestrator/m6-tui-screen.txt` に残す（tmux、合成の状態）。
- [ ] **Step 5: コミット** `feat(cli): enable MCP per agent and show sub-agent trees`

### Task 4: M6 受け入れ

- [ ] **Step 1: 偽プロバイダで確認する** — 作業用の `DGC_STATE_ROOT` と一時リポジトリで、MCP クライアント役のスクリプトから `dgc mcp` を通して次を確かめる。
  - 子の作成 → 待機 → 回答 → アーカイブ
  - 深さの上限
  - 子のモード制限
- [ ] **Step 2: 実プロバイダで確認する** — 最も安いモデル、自動承認なし、使い捨てリポジトリで行う。
  - Claude（haiku、`--mcp`）に「`create_agent` で子を 1 つ作り、`wait_agent` で回答を受け取って報告して」と頼む。
    - `mcp__dgc__create_agent` の権限要求が `dgc permit ls` に中継されることを確かめる。
    - 受け入れの担当者が `dgc permit` で応答する。許可してよいのは dgc の MCP ツール（`mcp__dgc__*`）の要求だけで、それ以外の要求は拒否する。応答した内容はすべて記録する。
    - 子が完了することを確かめる。
  - Codex でも 1 回同じ確認をする。MCP ツール呼び出しに承認が要るかどうかを記録する。
  - initialize の要求（`protocolVersion` と `clientInfo`）を fixtures に記録する。
  - ユーザーのログインは CLI の認証をそのまま使う。資格情報はログにも記録にも残さない。
- [ ] **Step 3: 全検証を通す** — fmt、`verify.sh rust`、guidance、msrv、tui-deps を通し、README と PROGRESS を更新する。
- [ ] **Step 4: コミット** `docs(orchestrator): complete M6 acceptance`。PROGRESS の M6 を完了にし、「現在の作業」をフェーズ A の総括（残りの保留指摘の整理）とフェーズ B の設計準備にする。
