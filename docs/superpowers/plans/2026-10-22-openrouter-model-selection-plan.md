# OpenRouter Model Selection Implementation Plan

> 実装担当者向け: 各タスクを、失敗するテストの追加、失敗確認、最小実装、成功確認、コミットの順に進める。チェックボックスで進捗を管理し、最後に変更全体をレビューする。

**Goal:** OpenRouter 専用 provider と、会話・認証情報を安全に分離した CLI/TUI モデル切替を追加する。

**Architecture:** 既存の Chat Completions と検証付き agent loop を利用し、OpenRouter 固有の設定、モデルメタデータ、リクエスト形式を小さな専用モジュールに閉じ込める。モデル一覧は認証不要の独立したカタログとして取得・キャッシュし、既存 picker を拡張する。モデル切替は保存を先に確定する新規セッション遷移とし、既存 provider の経路を変えない。

**Tech Stack:** Rust 2024、MSRV 1.94.0、既存の reqwest、tokio、serde、dirs、tempfile、chrono、crossterm、JobManager。新しい依存は追加しない。

**Spec:** `docs/superpowers/specs/2026-10-22-openrouter-model-selection-design.md`。2026-10-22 に提示した全15節の仕様書が承認済み。本計画と仕様書の両方を読む。添付時点の仕様書に残る「レビュー待ち」は当初の提出状態であり、その後の承認を本計画で記録する。

- 計画作成: Astra
- 実装担当: Codex（gpt-6.1-sol）
- 対象: mopemope/doge-code の最新 develop から分岐する feature branch
- 計画格納先: `docs/superpowers/plans/2026-10-22-openrouter-model-selection-plan.md`
- 状態: 計画作成・自己レビュー済み。実装未着手。本計画のレビュー後に指定済みの実装担当へ引き渡す。
- 承認済みの到達点: 実装、テスト、レビュー、PR 作成まで。マージと develop への直接更新は含まない。

## Global Constraints

- モデルを切り替えるときは新しい会話を開始する。切替前の会話履歴、保存済みセッション、未送信入力を保持する。
- 既定のモデルは用意しない。
- CLI、TOML、`DGC_PROVIDER` の値は `openrouter` とし、別名は追加しない。
- 固定ベース URL は `https://openrouter.ai/api/v1` とする。
- 推論先は `/chat/completions`、カタログ取得先は `/models` とする。
- 今回は `OPENROUTER_BASE_URL` を追加しない。
- リダイレクトを追従しない。
- API キーは CLI > `OPENROUTER_API_KEY` > 同一ソースに openrouter を明示したプロジェクト設定 > 同条件のユーザー設定。モデルは `OPENROUTER_MODEL` を用いる同じ順位。
- 空文字・空白のみの値は未設定として扱う。
- `OPENAI_*`、`OPENCODE_*`、OAuth の資格情報は流用しない。
- モデル ID は 1〜256 バイト、ASCII 英数字と `-_.~:/` のみ。スラッシュを含み、その両側は空でない。
- カタログは接続5秒・全体10秒・1操作1試行・展開後8 MiB・10,000件まで。ID256バイト、表示名256 Unicode scalar values、価格文字列128バイトまで。
- キャッシュは OS ユーザーキャッシュ配下 `doge-code/openrouter/models-v1.json`、TTL24時間、取得時刻が5分を超えて未来なら無効、読み込み上限8 MiB。
- 起動、セッション再開、推論のためだけにカタログ HTTP を暗黙に送らない。
- OpenRouter 専用 provider は常に Chat Completions を使用する。
- ツールを使う要求には `provider.require_parameters=true` を設定する。
- 今回はネイティブ SSE を追加せず、汎用 SSE パーサーも変更しない。
- カタログとセッションに秘密情報を保存しない。未知の能力・価格・usage を false や0と同一視しない。
- `opencode::default_base` は OpenCode 専用のままにする。
- 新しい依存は追加しない。
- 実際のキー、利用者の設定、有料推論は使用しない。
- 実装の導入は feature branch と PR を経由する。develop へ直接更新しない。

OAuth、キーチェーン、provider trait の全面変更、同一会話内切替、お気に入り・プリセット、自動モデル選択、予算操作、OpenRouter Responses/Messages、課金を伴う試験は対象外とする。

## Review Focus

1. `exec --resume` の指定は現状、設定読込より後で cfg に反映される。モデル未指定での OpenRouter 再開は保存モデルを先に復元し、新規作成へ逃げないこと。Task 4 の再開テストで固定する。
2. カタログを閉じた直後の完了通知と、別 provider へ移った後の古い通知が新しい画面を上書きしないこと。Ctrl-C の独立経路も対象にする。Task 7 の競合テストで固定する。
3. `reasoning` と `reasoning_content` が同時に存在する応答、usage が欠ける応答でも、詳細を壊したり未報告値を0にしたりしないこと。Task 3 の応答テストで固定する。
4. 圧縮後の小さな応答、重複 ID、未来時刻、保存失敗によって、正常なキャッシュや選択済み会話を失わないこと。Task 2 の境界テストで固定する。
5. OpenRouter を追加するための共通化が Go/Zen のキー共有や PR #177 の implicit/explicit provider 境界を変えないこと。Task 1、Task 3、Task 9 の回帰テストで固定する。

## 調査済みの現在地

2026-10-22 16:21 UTC に GitHub の develop、open PR、最新 CI を再確認した。develop は PR #177 マージ後から変更なし、open PR は0件、[develop CI 39808010067](https://github.com/mopemope/doge-code/actions/runs/39808010067) は7 jobsすべて成功している。実装時にも再確認し、固定 SHA をチェックアウトしない。

