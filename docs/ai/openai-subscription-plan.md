# OpenAI Subscription 対応実装計画

作成日: 2026-10-02。調査基準: `develop` / `9b4ac79`、開始時の worktree は clean。
状態: 対応実装済み・実接続未検証。実装と検証の結果は [作業ノート](openai-subscription-work-note.md) を参照。
実装担当: ユーザーが切り替える Sol。モデル切替は本計画の対象外。

## 目的と採用方式

ユーザーの「OpenAI Subscription」を、ChatGPT の契約枠を dgc の推論に利用する機能として扱う。
公式の OSS 向け **Sign in with ChatGPT（SIWC）＋ public Responses API** を採用する。
対象アカウントの適格性・許可・モデル利用可否は実際の認証と推論で確認する。
全プラン・全モデルが使えるという約束はしない。[公式概要](https://developers.openai.com/siwc/token-sharing-open-source)

既存の dgc agent loop、ローカルツール、MCP client、承認、provenance、Observation Store を実行主体として維持する。
Codex app-server 経由も公式に用意されているが、今回は dgc の実行主体と状態管理を二重化しない直接方式を推奨する。
これは本リポジトリ構造に基づく設計判断。[app-server 方式](https://developers.openai.com/siwc/token-sharing-open-source/codex-app-server)

初期対象は macOS/Linux のローカル CLI/TUI、テキストと既存 function tools。画像入力、WebSocket、デバイスコード認証、VM への認証移送、OS Keychain 対応は後続とする。
Windows では保護された資格情報ストアを実装・検証できるまで当該 provider を明示的に非対応とする。既存 API-key provider は維持する。
以下の CLI 名・設定名は推奨仕様であり、現時点で存在するコマンドではない。

## 公式仕様の基準

実装開始時にリンク先を再確認する。preview の変更はこの文書に日付付きで記録し、旧 Codex クライアント ID や非公開 endpoint で代替しない。

| 項目 | 確認した仕様 / 設計への帰結 |
|---|---|
| 認証 | OSS の動的登録、PKCE、OIDC 検証、契約枠利用の granted scope が必要。API key の値を OAuth token に差し替えるだけでは足りない。[Sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in) |
| 推論 | `POST https://api.openai.com/v1/responses`。OAuth Bearer。モデル一覧は同じ資格情報で取得し、完了イベントまで読む。[Models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference) |
| 制約 | HTTP は `store:false`、`stream:true`、入力配列とクライアント側履歴を使う。非対応フィールド・ツールを送らない。[Preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations) |
| 更新・ログアウト | アカウントごとの登録管理、refresh の直列化、revocation、秘密を保護した保存。[Accounts and sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions) |
| 期限 | `expires_in` 等の返却値を保存する。期限を定数で決め打ちしない。[Token reference](https://developers.openai.com/siwc/token-sharing-open-source/token-reference) |
| エラー | 契約枠上限・権限・一時障害を区別。別課金へ自動切替しない。[Errors and recovery](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery) |
| 推論履歴 | stateless な継続でも返却された reasoning items を保持・再送する。[Reasoning](https://developers.openai.com/api/docs/guides/reasoning) |

## 現状のコードと変更境界

以下は今回コードを読んで確認した事実。テスト実行によるランタイム再現ではない。

| 現在の場所 | 現状と必要な変更 |
|---|---|
| `src/config/app.rs:158` / `loading.rs` | CLI → env → project → user の API key/base URL/model 解決。provider とアカウント選択を追加し、既存既定値への影響を隔離 |
| `src/llm/client_core.rs:18` | `OpenAIClient` は公開 String の API key を持ち `Debug` を derive。endpoint は Chat Completions 固定。認証と transport を分離、秘密の Debug を redact |
| `src/llm/client_core/network.rs` | 通常の chat_once。契約 provider では内部 SSE を最後まで集約して同じ利用体験を提供 |
| `src/llm/tool_execution/requests.rs:30` | ツール付き JSON 応答と最大100回の外側 retry。契約 provider の typed error と retry をこのループに吸収させない |
| `src/llm/stream.rs` | Chat Completions の delta 形式と文字列マーカー。Responses 用 typed event を別実装 |
| `src/llm/tool_execution/streaming.rs:24` | JSON が一時的に有効になった時点でツールを実行し得る。契約 provider は完了確認前に副作用を実行しない |
| `src/llm/types.rs` / `chat_with_tools.rs` | ChatMessage は role/content/tool_calls/tool_call_id のみ。Responses の ordered output / reasoning / namespace を保持する拡張が必要 |
| `src/llm/tool_execution/agent_loop.rs` / `subagent.rs` | 共通 chat_tools_once 経由。認証ハンドルを共有し、推論ごとの履歴・usage は分離 |
| `src/llm/compact_history.rs:151,227` | chat_once と generic chat_once_request の両経路。片方だけの対応では圧縮時に失敗する |
| `src/tui/commands/new.rs` / `src/exec.rs` | api_key の有無で client を構築。OAuth ログイン済みでも現状のままでは初期化されない |
| `src/watch.rs` / `src/tools/doc.rs` / `src/features/doc_skill/generator.rs` | 補助機能の client 構築・呼び出し。共通 factory へ接続 |
| `src/session/{data,manager,store}.rs` / `src/tui/commands/session.rs` | 会話を JSON として保存し ChatMessage に復元。新フィールドの後方互換と再開テストが必要 |
| `src/llm/{context_budget,prompt_cache,reasoning,tool_catalog}.rs` | payload 見積り、usage、reasoning 設定、遅延ツール。wire 差分を考慮 |

`rg -n 'OpenAIClient::new|api_key|chat_once_request|chat_tools_once|chat_stream' src` を各段階の移行漏れ確認に使う。
テスト用 constructor と既存 API provider の互換利用は残してよい。

## 受け入れ条件

1. API key がない状態でも login → model 一覧 → exec/TUI → tool 実行 → 最終回答が完了する。
2. 認証拒否、scope 不足、期限切れ、refresh 失敗、利用上限、ネットワーク切断を区別して復旧手順を表示する。
3. API key と OAuth が同時に存在しても明示した provider のみ使用する。勝手に別アカウント・課金方式へ切り替えない。
4. 通常推論、サブエージェント、履歴圧縮、watch、文書生成で同じ provider 選択が適用される。
5. 複数 tool call、セッション再開、圧縮、キャンセルで call_id・未消費結果・reasoning 継続状態を壊さない。
6. token/code/verifier をログ、TUI、session、provenance、RepoMap、Git に保存しない。資格情報専用ストアだけに保存する。
7. 現行 API-key/OpenAI-compatible 設定と保存済みセッションが読め、既存テストが通る。
8. mock と実アカウント確認の結果を分けて記録する。実アカウント未確認なら「対応実装済み・実接続未検証」とする。

## 提案するユーザー操作

```text
dgc auth login openai
dgc auth status openai
dgc auth list openai
dgc auth use openai <account-label>
dgc auth logout openai [account-label]
dgc models --provider openai
dgc --provider openai --model <catalog-slug> exec "このプロジェクトの構成を説明して"
dgc --provider openai --model <catalog-slug>
```

- `auth login` は新規登録と既存ラベルの再認証を区別する。既存アカウント用 `--account`、追加用 `--new-account` を設ける。
- ブラウザ開始前に loopback listener を用意する。開けない環境では `--no-browser` で手動開始手順を表示する。認証 URL は永続ログに残さない。
- login 成功だけでは既存の provider 設定を書き換えない。最初は明示的な `--provider` と `--model` を使う。
- 通常の `exec/watch/run` は認証が必要なら終了して login を案内し、勝手にブラウザを起動しない。
- `auth status` の既定動作はローカル状態確認。期限内という表示を「サーバーが利用を許可した」と同一視しない。
- TUI は使用中 provider・アカウントラベル・モデルを表示し、未ログイン時に CLI 手順を案内する。初版の login 自体は CLI で行う。
- provider 未指定は既存 API-key 方式。新設定の優先順位は CLI → `DGC_PROVIDER` → project → user → 既存既定。
- アカウントの選択と秘密の保存先はユーザー領域のみ。project config で credential path、issuer、token URL、account を上書きさせない。
- 契約 provider は `OPENAI_API_KEY` を使わない。明示的 `--api-key` との併用は入力エラー。契約用モデルが未指定なら一覧・選択方法を示し、`gpt-4o-mini` を暗黙流用しない。
- 契約 provider の接続先は公式 endpoint に固定。既存 custom base URL は当該 provider で使用しない旨を通知。任意 endpoint は DI されたテストだけに許可する。
- `auth` と `models` は RepoMap/MCP/agent loop 起動前に分岐。`auth` の基本操作は不正な project 設定にも依存しない。

## 設計と実装タスク

### P0: 仕様・通信契約の固定

変更先: 本文書と `docs/ai/openai-subscription-work-note.md`（実装開始時に作成）。

- 公式 SIWC docs の再確認。namespace function call の request/response、OIDC discovery/JWKS、SSE terminal event の実際の型を確認し、秘密を含まない固定 fixture を用意する。
- dgc の `tool_search` はローカル function であり hosted Responses `type:tool_search` とは別物。wire で namespace 内 function として送ることを確認する。名前が同じだけで非対応と断定しない。
- `reasoning.effort`、namespace、encrypted reasoning の契約ルートでの可否を最小通信で確認する手順を準備。実ログインは後のスモークで実施し、未確認は記録する。
- `Cargo.toml` は Edition 2024/MSRV 1.88。OIDC/JWT、SHA-256、OS lock の不足を確認し、必要な依存だけ選ぶ。版と MSRV は実装時の公式 crate 情報で確定する。暗号・署名検証を自作しない。

完了条件: 未確認事項、request fixture、段階ごとの check が作業ノートにある。外部仕様の推測で実装を進めない。

### P1: provider と認証の境界

新規の主責務は `src/features/openai_subscription/` に置く（`mod.rs`, `auth.rs`, `credentials.rs`, `models.rs`, `responses.rs`, `sse.rs`, `error.rs`, `tests/` 等）。
既存 `src/llm/` は agent loop と互換 facade、`src/config/` は設定を担当する。

- `ProviderKind::{OpenAiCompatible, OpenAiChatGpt}`、`AuthHandle`、共通 client factory を追加する。
- `OpenAIClient` の呼び出し側互換はできるだけ保ち、provider 別の型付き request/result に分岐する。serde JSON を generic に渡す経路は内部の正規化 request に寄せる。
- AuthHandle は `Arc` で共有し request ごとに有効な Bearer を得る。token を client clone 時に固定コピーしない。
- 秘密型には独自の redact Debug。`AppConfig`、CLI 引数、HTTP error、tracing の全経路で漏れを確認する。
- request 全文の DEBUG 出力に OAuth/token response や opaque reasoning が入らないよう、契約 provider は安全な診断項目だけ出す。

完了条件: fake AuthHandle で client を作成でき、既存 provider の wire/body と動作が変わらない。

### P2: OAuth と資格情報ライフサイクル

- `AuthService` に `Clock`、`CredentialStore`、HTTP client、browser opener を注入し、テストで実 HOME/ブラウザを使わない。
- ユーザー用保存先は `dirs::config_dir()/doge-code/openai/` を提案。Unix directory 0700/file 0600、原子的書換え、所有者/シンボリックリンク検査、ファイルサイズ上限を実装する。
- host ID はランタイムに固定し、registration の識別子と分離。アカウント key は validated issuer/sub と issued client ID に紐付け、メールを一意キーにしない。
- 初回 `dynamic_agent_client` → callback の issued client ID、再認証は保存済み ID。各試行の PKCE S256/state/nonce、callback URI、有効期限を一時状態として持つ。
- loopback `127.0.0.1` の `/auth/callback`、試行中固定の port/URI を使用。state 不一致・重複 query・過大 request・期限切れ・callback 再利用を拒否し、listener を必ず回収する。
- ID token の署名/JWKS/issuer/audience/expiry/nonce と selected identity を検証してから credential を置換。token response の granted scopes で利用許可を判定する。
- scope 不足は「ログイン済み・契約枠利用未許可」として保持する。必要な再同意は明示操作時だけ行う。
- 保存する token set は access/refresh/ID token、scope、client ID、期限、`earliest_refresh_at` 等。通常の AppConfig/session に埋め込まない。
- refresh はプロセス内 single-flight とアカウント単位の OS file lock で直列化。lock 後にディスクを再読込し、他プロセスの更新済み token を再利用する。lock 待ちにも timeout/cancel を適用。
- refresh 成功は token set 全体を原子的に更新。logout/アカウント切替と競合して古い token が復活しないよう generation とロック境界を設ける。
- refresh 応答喪失、書込失敗、terminal refresh error を区別する。回転前 token を無限再送しない。失敗時に他アカウントの資格情報を消さない。
- logout は新規リクエストを停止し、discovery の revocation endpoint で失効を試み、ローカル token を削除する。未確認の遠隔失効は明示。client mapping と host ID は保持する。

認証フローのフィールド・再認証条件の正本: [Sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)。更新と失効の正本: [Accounts and sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions)。

完了条件: 初回/再認証/拒否/不正 callback/署名不正/アカウント混同/同時 refresh/logout 競合をローカル fake server と tempdir で再現できる。

### P3: Responses request とツール変換

- `ResponsesRequest` は明示フィールドの allowlist で構築する。Chat Completions body に追加/削除して使い回さない。
- instructions/system content は順序・権限を保持して `instructions` または developer message に写像。user/tool の文字列を developer に昇格させない。runtime hints は request overlay のまま扱う。
- user/assistant text、function call、function output をそれぞれ Responses item に写像。call item の `id` と実行対応用 `call_id` を区別する。
- dgc の ToolDef は安定 namespace（提案 `dgc`）内の function に変換。名前・schema・順序を保持し、既存の非 strict schema は明示的な `strict:false` を検討・検証する。
- namespace とローカル名の対応を型付きで管理し、未知 namespace/call_id/重複 ID は実行前に拒否する。MCP alias を壊さない。
- ローカル tool routing の有効化後、次リクエストに確定的な schema を載せる。hosted MCP、native tool_search、programmatic_tool_calling へ変換しない。
- `store:false` と `stream:true` を常に設定し、HTTP では必要な履歴を全送信する。`previous_response_id` を継続の根拠にしない。
- preview の非対応パラメータを serializer test で検出する。特に temperature、max_output_tokens、metadata を既存設定から混入させない。
- reasoning は現在の policy 判定を使い、wire は `reasoning.effort` に変換。モデル能力が不明な場合は勝手な高 effort を強制しない。

完了条件: ordinary text / 複数 tool / MCP alias / tool discovery / runtime hints の golden payload が一致し、禁則フィールドがない。[仕様制約](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)

### P4: SSE と retry の完了セマンティクス

- `InferenceEvent` を型付きにする。TextDelta、FunctionArgumentsDelta、OutputItem、Usage、Completed、Failed、Incomplete を区別する。
- byte 境界、UTF-8 分割、CRLF、複数 data 行、heartbeat、未知イベント、フレーム上限に対応する。最大応答サイズにも境界を設け、超過は明示エラー。
- text は逐次表示してよいが、ツール呼び出しは item/call_id ごとに集約し **response.completed を確認してから** 既存 dispatcher へ渡す。
- 引数の JSON が途中で一度有効になっただけでは実行しない。terminal event 前の EOF、failed、incomplete、cancel は成功扱いしない。
- UI 非ストリームの呼び出しでも内部は同じ SSE reader で集約。既存 `__TOOL_CALLS_DELTA__` 文字列プロトコルを新 provider に持ち込まない。
- `ProviderError` に HTTP status、error code/param、request ID、retryability、stream 開始/完了状態を保持。サーバー本文は長さ制限・redact の後に診断として扱う。
- 契約枠上限と不適格/権限不足は停止。一時 503/通常 rate limit は回数と総時間で制限した backoff。401 は credential 更新が妥当な場合のみ最大1回更新し、それでも失敗なら診断する。
- terminal refresh error、明示拒否、不正 body、stream 開始後の失敗を外側100回 retryへ流さない。二重 retry 層を作らず、既存側にも non-retryable 判定を通す。
- cancel は通信中・SSE待機・retry sleep・refresh lock 待ちを解除する。失敗後の自動再送でツール副作用を重複させない。

完了条件: HTTP 200 → text delta → subscription usage error を失敗と判定し、ツール未実行・履歴未消費を維持できる。[エラー仕様](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery)

### P5: 履歴・再開・圧縮

ここは認証追加と同程度に重要。先に [context/history skill](skills/dgc-context-history/SKILL.md) と [contracts](contracts.md) を読む。

- 提案: ChatMessage の assistant turn に optional/versioned な `provider_state` を追加。Responses の ordered output items（reasoning/encrypted content、message、function call、namespace/phase 等）を保持する。旧 JSON は serde default で読む。
- 表示用 content/tool_calls は projection。契約 provider の request builder は raw output を正本として再送し、projection と二重送信しない。raw JSON は既知 output item に限定し、未知型は失敗または明示的な保持方針を決める。
- `ChoiceMessageWithTools` → agent loop → HistoryManager → SessionManager → resume の全経路で state を運ぶ。client のグローバルな last-response cache に置かない。
- session owner/provider/model を記録するが秘密は記録しない。アカウント/provider 切替時は新セッションを基本とし、opaque state を異なる接続へ暗黙送信しない。旧セッションを続ける場合は元の選択へ戻す案内を出す。
- 圧縮・古い tool output の退避は call/result と output block の対応を保つ。未消費の最新ブロックは保持し、完了した過去ブロックだけを一体として要約・退避する。
- reasoning の opaque content を文字列要約、復号、ログ出力しない。保持対象の output items は返却順序のまま再送する。
- governor は Responses instructions/input/tools を含む実送信形状を評価する。opaque ciphertext 長をそのままトークン数にしない。usage と保守的推定を区別し、上限不明モデルは明示的 context 設定を案内する。
- usage の input/output/cached/reasoning を既存 Usage に変換し、欠落と0を区別する。完了時に1回加算し、cached tokens は context 使用量から差し引かない。
- API費用表示と契約利用量を分ける。token数から契約残量・リセット時刻・無料を推定しない。

完了条件: tool call → tool result → 再起動 → 継続、複数 call、圧縮、stream失敗、Observation回収で履歴の参照関係を保てる。

### P6: 全呼び出し元と UI への接続

- `main.rs` の auth/models early dispatch、provider CLI、FileConfig/AppConfig、TUI/exec/watch/doc の client factory を配線する。
- 要約・symbol edit・doc generation の「ツール無し推論」も Responses adapter に接続する。JSON文字列を期待する呼び出し側の既存契約を守る。
- `enable_stream_tools` の両値で契約 provider の completion gate を守る。新 provider で旧 streaming executor に戻る経路を残さない。
- モデル一覧は account ごとに分離。`models` array の `visibility:list`、display_name、slug、サーバー順序を扱い、切替時は再取得する。API-key の `data` 形式と混同しない。
- TUI と exec JSON は失敗を成功文にしない。stdout の JSON に login案内/進捗を混ぜず、診断は stderr。安全な request ID と recovery action を示す。
- ローカル token usage から公式利用管理へのリンクを提供する。README に対象環境、setup、provider の選び方、上限時の動作、logout、preview 制約を記載する。

完了条件: 全 entry point の mock 統合テストが API key 無しで動き、API-key 回帰テストも通る。[モデル取得・完了条件](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)

### P7: 検証・実接続・引き継ぎ

実装中は段階ごとの関連フィルタを先に実行する。新設モジュール名を変えた場合はテスト名も読み替え、0件成功を認めない。

```bash
bash scripts/verify.sh test features::openai_subscription::
bash scripts/verify.sh test config::
bash scripts/verify.sh test llm::client_core::
bash scripts/verify.sh test llm::tool_execution::requests::
bash scripts/verify.sh test llm::tool_execution::history::
bash scripts/verify.sh test llm::context_budget::
bash scripts/verify.sh test session::
bash scripts/verify.sh test tui::
cargo fmt --all
bash scripts/verify.sh rust
bash scripts/verify.sh guidance
bash scripts/verify.sh msrv
bash scripts/verify.sh tui-deps
bash scripts/verify.sh macos
```

追加の重要ケース:

| 境界 | 必須ケース |
|---|---|
| Auth | wrong state/nonce/aud/iss/signature、callback再利用、scope無し、同一emailの別登録、キャンセル |
| Credential | 0600/0700、既存symlink、不完全書込、2プロセスrefresh、refreshとlogout競合、Debug/logへの漏れ |
| HTTP/SSE | 401/403/429/503、detail-only body、event内error、EOF、incomplete、UTF-8分割、oversize、cancel |
| Tools | call_id保持、namespace解決、複数call、重複delta、tool_search有効化、terminal前に実行0回 |
| History | raw/projection二重送信無し、旧session、resume、未消費結果、圧縮、subagent混線無し |
| Routing | API key/OAuth共存、custom base URLへtokenが送られない、env/project設定競合、文書生成/要約経路 |

実アカウントの手動確認は専用の一時プロジェクトで行う。login のユーザー本人によるブラウザ同意は自動テストで代替できない。
確認順: login → model一覧 → 短い会話 → read-only tool → 一時ファイル編集とテスト → resume → 圧縮 → refresh → logout。
TUI の通常表示・未認証・利用制限エラーを端末録画またはスクリーンショットで確認する。認証画面、URL、個人情報は録画に含めない。
利用上限を故意に使い切らず、上限分岐は mock で検証する。Linux CI と macOS の結果を分け、未実施ゲートを明記する。

## Sol に渡す実行指示

> この計画に沿って doge-code に OpenAI Subscription 対応を実装してください。
> 最初に AGENTS.md、現在の差分、計画基準からのコード変更、公式 SIWC 仕様を確認してください。
> P0→P7 の順で進め、関連 Skill を必要な境界で読み、作業ノートに設計判断・完了チェック・未確認事項を残してください。
> API-key 互換、認証秘密の保護、契約課金の明示選択、Responses完了後だけのtool実行、履歴再開を必須条件とします。
> 計画とコードにずれがあれば根拠を記録して局所的に修正してください。既存のユーザー変更を巻き戻さないでください。
> 実アカウント認証に必要な本人操作以外は、実装とmock検証を継続してください。
> 実接続ができない場合は実装済み部分と実接続未検証を分けて報告してください。
> サブエージェント利用、commit/push/PR作成はこの計画からは指示しません。

## この計画作成時の検証記録

- ローカルコード・関連ガイダンス・現行公式ページを確認した。
- API-key専用初期化、Chat Completions固定、retry/stream/history の移行境界はコードで確認した。
- 実装前なので Rust runtimeテスト・OAuthログイン・推論の成功は主張しない。
- `bash scripts/verify.sh guidance`: PASS（ガイダンス整合検査＋開発スクリプト28テスト）。macOS/Python 3.13。
