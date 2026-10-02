# OpenAI Subscription 作業ノート

2026-10-02 開始、2026-10-03 検証。基準 `develop / 9b4ac79`。
開始時は計画ファイルだけが未追跡。実アカウントの OAuth login や token 取得は実施していない。

## 採用した境界

- SIWC public OAuth + 公式 `/v1/responses` HTTP/SSE。project config は issuer、credential path、account を変更できない。
- `OpenAIClient::from_config` を TUI、exec、watch、doc の共通 factory にした。通常推論、symbol edit、要約、subagent は同じ client の adapter を使う。
- provider は CLI → `DGC_PROVIDER` → project → user → `openai-compatible`。CLI 選択時は無効な環境設定にも優先する。
- scope 不足は identity-only の状態として保存。再同意は `auth login --enable-plan-usage` の明示操作だけで `prompt=consent` を追加する。
- 認証 code の `invalid_grant` は issued client ID を保持し、新しい state/nonce/PKCE/listener/code で一度だけ認証をやり直す。2度目の失敗は停止する。
- refresh はプロセス内 single-flight と registry-wide OS file lock。計画のアカウント単位より粗いロックだが、active 選択、logout、資格情報の原子的更新を同じ境界で扱い、別プロセスの最新 token を再読込する。
- rotating refresh token を送る前に更新未確定の状態を永続化する。応答喪失・キャンセル・書込失敗の後は古い token を再送せず再認証を案内する。terminal refresh error では当該 account の token だけを削除する。
- JWT は `jsonwebtoken 9.3.1`（PEM機能不要）、PKCE は `sha2 0.10`。暗号検証は既存ライブラリを使う。追加で解決された crate は jsonwebtoken のみ。
- loopback callback の Host/state/path/重複 query/サイズ/期限を検証。ID token は署名/JWKS/issuer/aud/exp/nonce/sub と選択済み identity を検証する。
- logout は lock 内で revoke を最大3回試し、token を削除し登録 ID/host ID を保持する。遠隔失効が未確認なら明示する。Ctrl-C でも cleanup を完了させる。
- Responses request は typed allowlist。namespace `dgc` 内の function でローカル tool_search/MCP alias を扱い、`strict:false` を明示する。禁則フィールドを既存 Chat Completions body から持ち込まない。
- SSE は byte framing と typed terminal events。表示は完了後にまとめて行う初版とした。完了前の delta/output_item は副作用を認可しない。旧 streaming tool executor に戻らない。
- assistant に versioned `provider_state` を保存し ordered raw output を正本として再送する。表示 projection は二重送信しない。session binding は account/provider/model の切替を拒否し、圧縮後も残す。
- governor は Responses input/tools を測定する。暗号文を token 化せず、opaque item ごとの仮置き余裕と実 usage による校正を使う。正確な tokenizer/契約残量として扱わない。不明な model capacity は `[llm] context_window_size` を案内する。
- completed tool batch は次の通信前に session checkpoint を保存する。失敗・キャンセル時にも checkpoint を保存し、TUI はそれを復元する。途中で結果のない call は「実行結果不明」の対応 output を保存し、自動 replay を避ける。
- TUI の status line に provider/account/model を保持。README に setup、scope、限度時、logout、保存・再開・preview制約を記載した。

## 公式仕様の確認

2026-10-02/03 に以下を再確認した。