現在の主な差分点は次のとおり。

- `src/config/app.rs::resolved_api_for_model` と `from_cli` は OpenCode 用判定を利用している。OpenRouter を同じ判定へ混ぜない。
- `src/features/model_selection/mod.rs::ModelPicker::results` は Go/Zen の静的参照を返す。動的カタログには所有型が必要。
- `src/tui/commands/model_selection.rs::start_model_session` には新規会話を保存してから公開する遷移が既にある。これを拡張する。
- `src/tui/commands/new.rs::construct_with_session_manager` は現在クライアントを再開より先に生成する。OpenRouter の保存モデル優先を妨げないよう、OpenRouter の再開時だけ生成を遅延させる。
- `src/llm/tool_execution/streaming.rs` の汎用経路は途中の引数からツールを実行できるため、OpenRouter をそこへ流さない。
- TUI の通知は文字列 channel。domain payload は専用 store に置く既存パターンがあり、カタログ JSON を通知やログへ埋め込む必要はない。

この計画作成ではテストは実行していない。CI の観測結果と、以下の実装後に必要な検証を混同しない。

## ファイルと責務の対応

| 分類 | ファイル | 責務 |
| --- | --- | --- |
| 新規 | `src/features/openrouter/mod.rs` | 固定接続先、ID、設定補助、メタデータ型、reasoning の解決、OpenRouter 応答正規化 |
| 新規 | `src/features/openrouter/catalog.rs` | 公開 HTTP、上限、正規化、キャッシュ、一覧・詳細の安全な表示 |
| 新規 | `src/features/openrouter/tests.rs` | provider・プロトコルの単体回帰。catalog は同ファイル内の tests も利用 |
| 変更 | `src/features/mod.rs`、`src/features/openai_subscription/mod.rs` | モジュール登録と ProviderKind の追加 |
| 変更 | `src/config/app.rs`、`src/config/inference_diagnostics.rs`、`src/config/tests.rs` | 設定分離、メタデータ参照、診断、回帰 |
| 変更 | `src/features/model_selection/mod.rs` | 所有型 picker、純粋な状態遷移、共通の選択補助 |
| 変更 | `src/llm/client_core.rs`、`src/llm/client_core/network.rs`、`src/llm/tool_execution/requests.rs` | 接続経路、リクエスト差分、共通検証、応答処理 |
| 変更 | `src/llm/stream.rs`、`src/llm/tool_execution/streaming.rs` | OpenRouter だけのバッファ応答・標準ループへの分岐 |
| 変更・回帰 | `src/llm/capabilities.rs`、`src/llm/reasoning.rs`、`src/llm/types.rs` | 既存の汎用意味論を維持し、専用 provider の能力を推測しない |
| 変更 | `src/session/selection.rs`、`src/session/data.rs`、`src/exec.rs`、`src/exec_resume_model_tests.rs` | 保存モデル、binding、再開順序、exec 回帰 |
| 変更 | `src/tui/commands/{new,core,model_selection,model_selection_tests,session_state}.rs` | クライアント準備、原子的遷移、カタログ結果 store の所有 |
| 新規 | `src/tui/commands/model_catalog.rs` | ジョブ起動、結果 store、完了・キャンセル処理とテスト |
| 変更 | `src/tui/commands.rs`、`src/tui/commands/handlers/dispatch.rs` | module 登録、handler のカタログ分岐 |
| 変更 | `src/tui/commands/handlers/slash_commands/{models,help}.rs` | slash command、help |
| 変更 | `src/tui/event_handlers/model_picker.rs`、`src/tui/event_loop.rs`、`src/tui/rendering.rs` | キー操作、Ctrl-C 経路、描画 |
| 変更 | `src/jobs/types.rs` | ModelCatalog kind の追加。汎用 manager の payload 設計は変えない |
| 変更 | `src/main.rs`、`tests/model_catalog_cli.rs`、`README.md` | CLI の早期分岐、help、利用方法 |
| 保存 | 上記仕様書・本計画の予定パス | 承認済み契約と実装判断を feature branch に同梱 |

既存ファイルの移動や無関係な整理はしない。行番号ではなく上記の symbol と責務で位置を特定する。

## 実装で共有する型と境界

以下は新規インターフェースの契約であり、関数本体の実装ではない。型名・引数・戻り値は各タスクで統一する。

### OpenRouter のドメイン型

`src/features/openrouter/mod.rs` に次を置く。

- `Support::{Supported, Unsupported, Unknown}`。
- `EffortSupport::{Unknown, GatewayAccepted, Listed(Vec<String>)}`。メタデータの省略と明示的 null を区別する。
- `ReasoningMetadata { supported_efforts: EffortSupport, mandatory: Option<bool> }`。
- `CatalogPrices { prompt: Option<String>, completion: Option<String>, request: Option<String>, conditional: Option<bool> }`。
- `OpenRouterModel { id: String, name: Option<String>, input_modalities: Option<Vec<String>>, output_modalities: Option<Vec<String>>, supported_parameters: Option<Vec<String>>, context_window: Option<u32>, prices: CatalogPrices, reasoning: ReasoningMetadata }`。
- `SelectedModelMetadata { provider: ProviderKind, fetched_at: chrono::DateTime<chrono::Utc>, model: OpenRouterModel }`。ランタイムでは `Option<Arc<SelectedModelMetadata>>` で共有し、完全な provider/model 一致を必須にする。
- `DecodedResponse { message: ChoiceMessageWithTools, usage: Option<Usage>, prompt_tokens: Option<u32> }`。既存 Usage の必須カウンターを、欠損時に0で埋めない。

