# プロバイダ wire フィクスチャ

実際の Claude Code CLI と Codex CLI を stdio で駆動し、やり取りを記録したもの。
M1 Task 10（偽プロバイダ）、Task 11（ClaudeAdapter）、Task 12（CodexAdapter）はこの記録と
下の Wire notes だけを根拠にする。

- 記録日: 2026-11-25
- Claude Code: `claude --version` → `2.1.332 (Claude Code)`、モデル `--model haiku`（`claude-haiku-5-5`）
- Codex: `codex --version` → `codex-cli 0.172.0`、モデル `gpt-6-luna`（`model/list` で「Fast and affordable」。
  `mini` という名前のモデルは一覧に無かった）、`turn/start` の `effort: "low"`
- OS: Linux x86_64。どちらもログイン済みの実アカウントを使用（ユーザー設定・フック・MCP 設定はそのまま）

## 形式

1 行 1 メッセージの JSONL: `{"dir":"in"|"out","msg":<JSON>}`。

- `in` = dgc → プロバイダ（stdin に書いた 1 行）、`out` = プロバイダ → dgc（stdout の 1 行）
- 行の順序は実際の送受信順（記録器が in/out を同じロックで追記）。stdout に JSON 以外の行は一度も出なかった。
- 1 ファイル = 1 プロセス。別プロセスにまたがるシナリオ（resume）は 2 つ目のプロセスだけを収録した。

## 伏せ字

値だけを置換し、フィールド名・構造・JSON 型は保つ。

| 元 | 置換後 |
|---|---|
| 使い捨てリポジトリの絶対パス | `/WORKSPACE`（Claude のプロジェクト名エンコード形式は `-WORKSPACE`） |
| `$HOME` | `/home/USER` |
| メールアドレス、組織名、プラン名 | `user@example.com`、`ORG`、`PLAN` |
| ホスト名、remote-control の installationId | `HOSTNAME`、ゼロ UUID |
| Codex `userAgent` | `dgc/0.172.0 (OS VERSION; x86_64) TERMINAL (dgc; 0.1.0)` |
| Claude の API request id（`req_...`） | `req_REDACTED` |
| Claude の thinking `signature` | `SIGNATURE_REDACTED` |
| Claude フック出力（`hook_response.output/stdout`） | `<redacted hook output>` |
| Claude `initialize` 応答の `commands`/`account`/`pid`、`system/init` の `skills`/`slash_commands`/`plugins`/`mcp_servers`/`plugin_warnings` | 1〜2 件の `example-*` に差し替え |
| レート制限の値（Claude `utilization`/`resetsAt`/`overage*`、Codex `usedPercent`/`resetsAt`/`balance`/`planType`） | `0.5` / `1700000000` / `"REDACTED"` / `50` / `"0"` / `"PLAN"` |
| ユーザー固有の MCP サーバー名 | `userMCP` |

session id、thread/turn/item id、tool_use id、タイムスタンプ、`emittedAtMs` は実値のまま（秘密ではない）。

## 記録手順

記録器は Python 3 標準ライブラリだけの使い捨てスクリプト（コミットしない）。各シナリオごとに
新しい使い捨て git リポジトリ（`README.md` と `notes.txt` の 2 ファイル）を作り、そこを cwd にして起動した。
子プロセスの環境から `CLAUDE*` / `CLAUDECODE` 変数は除いた（記録者自身が Claude Code 内で動いていたため）。
自動承認系の mode / policy は一度も使っていない。

### Claude

```
claude -p --input-format stream-json --output-format stream-json --verbose \
       --permission-prompt-tool stdio --model haiku [--resume <session_id>]
```

| ファイル | 内容 |
|---|---|
| `claude/simple_turn.jsonl` | initialize → 「Reply with exactly the word: pong」→ result |
| `claude/permission_allow.jsonl` | 「Bash で `touch hello.txt`」→ `can_use_tool` に allow → 実行され hello.txt 作成 |
| `claude/permission_deny.jsonl` | 同じ依頼に deny → 実行されず（hello.txt 無し）、モデルは「denied」と返答 |
| `claude/interrupt.jsonl` | 長文生成の依頼 → 2.5 秒後に interrupt → 同じプロセスでもう 1 ターン（「ok」） |
| `claude/resume.jsonl` | 前プロセス（コード語 BLUEFISH を覚えさせた 1 ターン、`simple_turn` と同形）の `session_id` で `--resume` した 2 つ目のプロセス。モデルは BLUEFISH と回答 |
| `claude/error.jsonl` | `--model no-such-model-dgc` で 1 ターン |
| `claude/error.stderr` | 上の stderr（全文） |
| `claude/permission_without_stdio_tool.jsonl` | 参考: `--permission-prompt-tool stdio` を**付けず**に permission_deny と同じ依頼 |

### Codex