- [Registration and sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in): dynamic registration、発行 ID の再利用、loopback URI、ID token 検証。
- [Accounts and sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions): rotating refresh、discovery/revocation。
- [Models and inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference): account ごとの `models`、`visibility:list`、SSE 完了条件。
- [Errors and recovery](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery): scope 不足時の明示再同意、terminal refresh error、利用制限と bounded retry。
- [Preview limitations](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations): public endpoint の request 制約。
- [Reasoning](https://developers.openai.com/api/docs/guides/reasoning): 現仕様では `store:false` で encrypted reasoning が既定返却されるため、legacy `include` 指定は不要。順序を保持して replay する。

秘密を含まない request/response fixtures は `src/features/openai_subscription/tests.rs` にある。
`fixtures/test-only-rsa.der` はローカル署名検証用に新規生成した公開テストデータで、実アカウントには一度も使用していない。

## 検証記録

- `bash scripts/verify.sh test features::openai_subscription::`: **49 tests PASS**。OAuth 初回/再認証/拒否/取り違え、code 失効の一度だけ再試行、署名/claims 不正、owner-only 保存、symlink、lock cancel、独立 handle の refresh 排他、refresh/logout 競合、model order、SSE、ordinary/subagent/tool、複数call、再開、圧縮、完了前の変更禁止、失敗後の未消費結果保存、中断 batch の結果不明扱い、request Debug の opaque 非露出を含む。
- `bash scripts/verify.sh msrv`: **PASS**。不足していた Rust 1.88.0 toolchain を導入して `cargo check --locked --all-targets --all-features` を確認。最終履歴変更後にも再実行して成功（45.9秒、`dgc-verify-0mxakvml`）。
- `bash scripts/verify.sh guidance`: **PASS**（28 Python tests）。
- `bash scripts/verify.sh tui-deps`: **PASS**。
- `bash scripts/verify.sh rust`: fmt **PASS**、Clippy `-D warnings` **PASS**。全 unit tests は **1311 PASS / 3 FAIL / 3 ignored**。全体 gate は失敗であり成功とは扱わない（下記 baseline 再現あり）。最終ログ: `/private/var/folders/53/55gyxrb52t9cm0nywd06cnsh0000gn/T/dgc-verify-djj5pvhh/`。
- 計画の focused filters: `config::` 53、`llm::client_core::` 12、`llm::tool_execution::requests::` 12、`llm::tool_execution::history::` 26、`llm::context_budget::` 18、`session::` 46、`tui::` 146 tests **PASS**。履歴/TUI の最終変更後も full run 内で当該 tests は成功した。
- `bash scripts/verify.sh macos`: 最終変更後に TUI 146 / execution 161 / jobs 34 tests **PASS**。ログ: `dgc-verify-z0n4owap`。
- `cargo test --locked --test evidence_cli`: **2 tests PASS**。全 unit gate の既存失敗で自動実行されなかった integration tests を独立実行した。
- `git diff --check`: **PASS**。
- release build **PASS**。既存 toolchain の `rust-objcopy` が `libLLVM.dylib` を見つけられず strip の warning が出たが、build/実行は成功した。
- 手動 release TUI: 一時 project、fake API key、loopback port 9、RepoMap/MCP server 無効で起動→`/help`→`/quit`。推論・外部通信は実施していない。録画: `/private/tmp/dgc-subscription-tui/release-startup-final.typescript`（最終変更で再buildし `cargo run --release --locked` を再確認）。
- subscription status line は本番 `view` を TestBackend で描画し、会話が続いても provider/account/model が表示されることを確認した。録画: `/private/tmp/dgc-subscription-tui/selection-render.typescript`。これは fake account の描画確認で、実ログイン済み TUI の確認とは区別する。

## 全体 gate の既存失敗

Git `HEAD / 9b4ac79` を `/private/tmp/dgc-subscription-baseline` に `git archive` で展開し、同じ macOS/権限で以下を個別再実行した。いずれも変更前から同じ箇所・同じ OS error で失敗した。

1. `features::evidence_report::tests::frozen_saved_evidence_is_not_reopened_from_mutable_storage`: `Session(ReadError(NotADirectory / code 20))`。
2. `features::evidence_report::tests::git_helpers_are_disabled_and_non_utf8_is_not_aliased`: non-UTF-8 filename 作成の `Illegal byte sequence / code 92`。
3. `features::verification_snapshot::tests::submodules_and_non_utf8_paths_cannot_establish_complete_match`: 同じ non-UTF-8 filename 作成エラー。

本機能のために unrelated evidence/report 実装や macOS filesystem テスト条件を書き換えていない。全体 gate を通すには、この既存 failure の解消が別途必要。


## 実接続と手動確認の残り

対応実装済み・実接続未検証。本人のブラウザ同意を伴う login、実アカウントの適格性、実 model catalog、namespace/reasoning の実推論、実 refresh/logout は未確認。
専用一時プロジェクトで計画 P7 の順序で確認する。利用枠を故意に使い切らない。
Linux CI はこの macOS 作業では実施していない。Windows の credential store、device flow、WebSocket、hosted tools は初版対象外。

## PR 前レビュー（2026-10-03）

- 認証用 HTTP client の30秒 timeout が推論にも適用される不具合を修正した。Responses request で設定済み request deadline を適用し、認証・retry を含む総時間制限は維持する。短い OAuth timeout と遅延サーバーによる回帰テストを追加した。
- TUI の account 表示を別途 credential selection から取得せず、実際に構築した executor client から取得する。起動中の active account 切替で表示と通信先がずれる競合を除いた。
- completed response に unfinished output item が含まれる場合と、assistant 以外の output message を拒否する。後者は保存済み raw state の replay 時にも検証し、role の権限昇格を防ぐ。回帰テストを追加した。
- 初版範囲の残りの実装は認められなかった。実接続の確認は本人のブラウザ同意が必要なため未実施として引き継ぐ。

レビュー修正後の再検証:

- `bash scripts/verify.sh test features::openai_subscription::`: **51 tests PASS**（`dgc-verify-6n7pf63u`）。通常 sandbox は loopback bind を拒否したため、ローカル模擬サーバーを許可して再実行した。
- `bash scripts/verify.sh rust`: fmt/Clippy **PASS**、unit tests **1313 PASS / 3 FAIL / 3 ignored**（`dgc-verify-wwqgwvrm`）。失敗は上記の変更前にも再現した3件と一致。
- `bash scripts/verify.sh msrv`: **PASS**（`dgc-verify-cvz2t72l`、43.8秒）。
- `bash scripts/verify.sh guidance`: **PASS**（`dgc-verify-gv_sa8lz`、28 Python tests）。
- `bash scripts/verify.sh macos`: **PASS**（TUI 146 / execution 161 / jobs 34、`dgc-verify-7lncazog`）。
- レビュー修正後の release build **PASS**。同じ `rust-objcopy` の環境 warning は継続。
- 修正後 release TUI の起動→`/help`→`/quit` を再確認。録画: `/private/tmp/dgc-subscription-tui/review-startup.typescript`（fake API key、推論なし）。
- `cargo test --locked --test evidence_cli`: **2 tests PASS**。