`AppConfig` と `OpenAIClient` に `openrouter_metadata: Option<Arc<SelectedModelMetadata>>` を追加する。設定ファイルや SessionData へこのフィールドをシリアライズしない。既定値は None。

### カタログ型

`src/features/openrouter/catalog.rs` に次を置く。

- `CatalogSnapshot { fetched_at: DateTime<Utc>, models: Vec<OpenRouterModel>, excluded_entries: usize }`。
- `CatalogPolicy::{PreferCache, Refresh, Offline}`。
- `CatalogFreshness::{Fresh, Stale}`。
- `CatalogResult { snapshot: Arc<CatalogSnapshot>, freshness: CatalogFreshness, offline: bool, warning: Option<CatalogWarning> }`。
- `CatalogError` と `CatalogWarning` は固定の安全な表示を持つ。HTTP 本文、reqwest の接続先詳細、認証値を保持したまま UI へ表示しない。
- `CatalogClient` は認証情報を持たない専用 reqwest client と、任意の cache path を持つ。

`CatalogClient` の本番生成では固定 URL のみを使用する。ローカル HTTP server と時計・保存先の注入はテスト用の非公開境界に限定し、環境変数や CLI の本番 backdoor を追加しない。

## Task 0: 最新状態と隔離された作業場所を確認する

**Files:** 読み取り: `AGENTS.md`、`docs/ai/workflow.md`、関連する `docs/ai/skills/`、`.github/workflows/ci.yml`、`Cargo.toml`。保存: 承認済み仕様書と本計画。

**Interfaces:** 入力は最新 develop と二つの文書。出力は、既存作業を保持した feature branch の作業場所と、対象の再確認結果。

- [ ] リモート、develop、最新 CI、open PR を読み取り、既に実装された重複作業がないことを確認する。
- [ ] 指定された実行環境で dirty/untracked を確認し、無関係な変更を保護した隔離作業場所を使う。既存 worktree を reset/clean しない。
- [ ] 仕様書と計画を予定パスへ保存し、仕様書冒頭の状態だけを「承認済み」に更新する。本文の仕様は変えない。
- [ ] `bash scripts/verify.sh guidance` を実行し、終了0を確認する。
- [ ] 文書のみを commit する。推奨メッセージ: `docs: record approved OpenRouter design and plan`。

最新コードに前提と矛盾する変更があれば、該当差分だけ再評価する。認証境界、課金経路、セッション保存方式、応答方式の変更が必要なら、コードへ押し込まず実装前に報告する。

## Task 1: Provider と設定分離を追加する

**Files:** 新規 `src/features/openrouter/{mod,tests}.rs`。変更 `src/features/mod.rs`、`src/features/openai_subscription/mod.rs`、`src/config/{app,inference_diagnostics,tests}.rs`、`src/features/model_selection/mod.rs`。

**Interfaces:** 上記ドメイン型を定義する。次の関数を提供する。

- `openrouter::validate_base(base: &str) -> anyhow::Result<()>`
- `openrouter::model_id(model: &str) -> anyhow::Result<&str>`
- `openrouter::inference_binding(model: &str) -> anyhow::Result<String>`
- `model_selection::config_value<T>(provider: ProviderKind, source: Option<ProviderKind>, value: Option<T>) -> Option<T>`
- `model_selection::provider_name(provider: ProviderKind) -> &'static str`
- `model_selection::missing_auth_message(provider: ProviderKind) -> &'static str`

新しい config_value は OpenRouter に関係する組み合わせだけを専用スコープに制限し、それ以外は既存 OpenCode 関数の意味論をそのまま利用する。opencode::default_base の戻り値は変更しない。

- [ ] `openrouter_config_isolates_sources_and_has_no_default_model` を追加する。CLI、環境変数、同 provider の project/user、blank、異なる provider、provider 未指定ソースをテーブル駆動で検証する。合成キーの値ではなく取得元も確認する。
- [ ] `openrouter_endpoint_and_model_id_are_strict` を追加し、次の契約を固定する。

```rust
assert!(openrouter::validate_base("https://OPENROUTER.AI:443/api/v1/").is_ok());
assert!(openrouter::validate_base("https://openrouter.ai.example.invalid/api/v1").is_err());
assert!(openrouter::validate_base("https://user@openrouter.ai/api/v1").is_err());
assert!(openrouter::validate_base("https://openrouter.ai/api/v1?token=x").is_err());
assert!(openrouter::model_id("publisher/model:free").is_ok());
assert!(openrouter::model_id("~publisher/model").is_ok());
assert!(openrouter::model_id("publisher/").is_err());
assert!(openrouter::model_id("publisher/model\n").is_err());
```

256/257バイト、HTTP、異なるポート、別 path、fragment も同じテスト表に含める。

- [ ] `bash scripts/verify.sh test openrouter` を実行し、新規機能が未実装であるため失敗することを確認する。コンパイルエラーと assertion 失敗を区別して記録する。
- [ ] ProviderKind::Openrouter と純粋関数、設定解決を実装する。OpenRouter の resolved_api_for_model は Chat 固定。通常起動のモデル不足はエラー、明示的な再開だけは Task 4 の復元までモデル確定を遅延できるようにする。診断は固定 endpoint、キー有無・取得元、課金ラベルを表示し、キー値を出さない。
- [ ] `bash scripts/verify.sh test openrouter`、`bash scripts/verify.sh test config::`、`bash scripts/verify.sh test opencode::` を実行し、各フィルターで1件以上実行・失敗0を確認する。PR #177 の implicit official Responses と explicit compatible Chat の既存テストも通す。
- [ ] 関連ファイルだけ commit。推奨: `feat: add isolated OpenRouter provider configuration`。