```
codex app-server        # stdio JSON-RPC（--listen stdio:// が既定）
```

| ファイル | thread/start の policy / sandbox | 内容 |
|---|---|---|
| `codex/simple_turn.jsonl` | on-request / workspace-write | 「pong」 |
| `codex/command_approval_accept.jsonl` | **untrusted** / workspace-write | 「`touch hello.txt` を実行」→ `item/commandExecution/requestApproval` に `accept` → 実行 |
| `codex/command_on_request_no_approval.jsonl` | on-request / workspace-write | 参考: 同じ依頼で承認要求が**出ない**ことの記録（サンドボックス内で実行される） |
| `codex/file_change_decline.jsonl` | on-request / **read-only** | 「apply_patch で hello.txt を作成」→ `item/fileChange/requestApproval` に `decline` → 作成されず |
| `codex/interrupt.jsonl` | on-request / workspace-write | 長文生成 → 最初の delta から 1 秒待って `turn/interrupt`（その時点で delta 74 個受信済み: 38〜111 行目、interrupt は 112 行目） |
| `codex/steer.jsonl` | on-request / workspace-write | 果物 40 個の列挙 → 最初の delta 直後に `turn/steer`（「steered とだけ返せ」）|
| `codex/resume.jsonl` | （resume 時は指定せず） | 前プロセスで BLUEFISH を覚えさせた thread を新プロセスで `thread/resume` → 1 ターン |
| `codex/resume_exclude_turns.jsonl` | 同上 | 参考: `thread/resume` に `excludeTurns: true` を付けた場合（ターンは実行しない） |
| `codex/error.jsonl` | on-request / workspace-write、model `no-such-model-dgc` | アクティブターン無しでの `turn/steer`/`turn/interrupt` のエラー応答と、不正モデルでの `turn/start` |

使用ターン数: Claude 8（うち参考 1）、Codex 9（うち参考 1、エラー 1）。

### M6 の MCP 起動引数