**完了条件:** 新しい環境変数の存在だけでは provider が変わらず、OpenCode/OAuth の資格情報取得処理は呼ばれない。OpenRouter の missing-model を既存既定値で埋めない。

## Task 2: 上限付きカタログとキャッシュを実装する

**Files:** 新規 `src/features/openrouter/catalog.rs`、変更 `src/features/openrouter/mod.rs`。単体テストは catalog.rs 内。

**Interfaces:** Task 1 の型を使用し、次を提供する。

- `CatalogClient::new(cache_path: Option<PathBuf>) -> Result<Self, CatalogError>`
- `CatalogClient::load(&self, policy: CatalogPolicy, cancel: CancellationToken) -> impl Future<Output = Result<CatalogResult, CatalogError>>`
- `default_cache_path() -> Option<PathBuf>`
- `read_cached(path: &Path, now: DateTime<Utc>) -> Result<Option<CatalogResult>, CatalogError>`
- `parse_catalog(bytes: &[u8], fetched_at: DateTime<Utc>) -> Result<CatalogSnapshot, CatalogError>`
- `CatalogSnapshot::selected_metadata(&self, id: &str) -> Option<Arc<SelectedModelMetadata>>`
- `format_catalog(result: &CatalogResult, exact_model: Option<&str>) -> anyhow::Result<String>`

- [ ] `openrouter_catalog_preserves_unknowns_and_exact_ids` を追加する。`architecture.input_modalities/output_modalities`、`supported_parameters`、`context_length`、`pricing`、`reasoning.supported_efforts/mandatory` を取り込む。存在しない配列と空配列、明示 null の supported_efforts、0/負数/不正型の context、価格未報告を区別する。未知項目を受け付け、ID は大小文字・suffix を保持する。
- [ ] `openrouter_catalog_limits_and_cache_failures` を追加する。8 MiBちょうどと1バイト超、展開後超過、10,000/10,001件、重複有効ID、不正ID除外、全件除外、表示名256/257 scalar、価格128/129バイトを検証する。任意フィールドの不正・上限超過は不明として扱い、IDの不正は項目を除外する。正常な旧キャッシュが失敗した更新で変わらないことを byte 比較する。
- [ ] `openrouter_catalog_policy_and_cancellation` を追加する。時計を固定し、TTL24時間の境界、未来5分以内/超過、fresh取得0回、stale取得1回、refresh1回、offline0回、失敗時stale、キャッシュなし失敗、書込失敗時メモリ利用、キャンセルを確認する。
- [ ] テストを実行して失敗を確認する。コマンド: `bash scripts/verify.sh test openrouter_catalog`。
- [ ] 専用 reqwest client、chunk 読み取り中のサイズ検査、単発取得、キャッシュの正規化・atomic rename を実装する。非2xxやredirectは失敗。旧キャッシュに失敗本文を保存しない。cache serialization 自体も読込上限に収まることを確認し、保存不能ならメモリ成功＋警告とする。
- [ ] 同じ focused test を再実行する。ローカル HTTP server が Authorization/Cookie/推論ヘッダーを一切受信せず、送信先がその server 一か所であることを assert する。表示の制御文字除去には既存 safe_display を利用し、canonical ID を変更しない。
- [ ] 関連ファイルだけ commit。推奨: `feat: add bounded public OpenRouter model catalog`。

**完了条件:** キャッシュは公開メタデータだけを持つ。カタログ取得は設定やクライアント生成に依存せず、再試行や有料推論を起こさない。

## Task 3: 検証付き Chat と reasoning を接続する

**Files:** 変更 `src/features/openrouter/{mod,tests}.rs`、`src/llm/client_core.rs`、`src/llm/client_core/network.rs`、`src/llm/tool_execution/requests.rs`、`src/llm/stream.rs`、`src/llm/tool_execution/streaming.rs`。回帰 `src/llm/{types,reasoning,capabilities,history}.rs`、`src/llm/tool_execution/history.rs` と既存 tool execution tests。

**Interfaces:**

- `openrouter::resolve_effort(metadata: Option<&SelectedModelMetadata>, model: &str, mode: ReasoningMode, requested: Option<ReasoningEffort>) -> anyhow::Result<Option<ReasoningEffort>>`
- `openrouter::adapt_request_value(request: &mut serde_json::Value, metadata: Option<&SelectedModelMetadata>, model: &str, mode: ReasoningMode, requested: Option<ReasoningEffort>, has_tools: bool) -> anyhow::Result<()>`
- `openrouter::decode_response(value: serde_json::Value) -> anyhow::Result<DecodedResponse>`

adapt_request_value は OpenRouter 分岐でのみ呼ぶ。実際に送る JSON と telemetry が同じ適用済み値を参照する。decode_response は protocol 検証を行い、ツールカタログと引数の検証は既存 validate_tool_message を共有する。

- [ ] `openrouter_request_uses_chat_and_required_parameters` を追加する。完全 model ID、Chat endpoint、tools 時の require_parameters、ネストした reasoning.effort を検証する。reasoning_effort、models、reasoning.mode、OpenCode/OAuth ヘッダーが送信されないことを assert する。
- [ ] `openrouter_effort_policy_does_not_guess_or_escalate` を追加する。Autoのsupported/unsupported/unknown、Fixedのknown unsupported/unknown、Off、mandatory、explicit nullとmissingを表で検証する。model不一致のmetadataをUnknownとして扱う。
- [ ] `openrouter_response_preserves_reasoning_and_validates_before_dispatch` を追加する。両 reasoning 名の同時出現では reasoning_content を選び、details のJSON全体がツール結果後の次リクエストと保存・復元後も一致することを確認する。無効ツールと有効ツールを同じ応答に入れ、実行カウンターが0であることを確認する。既存 HistoryManager の compaction テストにも、未消費の現在の assistant reasoning/tool/result ブロックが分断されず details が一致するケースを追加する。
- [ ] `openrouter_response_rejects_error_refusal_and_incomplete` を追加する。HTTP200error、wrong role、missing/length/content_filter/error finish_reason、refusal、空の最終文、Responses provider_state、不正JSONを拒否する。tool_calls finish とnull contentの正常例は通す。usageなし・一部欠損は値を捏造せず、通常の内容の処理を妨げない。
- [ ] `bash scripts/verify.sh test openrouter` を実行し、失敗を確認する。
- [ ] from_config、wire_model、inference_binding、HTTP redirect policy に専用分岐を追加する。キーなしでも endpoint/ID を検証してから None を返す。普通の text と tools の両経路に同じ OpenRouter decoder を適用し、既存バッチ検証後だけ返す。usage は完全なカウンターがある場合だけ既存 Usage へ渡し、prompt_tokens は実際に存在する値だけ返す。汎用 validate_completion の missing finish 許容を全 provider で変えず、OpenRouter 内だけ厳格化する。
- [ ] OpenRouter の chat_stream は検証済み非ストリーミング応答を1項目で返し、run_agent_streaming_once は通常 agent loop に委譲する。送信前・本文読込・retry sleep の既存キャンセルを保持する。
- [ ] 安全なエラー分類を追加する。401/402/403/404とHTTP200内errorは1回で終了、429/5xxは既存retry policy。raw body、reasoning、tool arguments、合成秘密sentinelがerror/logへ出ないテストを追加する。
- [ ] `bash scripts/verify.sh test openrouter`、`bash scripts/verify.sh test llm::`、`bash scripts/verify.sh test opencode::` を実行し、成功を確認する。main agent、subagent、tools-free finalization、text buffered の各入口をローカル fixture で通す。
- [ ] 関連ファイルだけ commit。推奨: `feat: adapt OpenRouter Chat through validated request paths`。

**完了条件:** 汎用 SSE は変更しない。ツールは完全な応答とバッチ全体の検証前に実行されない。サーバーの同一モデルのホスト選択を、クライアントの別モデル fallback と混同しない。

## Task 4: 保存モデルと能力スナップショットを起動・再開へ接続する

**Files:** 変更 `src/features/model_selection/mod.rs`、`src/features/openrouter/mod.rs`、`src/config/{app,inference_diagnostics}.rs`、`src/session/{selection,data}.rs`、`src/exec.rs`、`src/exec_resume_model_tests.rs`、`src/tui/commands/{new,model_selection,session_state,model_selection_tests}.rs`、`src/main.rs`。

**Interfaces:**

- `openrouter::selection_config(current: &AppConfig, model: &str, environment_key: Option<String>, metadata: Option<Arc<SelectedModelMetadata>>) -> anyhow::Result<AppConfig>`
- `openrouter::cached_metadata(model: &str) -> Option<Arc<SelectedModelMetadata>>` は disk read のみで、HTTP しない。
- `model_selection::records_selection(provider: ProviderKind) -> bool` は既存Go/ZenとOpenRouterに true。
- 既存 `session::selection::config_for_session(...)` と `validate_binding(...)` の signature は保持する。
- 既存 `TuiExecutor::prepare_session_selection` / `apply_session_selection` を拡張し、provider/modelの変更後は毎回専用クライアントを準備する。

- [ ] `openrouter_resume_restores_saved_model_before_client` を追加する。起動modelが空、別model、同modelのそれぞれで保存modelが優先されることを検証する。TUIとexecの両方で、異なるprovider、異なるbinding、keylessでも不一致を mutation 前に拒否する。
- [ ] `openrouter_explicit_resume_missing_does_not_create_session` を追加する。明示ID未発見とlatest空storeの双方で、OpenRouterではエラーかつ新規ファイル0を確認する。既存providerのlatest空storeの従来挙動は保持する。
- [ ] `openrouter_context_metadata_is_identity_scoped` を追加する。manual override > exact正値 > unknown/fallback、切替後の旧metadata破棄、cacheなし再開、実行中cloneの不変性を検証する。再開・起動時のネットワーク呼出数は0。
- [ ] `bash scripts/verify.sh test openrouter` と `bash scripts/verify.sh test resume_model` を実行し、失敗を確認する。
- [ ] AppConfig::from_cli で top-level resume と Commands::Exec の resume を判定し、明示再開時だけ未確定modelを許す。exec は既存のprepared session経路で復元する。TUIはOpenRouter再開時に初期clientを作らず、prepare_session_selection内で保存model確定後に生成する。元のproviderでは初期化順序を変更しない。
- [ ] 新規起動、/new、picker、exec、セッション切替・復元の各保存点で records_selection を使う。binding は `Openrouter:https://openrouter.ai/api/v1:ChatCompletions:<full-model-id>` に統一する。key/account情報やmetadataをSessionDataに追加しない。
- [ ] AppConfig::get_context_window_size にexact metadata参照を追加し、既存fallback estimateと表示の区別を保持する。cfg、client、FsToolsのconfigを同じ準備済み選択に揃える。subagentも親が渡すsnapshotを使用する。
- [ ] `bash scripts/verify.sh test session::`、`bash scripts/verify.sh test resume_model`、`bash scripts/verify.sh test model_selection`、`bash scripts/verify.sh test openrouter` を実行する。unknownなカタログIDの直接起動、legacy generic OpenRouterの再開、PR177 legacy Chat guidanceも確認する。
- [ ] 関連ファイルだけ commit。推奨: `feat: preserve OpenRouter selection across session lifecycle`。