MCP が有効なとき、Claude には `--mcp-config` と stdio サーバー
`dgc` のインライン JSON を渡す。Codex には `app-server` の前に
`mcp_servers.dgc` の `command`、`args`、`env`、`tool_timeout_sec`、
`required=true` を `-c` で渡す。ユーザー承認を使うモードでは
`default_tools_approval_mode="prompt"` も渡す。アダプタテストは
起動引数を取得して通常モードと `full-access` の内容、MCP 無効時の完全一致を
確認する。`mcp_required_startup_failure.jsonl` は必須サーバーの起動に失敗した
ときの `thread/start` エラーを再現する合成シナリオ。
公式資料と Codex CLI での読み取り結果は [進捗の決定事項](../../../../docs/orchestrator/PROGRESS.md#決定事項) に記録。

---

## Wire notes: Claude Code 2.1.332（stream-json）

### 起動

```
claude -p --input-format stream-json --output-format stream-json --verbose \
       --permission-prompt-tool stdio --model <M> [--permission-mode <MODE>] [--resume <session_id>]
```

- cwd = ワークスペース。stdin を閉じるまでプロセスは生き続け、複数ターンを受け付ける。
- **`--permission-prompt-tool stdio` は必須**。`--help` には載っていない隠しオプションだが受け付けられる。
  これを付けないと（`--permission-prompts` は既定値 `host` のまま。明示指定は未試行）、承認が必要なツールは
  ホストへ問い合わせずに自動拒否される: `{"type":"system","subtype":"permission_denied","tool_name":"Bash","tool_use_id":"toolu_..","message":"touch in '/WORKSPACE/hello.txt' needs approval. ..."}`
  が出て、tool_result は `is_error:true`、`tool_result_meta[].permission_decision = {"decision":"reject","source":"config"}`
  （`permission_without_stdio_tool.jsonl`）。
- `--permission-mode` の選択肢（`--help`）: `acceptEdits|auto|bypassPermissions|manual|dontAsk|plan`。
  指定しないとき `initialize` 応答の `current_permission_mode` と `system/init` の `permissionMode` は `"default"`。
  `--permission-mode` を付けた起動は記録していない。

### ハンドシェイクと 1 ターンの順序

1. dgc → `{"type":"control_request","request_id":"init_1","request":{"subtype":"initialize"}}`
   （`request_id` は dgc が決める任意の文字列）
2. CLI → ユーザー設定の SessionStart フックがあれば `system/hook_started` と `system/hook_response` が
   **initialize の応答より先に**出る（`--include-hook-events` なしでも出る。`hook_name` は新規起動で
   `SessionStart:startup`、resume で `SessionStart:resume`）。dgc は無視してよい。
3. CLI → `{"type":"control_response","response":{"subtype":"success","request_id":"init_1","response":{...}}}`。
   `response` のキー: `commands, agents, output_style, available_output_styles, user_output_styles_dir, models,
   account{email,organization,subscriptionType,apiProvider}, pid, claude_code_version, current_permission_mode,
   session_state, capabilities, ...`。`models[]` は `{value, resolvedModel, displayName, description, ...}`。
   initialize を省いた場合の挙動は未確認（常に送ること）。
4. dgc → ユーザー入力:
   `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]},"parent_tool_use_id":null,"session_id":""}`
   （記録では Agent SDK と同じく `parent_tool_use_id` と `session_id:""` を付けた。これらを省いた形は未確認。）
5. CLI → `{"type":"system","subtype":"init","session_id":"<uuid>","cwd":"/WORKSPACE","model":"claude-haiku-5-5","permissionMode":"default","tools":[..],"claude_code_version":"2.1.332",...}`。
   **ユーザー入力ごと（ターンごと）に毎回出る**（プロセス起動時に 1 回ではない）。`session_id` は `control_request` / `control_response` 以外の全メッセージに付く（制御メッセージには付かない）。
6. CLI → 0 個以上の `{"type":"system","subtype":"thinking_tokens","estimated_tokens":N,"estimated_tokens_delta":N,...}`（無視可）。
7. CLI → `assistant` メッセージ。**content ブロック 1 つにつき 1 行**（thinking / tool_use / text が別の行で、
   同じ `message.id` を共有する）。`message.content[].type` は `thinking`（`thinking` は空文字、`signature` あり）、
   `tool_use`（`id`, `name`, `input`, `caller`）、`text`（`text`）。通常の行の `message.stop_reason` は `null`
   （確定値は `result` 側）。ただし不正モデル時の合成エラー行（`message.model:"<synthetic>"`）は `"stop_sequence"`（`error.jsonl` 9 行目）。トップレベルに `session_id`, `uuid`, `timestamp`, `request_id`, `parent_tool_use_id`。
8. CLI → `{"type":"rate_limit_event","rate_limit_info":{...}}`（API 呼び出しごと。下記）。
9. ツール実行後 CLI → `{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_..","content":"..","is_error":false}]},"tool_use_result":{..},"tool_result_meta":[{"id":"toolu_..","permission_decision":{"decision":"accept","source":"user_temporary"}}],...}`。
10. CLI → `result`（**ターン終了の唯一の合図**）:
    ```
    {"type":"result","subtype":"success","is_error":false,"result":"pong","stop_reason":"end_turn",
     "terminal_reason":"completed","num_turns":1,"session_id":"..","total_cost_usd":0.0034,
     "usage":{"input_tokens":2,"cache_creation_input_tokens":16192,"cache_read_input_tokens":13878,"output_tokens":4,...},
     "modelUsage":{"claude-haiku-5-5":{"inputTokens":..,"outputTokens":..,"costUSD":..,"contextWindow":1000000,...}},
     "permission_denials":[],"api_error_status":null,"duration_ms":747,...}
    ```
11. 次の `user` を送れば同じプロセスで次のターン（`system/init` から繰り返し）。stdin を閉じると終了。
    終了コードは 0。ただし最後の `result` が `is_error:true` だった場合は 1（`error.jsonl`）。

### 権限要求（stdio）

CLI → dgc（`assistant` の tool_use 行の直後）:

```
{"type":"control_request","request_id":"fea6759a-ad91-49ba-b9b6-b55ac5acaf7c",
 "request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash",
  "input":{"command":"touch hello.txt","description":"Create empty hello.txt file in current directory"},
  "description":"Create empty hello.txt file in current directory",
  "permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"touch hello.txt"}],"behavior":"allow","destination":"localSettings"},
                            {"type":"addDirectories","directories":["/WORKSPACE"],"destination":"session"},
                            {"type":"setMode","mode":"acceptEdits","destination":"session"}],
  "blocked_path":"/WORKSPACE/hello.txt","tool_use_id":"toolu_01RoNynfmdUmi7KungCUcYfo"}}
```

`request_id` は CLI が振る UUID 文字列。`tool_use_id` で直前の tool_use と対応付く。応答は同じ `request_id` で:

- 許可: `{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"allow","updatedInput":<input をそのまま>}}}`
  → ツールが実行され、tool_result の `permission_decision` は `{"decision":"accept","source":"user_temporary"}`。
- 拒否: `{"type":"control_response","response":{"subtype":"success","request_id":"<id>","response":{"behavior":"deny","message":"Denied by dgc user"}}}`
  → tool_result は `{"type":"tool_result","content":"Denied by dgc user","is_error":true,...}`、
  `tool_use_result:"Error: Denied by dgc user"`、`tool_result_meta[].non_execution_kind:"permission-rule"`、
  `permission_decision:{"decision":"reject","source":"user_reject"}`。ターンは続行し、`result` は
  `subtype:"success"`, `is_error:false` で `permission_denials:[{"tool_name":"Bash","tool_use_id":"..","tool_input":{..}}]`。
- 応答を返すまで CLI は待つ（記録では即時応答。タイムアウトの有無は未確認）。
- `permission_suggestions` を使った「セッション中は許可」（`updatedPermissions`）は試していない。

### 中断

dgc → `{"type":"control_request","request_id":"int_1","request":{"subtype":"interrupt"}}`

CLI → 次の順（`interrupt.jsonl`）:
1. `{"type":"control_response","response":{"subtype":"success","request_id":"int_1","response":{"still_queued":[]}}}`
2. `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]},...}`
3. `{"type":"result","subtype":"error_during_execution","is_error":true,"terminal_reason":"aborted_streaming","stop_reason":null,"result":null,"errors":["[ede_diagnostic] ..."],...}`

プロセスは生きたままで、次の `user` 入力で通常どおり次のターンが動く（同じ `session_id`）。
中断までに完了した `assistant` 行は無かった（thinking 中に中断）。

### Resume

前のプロセスの `session_id` を `--resume <session_id>` に渡して新しいプロセスを起動し、同じ cwd で
initialize → user を送る。`system/init` と `result` の `session_id` は**元と同じ値**（新しい id は振られない）。
会話文脈は保持される（BLUEFISH を回答）。

### エラー（不正モデル）

`--model no-such-model-dgc` でも起動・initialize・`system/init`（`model:"no-such-model-dgc"`）までは成功し、
最初のターンで失敗する:

- `assistant`（合成）: `message.model:"<synthetic>"`, `message.content:[{"type":"text","text":"There's an issue with the selected model (no-such-model-dgc). ..."}]`,
  トップレベルに `"error":"model_not_found"`, `"is_api_error_message":true`, `"api_error_status":404`, `"api_error_type":"not_found_error"`。
- `result`: **`subtype:"success"` かつ `is_error:true`**、`terminal_reason:"api_error"`, `api_error_status:404`,
  `api_error_type:"not_found_error"`, `result:<上のエラー文>`。→ 成否は `subtype` ではなく `is_error` で判定すること。
- stderr: `[claude-code:unrecognized_model] {"model":"no-such-model-dgc","query_source":"sdk"}`（1 行）。
- stdin を閉じた後の終了コード 1。
- 未ログイン時のエラーは記録していない（ユーザーのログアウトは行わない方針）。

### レート制限

```
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1700000000,"rateLimitType":"five_hour",
  "overageStatus":"..","overageDisabledReason":"..","isUsingOverage":false,
  "unifiedWindows":{"five_hour":{"utilization":0.5,"resetsAt":1700000000,"status":"allowed"},
                    "seven_day":{"utilization":0.5,"resetsAt":1700000000,"status":"allowed"}}},
 "uuid":"..","session_id":".."}
```

制限に達していなくても API 呼び出しごとに出る。観測した `status` は `"allowed"` のみで、上限到達時の実際の
`rate_limit_event`、`assistant`、`result` は**未観測**。dgc は `rate_limit_info.status` または `overageStatus` が
`"allowed"` / `"allowed_warning"`、あるいは `isUsingOverage` が `true` の更新をデバッグログだけに記録する。
それ以外で `status` が `"rejected"` または未知の文字列の場合は、その値を正規化して `rate_limited` の理由とする。
[Claude Agent SDK の型定義](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/types.py)

`rate_limit_info.resetsAt` が整数ならリセット時刻も記録する。記録した API エラー行の形式に基づき、`assistant` または
`result` 行で `api_error_status` が整数の `429` の場合は、理由を `http_429` とする。到達ケースと追加使用で継続するケースは
`claude/scenarios/rate_limited_*.jsonl` の合成シナリオで検証する。

### その他の出力（無視してよいもの）

`system/hook_started`、`system/hook_response`、`system/thinking_tokens`、`system/permission_denied`
（stdio ツールなし時のみ）。未知の `type` / `subtype` は今後も増えうる。

---

## Wire notes: Codex app-server 0.172.0（stdio JSON-RPC）

### 共通の形

- 1 行 1 JSON。**`"jsonrpc":"2.0"` フィールドはサーバーのどのメッセージにも付かない**。dgc も付けずに送り、受理された。
- 要求: `{"method":"..","id":<int>,"params":{..}}`、応答: `{"id":<int>,"result":{..}}` または
  `{"error":{"code":-32600,"message":".."},"id":<int>}`、通知: `{"method":"..","params":{..},"emittedAtMs":<int>}`
  （**`emittedAtMs` は params の外、トップレベル**。付かない通知もある）。
- サーバー → dgc の要求（承認）も `{"method":"..","id":0,"params":{..}}`。**サーバー要求の id は 0 から始まる別の番号空間**で、
  dgc 自身の要求 id と衝突しうる。`method` の有無で区別すること（`method` あり + `id` あり = サーバー要求、
  `method` なし + `id` あり = dgc の要求への応答）。

### ハンドシェイク

1. dgc → `{"method":"initialize","id":1,"params":{"clientInfo":{"name":"dgc","title":"dgc","version":"0.1.0"}}}`
   （`title` は任意、`name`/`version` 必須）
2. サーバー → `{"id":1,"result":{"userAgent":"dgc/0.172.0 (...) (dgc; 0.1.0)","codexHome":"/home/USER/.codex","platformFamily":"unix","platformOs":"linux"}}`。
   `userAgent` に CLI バージョン `0.172.0` が入る。
3. dgc → `{"method":"initialized"}`（params なし）
4. この直後、要求していない通知が来うる: `configWarning`（ユーザー設定の警告）、`remoteControl/status/changed`。無視可。

### thread/start

dgc → `{"method":"thread/start","id":2,"params":{"cwd":"/WORKSPACE","approvalPolicy":"on-request","sandbox":"workspace-write","model":"gpt-6-luna"}}`

- `approvalPolicy`: `"untrusted"|"on-request"|"never"`（または granular オブジェクト）。`sandbox`: `"read-only"|"workspace-write"|"danger-full-access"`。
- 応答 `result`: `thread{id, sessionId(=id), cwd, model, status{type:"idle"}, path, cliVersion:"0.172.0", originator:"dgc", source:"vscode", turns:[], ...}`、
  加えて `model`, `modelProvider:"openai"`, `approvalPolicy`, `approvalsReviewer:"user"`, `sandbox`（**応答では
  オブジェクト** `{"type":"workspaceWrite","writableRoots":[],"networkAccess":true,...}` / `{"type":"readOnly",...}`）,
  `reasoningEffort`（ユーザー設定由来。今回 `xhigh`）, `serviceTier`, `cwd`, `instructionSources` など。
- 続いて通知 `thread/started {thread}`、`warning {threadId, message}`（設定警告）、`mcpServer/startupStatus/updated`
  （ユーザーの MCP サーバーの起動状況。`status: starting|ready|failed`）。
- 不正なモデル名でも thread/start は成功する（`warning` が出るだけ）。

### turn/start とターン終了

dgc → `{"method":"turn/start","id":3,"params":{"threadId":"..","input":[{"type":"text","text":".."}],"effort":"low"}}`
（`effort` は任意。記録ではユーザー設定の `xhigh` を避けるため付けた）

応答 `{"id":3,"result":{"turn":{"id":"<turnId>","rootTurnId":"<turnId>","items":[],"itemsView":"notLoaded","status":"inProgress","error":null,...}}}`。
応答は即時で、ターンは非同期に進む。通知の順序（`simple_turn.jsonl`）:

1. `thread/status/changed {threadId, status:{type:"active",activeFlags:[]}}`
2. `turn/started {threadId, turn{id, status:"inProgress", ...}}`
3. （ユーザーのフック: `hook/started` / `hook/completed`。無視可。`account/updated {authMode, planType, reason}` も来る）
4. `item/started` / `item/completed` で `item.type:"userMessage"`（送ったテキスト）
5. `item/started {item:{type:"agentMessage",id:"msg_..",text:"",phase:"final_answer",...}, threadId, turnId, startedAtMs}`
6. `item/agentMessage/delta {threadId, turnId, itemId, delta:"pong"}`（0 回以上）
7. `item/completed {item:{type:"agentMessage",id,text:"pong",...}, threadId, turnId, completedAtMs}`
8. `thread/tokenUsage/updated {threadId, turnId, tokenUsage:{total:{totalTokens,inputTokens,cachedInputTokens,cacheWriteInputTokens,outputTokens,reasoningOutputTokens}, last:{同形}, modelContextWindow}}`
   — **使用量はここで来る**（`turn/completed` には含まれない）。モデル応答ごとに出る。
9. `account/rateLimits/updated`（下記）
10. `thread/status/changed {status:{type:"idle"}}`
11. **`turn/completed {threadId, turn:{id, status, error, items, itemsView, startedAt, completedAt, durationMs}}` がターン終了の合図**。
    `turn.status`: `"completed"` / `"interrupted"`（中断）/ `"failed"`（`turn.error` にエラー）。
    正常完了では `itemsView:"summary"` で `items` に agentMessage が入るが、中断・失敗では `itemsView:"notLoaded"`, `items:[]`
    （`interrupt.jsonl` 116 行目、`error.jsonl` 45 行目）。**アダプタは `turn/completed` の `items` に依存しないこと**
    （本文は `item/*` 通知から組み立てる）。

コマンド実行は `item.type:"commandExecution"`（`command:"/usr/bin/bash -lc 'touch hello.txt'"`, `cwd`, `status:"inProgress"→"completed"`,
`exitCode`, `aggregatedOutput`, `commandActions`）、ファイル変更は `item.type:"fileChange"`
（`changes:[{"path":"/WORKSPACE/hello.txt","kind":{"type":"add"},"diff":"hi\n"}]`, `status:"inProgress"→"declined"` 等）。

### 承認要求

**いつ出るか（実測）**:
- `on-request` + `workspace-write`: ワークスペース内の `touch` は承認なしでサンドボックス内実行された（`command_on_request_no_approval.jsonl`）。
- `untrusted` + `workspace-write`: 同じコマンドで `item/commandExecution/requestApproval` が出た。
- `on-request` + `read-only`: apply_patch によるファイル作成で `item/fileChange/requestApproval` が出た。

コマンド（サーバー → dgc）:

```
{"method":"item/commandExecution/requestApproval","id":0,"params":{"kind":"command",
  "threadId":"..","turnId":"..","itemId":"exec-7acf..","startedAtMs":1795597423518,"environmentId":"local",
  "command":"/usr/bin/bash -lc 'touch hello.txt'","cwd":"/WORKSPACE",
  "commandActions":[{"type":"unknown","command":"touch hello.txt"}],
  "proposedExecpolicyAmendment":["touch","hello.txt"],
  "availableDecisions":["accept",{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["touch","hello.txt"]}},"cancel"]}}
```

直前に `thread/status/changed {status:{type:"active",activeFlags:["waitingOnApproval"]}}` と
同じ `itemId` の `item/started`（commandExecution）が来る。

ファイル変更（サーバー → dgc）:

```
{"method":"item/fileChange/requestApproval","id":0,"params":{"threadId":"..","turnId":"..","itemId":"exec-d963..","startedAtMs":1795597438314,"reason":null,"grantRoot":null}}
```

**差分は params に入らない**。直前の `item/started`（`item.type:"fileChange"`、同じ `itemId`）の `changes` から取る。
`availableDecisions` もこちらには付かない。

応答（dgc → サーバー、`id` はサーバー要求の id）: `{"id":0,"result":{"decision":"accept"}}` / `{"id":0,"result":{"decision":"decline"}}`。
その後 `serverRequest/resolved {threadId, requestId:0}` → `thread/status/changed {activeFlags:[]}` → `item/completed`
（accept なら commandExecution `status:"completed"`、decline なら fileChange `status:"declined"`）。
decline 後もターンは続き、モデルが返答して `turn/completed status:"completed"` で終わる。

### MCP elicitation（合成）

Codex CLI 0.172.0 の `codex app-server generate-json-schema` で生成した
`McpServerElicitationRequestParams` と `McpServerElicitationRequestResponse` に基づく。
`mcpServer/elicitation/request` は `threadId` と `serverName` が必須で、
`turnId` と `environmentId` は省略または null にできる。フォームは `message` と
`requestedSchema` を持ち、URL 型は `message`、`url`、`elicitationId` を持つ。
通常フォームの `mode` は `form`。生成スキーマには拡張フォームの
`openai/form`、`openaiForm` もある。応答の `action` は `accept`、`decline`、
`cancel` のいずれかで、入力を送らない応答の `content` は null。
2026-11-27 に参照。[Codex App Server の MCP elicitation](https://learn.chatgpt.com/docs/app-server#mcp-server-elicitation-requests)

Codex のツール承認は通常フォームとして送られ、`requestedSchema`
は `{"type":"object","properties":{}}`、`_meta.codex_approval_kind` は
`mcp_tool_call`。`_meta.tool_name` がツール名、任意の `_meta.tool_params`
が引数を示す。Codex が設定する `serverName` と合わせ、これらを権限要求の要約に使う。
ツール名や引数のメタデータは第三者の MCP サーバーが影響を与え得る情報で、
`accept` を返すにはユーザーの許可が必要。`codex_requires_user_input` が true の要求と、フィールドのある要求は
入力として扱う。2026-11-27 に参照。[Codex 0.172.0 の承認要求生成](https://github.com/openai/codex/blob/rust-v0.172.0/codex-rs/core/src/mcp_tool_call.rs#L2391-L2426) [承認メタデータの定義](https://github.com/openai/codex/blob/rust-v0.172.0/codex-rs/protocol/src/mcp_approval_meta.rs)

`mcp_elicitation_*.jsonl` はこのスキーマと公式ソースに基づく合成シナリオ。
dgc と別の MCP サーバーの承認、ユーザーによる拒否、中断・終了による取り消し、
入力フォームの拒否、および要約と元データの上限を検証する。

### 中断

dgc → `{"method":"turn/interrupt","id":4,"params":{"threadId":"..","turnId":".."}}`
→ `{"id":4,"result":{}}` → `thread/status/changed {idle}` → `turn/completed {turn:{status:"interrupted","items":[],"error":null,...}}`。
中断時に流れていた agentMessage には `item/completed` が**来ない**（delta も止まる）。
アクティブなターンが無いときは `{"error":{"code":-32600,"message":"no active turn to interrupt"},"id":4}`。

### Steer

dgc → `{"method":"turn/steer","id":4,"params":{"threadId":"..","expectedTurnId":"<現ターン id>","input":[{"type":"text","text":".."}]}}`
→ `{"id":4,"result":{"turnId":"<同じターン id>"}}`。

`steer.jsonl` の順序: 旧 agentMessage の `item/started`（37 行目）→ delta 1 個（38 行目）→ `turn/steer` 送信（39 行目）→
旧 item の delta（40 行目）→ steer の応答 `{turnId}`（41 行目）→ **応答の後も旧 item の delta が続く**（42〜56 行目。送信後に計 16 個、
旧 item 全体で 17 個）→ `thread/tokenUsage/updated` 等 → 追加入力が新しい `userMessage` item（61〜62 行目）→ 新しい `agentMessage`
item（63 行目、delta 64〜65 行目）→ 新 item だけの `item/completed`（66 行目）。
**旧 item には `item/completed` が来ない**。steer 応答で旧 item のストリームが止まると仮定せず、delta は `itemId` ごとに束ね、
旧 item は完了通知なしで終わるものとして扱うこと。`turn/completed` は 1 回だけで、
`turn.id` は元のまま。アクティブなターンが無いときは `{"error":{"code":-32600,"message":"no active turn to steer"},"id":3}`。

### Resume

新しいプロセスで initialize → initialized の後:

dgc → `{"method":"thread/resume","id":2,"params":{"threadId":"..","model":"gpt-6-luna"}}`
→ `{"id":2,"result":{"thread":{"id":<同じ id>,...,"turns":[..]},"approvalPolicy":"on-request","sandbox":{"type":"workspaceWrite",..},...}}`。
approvalPolicy / sandbox を渡さなくても元の thread の値が復元された。続けて `thread/tokenUsage/updated`
（前回までの累計）、`thread/goal/cleared` が来る。その後は同じ `threadId` に `turn/start` で文脈付きのターンになる。

- `excludeTurns` を付けないと `deprecationNotice {"summary":"Full-history hydration is deprecated for paginated threads; use `excludeTurns: true` ..."}` が出る。
- `excludeTurns: true` を付けると deprecationNotice は出ず、`thread.turns` は空（`resume_exclude_turns.jsonl`）。
  **アダプタは `excludeTurns: true` を付けることを推奨**（その形でのターン実行は記録していないが、応答の構造は同じ）。

### エラー

不正モデル（`error.jsonl`）: thread/start と turn/start は成功し、ターン中に:
1. `warning {"message":"Model metadata for `no-such-model-dgc` not found. ..."}`（turn/start 直後）
2. `thread/status/changed {status:{type:"systemError"}}`
3. `error {"error":{"message":"The 'no-such-model-dgc' model is not supported when using Codex with a ChatGPT account.","codexErrorInfo":"modelNotFound","additionalDetails":null,"misalignment":null},"willRetry":false,"threadId":"..","turnId":".."}`
4. `turn/completed {turn:{status:"failed","error":{<3 と同じ error オブジェクト>}}}`

stderr にはモデルエラーは出ない。stderr には tracing ログ（ANSI カラー付き。パイプでも色コードが入る）で
設定警告や MCP 接続失敗が出るだけ。プロセスはエラー後も生きており、stdin を閉じると終了コード 0。

### レート制限

```
{"method":"account/rateLimits/updated","params":{"rateLimits":{"limitId":"codex","limitName":null,"normalModelSlug":null,
  "primary":{"usedPercent":50,"windowDurationMins":10080,"resetsAt":1700000000},"secondary":null,
  "credits":{"hasCredits":false,"unlimited":false,"balance":"0"},"individualLimit":null,"spendControlReached":null,
  "planType":"PLAN","rateLimitReachedType":null}},"emittedAtMs":..}
```

制限に達していなくてもモデル応答ごとに出る（`thread/tokenUsage/updated` の直後）。`balance` は文字列または null。
観測した `rateLimitReachedType` は `null` のみで、上限到達時の実際の通知は**未観測**。dgc は、Codex CLI 0.172.0 の
app-server 生成スキーマに基づき、次のどちらかで `rate_limited` を記録する。

- `ErrorNotification` の `params.error.codexErrorInfo` が `usageLimitExceeded` または `rateLimitExceeded` で、
  `willRetry` が false または省略されている。再試行するエラーはデバッグログのみ。
- `AccountRateLimitsUpdatedNotification` の `params.rateLimits.rateLimitReachedType` が非 null の列挙値。
  `primary` と `secondary` のうち `usedPercent >= 100` のウィンドウを優先し、その中で最も早い整数の `resetsAt` を記録する。
  どちらも 100% 未満か未報告なら両方を対象とする。`individualLimit` は対象外。

該当する列挙値を理由とし、`rateLimitReachedType: null` の通知は無視する。到達ケースは
`codex/scenarios/rate_limited_*.jsonl` の合成シナリオで検証する。

両プロバイダとも、`reason` の `[A-Za-z0-9_]` 以外の文字は `_` に置換し、先頭 48 文字を記録する。

---

## 計画の「予想」との対照

### Claude

| 予想 | 結果 |
|---|---|
| 起動フラグ `-p --input-format stream-json --output-format stream-json --verbose --permission-prompt-tool stdio [--model] [--permission-mode] [--resume]` | 確認（`--permission-mode` は未使用）。`--permission-prompt-tool stdio` は help に無いが**必須** |
| `control_request` `initialize` | 確認。応答より前に SessionStart フックの system 行が出る |
| ユーザー入力 `{"type":"user","message":{"role":"user","content":[{"type":"text",..}]}}` | 確認（`parent_tool_use_id:null`, `session_id:""` を付けて送った。省略形は未確認） |
| 中断 `control_request` `interrupt` | 確認。応答 `{"still_queued":[]}`、続いて `[Request interrupted by user]` の user 行と `result`（`error_during_execution`, `is_error:true`） |
| 権限応答 allow `{"behavior":"allow","updatedInput":..}` / deny `{"behavior":"deny","message":..}` | 確認 |
| `system/init`（`session_id`） | 確認。ただし**ターンごとに出る** |
| `assistant` | 確認。content ブロックごとに 1 行 |
| `user`（tool_result） | 確認。中断時はテキストの user 行も出る |
| `result`（`usage`, `is_error`） | 確認。**不正モデルでは `subtype:"success"` なのに `is_error:true`** |
| `control_request{subtype:"can_use_tool", tool_name, input}` | 確認。`request` の中に入る。`tool_use_id`, `permission_suggestions`, `blocked_path` 等も付く |
| レート制限（形式未確認） | `rate_limit_event` として毎回出る。制限到達時の値は未観測 |
| resume | `--resume` で同じ `session_id` が継続 |
| 未ログインエラー | 未記録（方針によりログアウトしない） |

### Codex

| 予想 | 結果 |
|---|---|
| `initialize{clientInfo:{name:"dgc",version}}` → `initialized` | 確認。`jsonrpc` フィールド不要・サーバーも付けない |
| `thread/start{cwd, model?, approvalPolicy, sandbox}` → `result.thread.id` | 確認。応答の `sandbox` はオブジェクト形 |
| `turn/start{threadId, input:[{type:"text",text}]}` → `result.turn.id` | 確認 |
| `turn/steer{threadId, expectedTurnId, input}` | 確認。応答 `{turnId}`、同一ターン内で継続 |
| `turn/interrupt{threadId, turnId}` | 確認。応答 `{}`、`turn/completed` が `status:"interrupted"` |
| `item/commandExecution/requestApproval` → `{decision:"accept"}` | 確認（`untrusted` policy で発生。`on-request`+`workspace-write` では発生せず） |
| `item/fileChange/requestApproval` → `{decision:"decline"}` | 確認（`on-request`+`read-only` で発生）。差分は params に無く item/started 側 |
| `item/permissions/requestApproval` → `{permissions, scope}` | **未記録**（2026-11-26 にスキーマ `PermissionsRequestApprovalParams`/`Response` から実装）。params は `{threadId, turnId, itemId, cwd, reason?, permissions:{fileSystem?:{read?,write?,entries?}, network?:{enabled?}}, startedAtMs}`、応答は `{permissions:<許可するプロファイル>, scope:"turn"\|"session"}`。合成シナリオ `scenarios/permissions_approval_{session,deny}.jsonl` |
| 決定値 `accept|acceptForSession|decline|cancel` | スキーマ上は存在。ただし記録したコマンド承認の `availableDecisions` は `accept`, `acceptWithExecpolicyAmendment`, `cancel` のみで、**`decline` と `acceptForSession` は提示されなかった**。コマンドへの `decline`、および `acceptForSession` は未検証 |
| 通知 `turn/started`, `item/agentMessage/delta`, `item/started`, `item/completed`, `turn/completed`, `error`, `account/rateLimits/updated` | すべて確認。`account/rateLimits/updated` は毎応答出る。使用量は `thread/tokenUsage/updated` |
| サーバー要求 id | 予想に記載なし。**0 から始まり dgc の要求 id と番号空間が別** |
| `thread/resume{threadId}` | 確認。`excludeTurns:true` 推奨（無いと deprecationNotice） |