**完了条件:** 保存済みモデルを読む前にデフォルトモデルや他のキーでクライアントを生成しない。保存情報を読み替える移行処理は追加しない。

## Task 5: CLI からカタログを利用できるようにする

**Files:** 変更 `src/main.rs`、`tests/model_catalog_cli.rs`、`src/features/openrouter/catalog.rs`。

**Interfaces:** Commands::Models に `refresh: bool` と `offline: bool` を追加する。早期分岐から CatalogClient::load と format_catalog を利用する。Go/Zen、OAuth の既存処理はそれぞれ従来の関数へ委譲する。

- [ ] `openrouter_model_catalog_cli_is_keyless_and_config_independent` を追加する。invalid TOML と合成OPENAI/OPENCODEキーを置いても、--offlineで既知fixtureを表示し、キーや設定本文を出さず、設定ファイルも変えないことを確認する。
- [ ] `openrouter_model_catalog_cli_flags_and_exact_lookup` を追加する。refresh+offline競合、非OpenRouterへの各flag、完全IDの大小文字、suffix、missing model、cacheなしofflineのexit statusを検証する。
- [ ] `bash scripts/verify.sh test model_catalog_cli` を実行し、失敗を確認する。
- [ ] config、logging、repomap、MCP初期化より前のmodels分岐にOpenRouterを追加する。Ctrl-CはカタログCancellationTokenへ伝える。`--model` は取得後の厳密検索であり、推論やモデル自動選択をしない。
- [ ] `bash scripts/verify.sh test model_catalog_cli` を再実行する。CLI integrationはtemp cacheのofflineを主に使い、HTTP動作はTask2の注入境界で検証する。本番CLIにテスト用endpointオプションは追加しない。
- [ ] 関連ファイルだけ commit。推奨: `feat: expose OpenRouter catalog through models command`。

## Task 6: Picker を所有型と明示状態へ拡張する

**Files:** 変更 `src/features/model_selection/mod.rs`、`src/tui/rendering.rs`、`src/tui/commands/model_selection_tests.rs`。

**Interfaces:**

- `ModelPickerEntry { provider: ProviderKind, id: String, name: Option<String>, api_label: String, unavailable_reason: Option<String>, metadata: Option<Arc<SelectedModelMetadata>> }`。
- `ModelPickerState::{ReadyFresh, LoadingWithoutSnapshot, RefreshingWithSnapshot, ReadyStale, FailedWithoutSnapshot}`。
- 既存ModelPickerへ owned entries、state、`generation: u64`、`catalog_job: Option<JobId>` とカタログ表示情報を追加する。
- `ModelPicker::results(&self) -> Vec<&ModelPickerEntry>`。
- `ModelPicker::cycle_provider(&mut self, backwards: bool)`。
- `ModelPicker::can_select(&self) -> bool`。
- `ModelPicker::apply_catalog(&mut self, generation: u64, result: Result<CatalogResult, CatalogError>) -> bool`。generation不一致はfalseで無変更。

- [ ] `openrouter_model_picker_search_and_capabilities` を追加する。ID/name substring、完全一致優先、ID順、現在model優先、unknown/非tools/非textのdisabled理由、未知price/contextを検証する。
- [ ] `openrouter_model_picker_loading_requires_new_enter` を追加する。loading/refreshingで選択不可、完了・失敗後に選択が自動実行されないこと、古いgenerationの結果を破棄することを検証する。
- [ ] `bash scripts/verify.sh test model_picker` を実行し、失敗を確認する。
- [ ] Go/Zen静的specからowned entryへ変換し、既存adapterの選択可能性を保持する。OpenRouterだけtext+tools確認を要求する。Tab/ShiftTabの純粋なprovider順とローカル検索を実装する。
- [ ] 描画を共通DTOに合わせる。provider、従量課金、キー存在、ID/name、能力、context、prompt/completion/request参考価格と条件付き料金、取得時刻/freshness/offline、NEW conversation/cancelを表示する。狭い画面でも入力欄と選択理由を壊さない。
- [ ] `bash scripts/verify.sh test model_picker` と `bash scripts/verify.sh test model_selection` を再実行する。terminal backend testsで狭い幅と制御文字を含む表示名も確認する。
- [ ] 関連ファイルだけ commit。推奨: `feat: support dynamic model entries in picker`。

## Task 7: カタログジョブと原子的な新規会話選択を TUI に接続する

**Files:** 新規 `src/tui/commands/model_catalog.rs`。変更 `src/jobs/types.rs`、`src/tui/commands.rs`、`src/tui/commands/{core,new,model_selection,model_selection_tests}.rs`、`src/tui/commands/handlers/dispatch.rs`、`src/tui/commands/handlers/slash_commands/models.rs`、`src/tui/event_handlers/model_picker.rs`、`src/tui/event_loop.rs`。

**Interfaces:**

- JobKindに `ModelCatalog` を追加し、表示は `model_catalog` とする。
- `CatalogDelivery { generation: u64, provider: ProviderKind, result: Result<CatalogResult, CatalogError> }`。
- `CatalogResultStore` は `Arc<Mutex<HashMap<JobId, CatalogDelivery>>>` を保持し、`stage(id: JobId, delivery: CatalogDelivery)`、`take(id: JobId) -> Option<CatalogDelivery>`、`discard(id: JobId)` を提供する。
- `TuiExecutor::start_model_catalog(&mut self, ui: &mut TuiApp, policy: CatalogPolicy) -> anyhow::Result<()>`。
- `TuiExecutor::finish_model_catalog(&mut self, ui: &mut TuiApp, id: JobId) -> anyhow::Result<bool>`。対象catalog jobならtrue。通常foreground完了処理へ流さない。
- `TuiExecutor::cancel_model_catalog(&mut self, ui: &mut TuiApp)`。
- CommandHandlerには同じ起動・キャンセル操作を呼ぶ最小の委譲口を追加する。実装済みprovider/model選択の既存signatureを保持し、選択時にpicker entryのmetadataを準備済みcfgへ渡す。

- [ ] `openrouter_catalog_job_consumes_once_after_terminal` を追加する。JobSpecはBackground、WorkspaceAccess::None。payloadがterminal前にselect可能にならず、consume-onceで取り込まれることを確認する。
- [ ] `openrouter_picker_cancel_discards_late_completion` を追加する。Esc、Ctrl-G、event_loopのCtrl-C別経路、provider変更、閉じて再表示、キャンセル後完了を検証する。古い結果・warningが新しい画面へ入らず、storeに不要payloadを残さないことを確認する。
- [ ] `openrouter_model_selection_is_atomic_and_key_isolated` を追加する。同OR内ではkey+origin保持、他providerからはOR環境キーだけ、keyless可、同じdraft維持、history隔離、save failure/lock failure時の旧cfg/client/history/checkpoint保持、repeat Enterの二重作成なしを確認する。既存busy/queued/diff review拒否とno pruningも検証する。
- [ ] `bash scripts/verify.sh test model_selection`、`bash scripts/verify.sh test model_catalog`、`bash scripts/verify.sh test model_picker` を実行し、失敗を確認する。
- [ ] 既存JobManager::spawnに `JobSpec::new(JobKind::ModelCatalog, JobScope::Background, WorkspaceAccess::None, ...)` を渡す。ジョブ本体で結果をstoreへstageし、管理者がterminal化・active解除した後のcompletion hookでIDだけ通知する。core::set_ui_txのforeground-only条件をModelCatalogだけに限定拡張する。scoped_senderやagent ownership判定は流用しない。
- [ ] dispatch::handle_job_completedの先頭でcatalog完了を処理してearly returnする。通常foregroundのstatus、processing_start_time、deferred follow-upを変更しない。cancel/abort時もtake/discardし、生成失敗・spawn失敗ではpickerを取得中のまま残さない。
- [ ] slash commandを仕様どおりに拡張し、fresh cacheは即表示、staleは表示を残してjob取得する。Ctrl-Rとprovider移動では現在jobをcancelし、generationを進める。取得中Enterは無効。close/cancelを共通化し、Ctrl-Cの独立経路も必ず通す。
- [ ] start_model_sessionで所有entryを検証し、Task4のselection_configを用いる。fallible lock・保存を先に完了し、cfg/client/tools/UIを公開してから新履歴を開始する。既存のdurability warningとdraft保護を保持する。
- [ ] `bash scripts/verify.sh test model_selection`、`bash scripts/verify.sh test model_picker`、`bash scripts/verify.sh test model_catalog`、`bash scripts/verify.sh test jobs::`、`bash scripts/verify.sh test tui::followup::` を実行し、成功を確認する。
- [ ] 関連ファイルだけ commit。推奨: `feat: integrate cancellable OpenRouter picker and session switching`。

**完了条件:** カタログ更新が処理中の会話を変更せず、完了通知が別のforeground処理を終了したように見せない。ネットワークは検索キーごとに呼ばれない。

## Task 8: ドキュメントと利用者向け表示を揃える

**Files:** 変更 `README.md`、`src/main.rs` のhelp、`src/tui/commands/handlers/slash_commands/help.rs`、必要な `src/config/inference_diagnostics.rs` の説明。既存 `tests/model_catalog_cli.rs` を拡張。

**Interfaces:** 仕様に記載したCLI/TUI操作と実際の表示を一致させる。新しい永続設定や機能を追加しない。

- [ ] CLI helpにOpenRouter、refresh/offline競合と対象provider、モデルの明示指定が現れるテストを追加し、失敗を確認する。
- [ ] READMEへ専用環境変数、設定順位、固定URL、generic利用時との区別、モデル一覧/詳細/refresh/offline、CLI起動/exec/resume、/providerと/modelsを記載する。
- [ ] 新規会話方式、draft保持、keyless閲覧、従量課金、価格参考値、unknown、キャッシュ寿命、モデルfallbackなし、初版は完成後表示、キャンセルで課金停止を保証しないことを説明する。
- [ ] 旧genericセッションの再開とロールバック時の注意を記載する。サンプルには架空IDまたは明示的なplaceholderだけを使い、既定の有料モデルを推奨設定として埋め込まない。実キーは書かない。
- [ ] `bash scripts/verify.sh test model_catalog_cli` と `bash scripts/verify.sh guidance` を実行する。
- [ ] 関連ファイルだけ commit。推奨: `docs: explain OpenRouter setup and safe model switching`。

## Task 9: 回帰・画面確認・変更全体のレビューを行う

**Files:** 全変更ファイルと既存回帰テスト。新しい機能コードを追加するタスクではない。

**Interfaces:** 入力はTasks1〜8のbranch。出力は検証結果、既知の制約、レビュー済み差分、作成済み draft PR。

- [ ] 次の回帰マトリクスを、mock、static inspection、実行環境/CIごとの結果で埋める。未実行を成功扱いしない。

| 経路・条件 | 必須の結果 |
| --- | --- |
| provider未指定＋公式OpenAI URL | Responses維持 |
| 明示openai-compatible＋公式URL | Chat維持 |
| 汎用カスタムURL・従来OpenRouter URL | 既存Chat・既存意味論維持 |
| OpenAI OAuth | 別認証・Responses維持 |
| Go/Zen | 既存静的catalog・認証・model routing維持 |
| 専用OpenRouter text/tools/subagent | Chat・完全ID・検証付き非stream |
| キーなしbrowse/select、キーなしinference | 前者可、後者明確なエラー |
| HTTP200error・invalid tools・refusal・incomplete | 正常完了/兄弟tool実行/モデルfallbackなし |
| catalog失敗・古いgeneration・cancel | 会話と新しいpickerを保護 |
| saved model resume・provider mismatch | 正しい復元、異経路は変更前拒否 |
| usage/context/price欠損 | 不明と0を区別 |
| ログ・diagnostics・cache・session | 秘密sentinelがない |

- [ ] 作業ディレクトリをfeature worktreeのrootとして `cargo fmt --all`、`bash scripts/verify.sh rust`、`bash scripts/verify.sh guidance`、`bash scripts/verify.sh tui-deps` を実行する。rust wrapperはfmt check、locked Clippy `-D warnings`、full locked testsを含む。全て終了0を要求する。
- [ ] CIのMSRV gateに合わせ `bash scripts/verify.sh msrv` を実行する。1.94.0未導入なら環境ブロッカーとして報告し、CIの `rustup run 1.94.0 cargo check --locked --all-targets --all-features` で確認する。依存を変えていないこともCargo diffで確認する。
- [ ] macOSでは `bash scripts/verify.sh macos`、LinuxではmacOSを代用したと主張せず対応CI結果を待つ。既存CIはRust1.99.0とMSRV1.94.0を利用しているが、実行時のworkflowを優先する。
- [ ] `cargo run --release -- <flags>` で、隔離したtemp project/config/cacheとキーなし状態を使いTUIを手動確認する。Task6/7のローカルfixtureとfresh/stale cacheを使い、検索、完全ID貼付、Enter/repeat、Esc、Ctrl-C/G、再表示、更新中close、Tab/ShiftTab、stale fallback、狭い幅、draft、busy拒否を画面またはterminal recordingで残す。リアル推論は送らない。
- [ ] 変更全体をレビューする。認証分離、request/model snapshot、session transaction、opaque reasoning、キャンセル競合、ログ安全性を優先する。指摘を修正したら影響するfocused testsと最終ゲートを再実行する。
- [ ] 最後に develop と関連PR/CIを再確認し、差分の競合を確認する。PRのbaseはdevelop、headはfeature branch。最新差分と検証結果を記載して draft PR を作成し、URLとCI状態を報告する。PR作成は明示的に依頼済みで、追加の一般的な承認確認は不要。developへの直接push/mergeはしない。
- [ ] 最終報告には変更、判断、各検証コマンドと結果、macOS等の未実行項目、live未検証、残るCI状態、次の一手を簡潔に残す。作業再開に不要な新しいログファイルは増やさない。

## 自己レビュー

### 仕様の網羅性

| 仕様節 | 対応タスク |
| --- | --- |
| 1 目的・承認方針 | Global Constraints、Tasks1〜9 |
| 2 前提・非対象 | Task0、Task9 |
| 3 CLI/TUI | Tasks5〜8 |
| 4 provider・認証 | Tasks1、3、4、7 |
| 5 ID・catalog・cache | Tasks1、2、5 |
| 6 picker状態・表示 | Tasks6、7 |
| 7 Chat・tool・非stream | Task3 |
| 8 reasoning・履歴 | Tasks3、4、9 |
| 9 session遷移・resume | Tasks4、7 |
| 10 metadata・context・usage | Tasks2〜4、6 |
| 11 error・retry・log | Tasks2、3、9 |
| 12 実装境界 | ファイル表、Tasks1〜8 |
| 13 受け入れ・検証 | 各taskのred/green、Task9 |
| 14 rollout・rollback | Tasks0、8、9 |
| 15 自己レビュー事項 | Review Focus、全taskと本節 |

### 整合性の確認

- 各タスクに対象ファイル、受け渡す型・関数、失敗テスト、検証コマンド、コミット境界がある。
- metadataの型は一つに統一し、cfg/client/pickerに同じprovider/model識別を保持する。カタログ全体をSessionDataに保存しない。
- catalog payloadはstore、通知はJobIdとし、既存文字列transportを全面改修しない。terminal後に取り込むためactive_countによる切替拒否とも整合する。
- missing-modelとresumeの初期化順序、Ctrl-Cの別経路、同時reasoningフィールド、partial usage、圧縮応答・未来時刻・cache保存失敗に、それぞれ担当タスクとテストを割り当てた。
- 既存汎用Chatのmissing finish許容やGo/Zenのキー共有を、新providerのために一律変更しない。
- テスト用endpointは非公開注入に限定し、本番の固定URL・キー分離を緩めない。
- モックで検証できる範囲とライブ未検証を区別した。実モデルの可用性や品質、有効キー、残高は確認しない。
- 承認済み仕様からの機能追加・仕様変更はない。重大な設計矛盾は見つかっていない。

実装方式と担当モデルは既に指定済みである。本計画のレビュー後、Codex（gpt-6.1-sol）が上記タスクを順に実装する。

## 参照

- [PR #177](https://github.com/mopemope/doge-code/pull/177)
- [develop CI](https://github.com/mopemope/doge-code/actions/runs/39808010067)
- [OpenRouter Models](https://openrouter.ai/docs/guides/overview/models)
- [OpenRouter Reasoning tokens](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens)
- [OpenRouter Provider routing](https://openrouter.ai/docs/guides/routing/provider-selection)
- [OpenRouter Errors](https://openrouter.ai/docs/api_reference/errors-and-debugging)
