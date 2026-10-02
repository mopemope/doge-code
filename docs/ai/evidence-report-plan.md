# 検証記録エクスポートの実装計画

この計画は、dgc が保存した要求・変更・検証結果を、PR レビューに添付できる Markdown と JSON に出力する機能の実装指示である。初期版は実装済み。実装後の検証記録は `evidence-report-work-note.md`、実行時 snapshot の追加は `verification-snapshot-work-note.md` を参照する。

対象利用者は、エージェントが作った変更をレビューする開発者。変更の目的、観測した検証結果、未確認事項、現在のコードとの不一致を一つの記録で確認できることを目的とする。

調査日: 2026-10-02。調査時 HEAD: `dadb813`。実装開始時に HEAD と作業差分を再確認する。既存のユーザー変更を上書きしない。

## 完成時の利用方法

```bash
dgc session evidence <SESSION_ID>
dgc session evidence <SESSION_ID> --format json
dgc session evidence <SESSION_ID> --base main
dgc session evidence <SESSION_ID> --include-content
dgc session evidence <SESSION_ID> --format markdown > /tmp/dgc-evidence.md
```

- セッション ID は既存 CLI と同じ一意な prefix を許可する。ID は必須とし、暗黙の latest 選択は追加しない。
- `--format` は `markdown` と `json`。既定値は `markdown`。
- stdout は完成した成果物だけ。診断ログは stderr。JSON は単独の JSON document とし、進捗文を混ぜない。
- v1 は stdout 出力のみ。`--output`、上書きオプション、PR 自動投稿は追加しない。README の保存例はリポジトリ外へのリダイレクトとする。リポジトリ内へリダイレクトすると、シェルが先に作った出力ファイルが比較対象になる場合がある。
- API キー不要。LLM、RepoMap 生成、MCP listener/client を起動しない。Git の読み取り以外の外部コマンドや通信を行わない。
- 証跡やソース、Git index、セッションを更新しない。レポート作成自体を mutation や verification として記録しない。通常のアプリ診断ログはこの禁止の対象外だが、証跡本文はログに出さない。
- 正常にレポートを出せた場合は exit 0。過去の検証失敗や証跡不足はレポートの内容であり、CLI の実行失敗とは区別する。
- CLI 入力不正は clap の既存終了規約。読めないセッション、明示した base の解決失敗、出力失敗、一貫した読み取りができない場合などは非ゼロ終了。品質ゲート用の独自終了コードは v1 で追加しない。

## 初期版の対象範囲

実装するもの:

1. 保存済みセッション、計画、provenance v1〜v4 の読み取り。
2. 指示・要求・計画・変更・検証の関連を保持した、独立したレポートモデル。
3. 現在の workspace に対する変更状態と verification obligation の状態表示。
4. Git HEAD と、指定時の base に対する現在の差分ファイル一覧。未帰属の変更を明示。
5. 出力時の対象ファイル状態の識別情報と、読み取りの整合性確認。
6. Markdown / JSON の決定的な整形、欠落情報の明示、README、回帰テスト。

後続機能として保留するもの:

- テスト再実行、修正前後比較、テスト件数や coverage の採取、環境再現。
- 新しい provenance wire version、過去イベントの移行・再解釈。
- MCP 公開、LLM tool 登録、TUI パネル変更、GitHub API / Check Runs / PR 投稿。
- 複数セッション統合、署名・改ざん耐性の保証、レポート import/check コマンド。
- 自動的な要求達成判定、マージ許可判定、独自の総合スコア。

## 既存コードの再利用箇所

| 場所 | 再利用する責務と注意点 |
|---|---|
| `src/main.rs` | `Commands::Session` は RepoMap / LLM 初期化前に分岐済み。ここを維持する |
| `src/session/cli.rs` | `SessionCommands`、ID prefix 解決、session CLI の入り口 |
| `src/session/store.rs` | `load`、`resolve_id_prefix`、`session_dir`。`new` は mkdir するため読み取り専用経路を追加する |
| `src/session/data.rs` | `provenance_incomplete`、`provenance_record_failures`、`changed_files` |
| `src/tools/plan.rs` | `plan_read_from_base_path`。現在と legacy の保存先に対応し、計画なしは空として読める |
| `src/provenance/store.rs` | `load_all` で v1〜v4 を正規化。読み込み警告を捨てない |
| `src/provenance/query.rs` | `resolve_active_states` と `compute_coverage` |
| `src/provenance/requirements.rs` | `current_requirements`、`plan_requirement_links`、`compute_requirement_coverage` |
| `src/provenance/obligations.rs` | `compute_obligation_coverage` と既存 evidence state |
| `src/provenance/types.rs` | 記録済みのコマンド・結果・帰属・hash の意味 |
| `src/diff_review.rs` | evidence の表示方針の参考。TUI 用件数上限をレポートへ流用しない |
| `src/execution/runner.rs` | 有限 Git コマンドの managed process runner |

既存の `provenance_read` tool はモデル向けの pagination / budget を持つ。レポートは tool の文字列出力を解析せず、保存ストアと型付き query を直接利用する。`FsTools` や `Executor` をレポートのためだけに生成しない。

既存 query の `verified_active_change_ids` は内部名として存在するが、出力では `observed_passing_change_ids` などに明示変換する。正しさの証明を意味する `verified` / `proven` / `satisfied` を新しい状態として追加しない。

## レポートの仕様

レポート schema は provenance wire schema と独立した `schema_version: 1` とする。`serde_json::Value` の寄せ集めではなく、型付き DTO と列挙型を定義する。

| フィールド群 | 必須内容 |
|---|---|
| `schema_version`, `generator`, `generated_at` | レポート版、dgc 版、UTC の生成時刻 |
| `session` | ID、保存済み更新日時。会話本文・token履歴は出さない |
| `scope` | 単一セッション、現在の workspace との比較であること、project と Git root の相対関係 |
| `repository` | Git 状態、現在の HEAD OID、任意の指定 base と解決済み OID、比較方式 |
| `snapshot` | 対象範囲、各パスの現在状態、manifest digest、観測時の整合性状態 |
| `directives` | ID、origin、raw/effective instruction の hash。本文は既定で除外 |
| `requirements` | ID、statement、Active/Withdrawn、source directive IDs、関連計画・変更・検証・既存 coverage state |
| `plan` | 現在の plan items と verification obligations。完了 status と検証結果を別々に表示 |
| `changes` | 全 change IDs、file/symbol、凍結済みの帰属、before/after hash、現在の lifecycle state |
| `verifications` | event ID、時刻、source、kind、program/argv、cwd、outcome、観測した change IDs、obligation ID/binding |
| `workspace_comparison` | 差分ファイルとセッション変更の関連。未帰属項目は独立して保持 |
| `summary`, `warnings`, `limitations` | 状態別件数、情報欠落、要再確認事項、保証範囲 |

DTO の具体的な Rust 型名や内部関数分割は実装者の裁量とするが、この情報と意味を維持する。単一の overall pass/fail は付けない。

### 状態と証拠の意味

- `RequirementStatus` と要求の evidence state は別の属性。要求文をユーザーの逐語引用として表示しない。
- change の帰属は `ChangeCommitted` 当時の値。現在の計画で上書きしない。
- verification の帰属も実行開始時に固定された値をそのまま出す。
- obligation の `pending / observed_passing / observed_failing / stale / diverged / reverted / mixed / no_linked_change` は既存計算に合わせる。
- requirement coverage の `observed_passing` は「関連する成功観測がある」であり、全 obligation 成功ではない。要求行には obligation の成功・未実施等の内訳も併記する。
- 最新の失敗、古い成功、取り消し済み・superseded の変更を隠さない。成功履歴があるだけで現在の obligation を成功に変更しない。
- `session.changed_files` は workspace 全体の変更一覧ではない。外部変更の検出には Git 比較を併用する。
- 既存記録には実行時の OS / toolchain / lockfile 全体、実行テスト件数、依存ファイル全体の snapshot がない。対応属性は `null` / `not_recorded` とし、現在の環境を過去の環境として補完しない。
- report の manifest は出力時の限定範囲の識別情報。実行時 snapshot、リポジトリ全体の同一性、再現性、署名を意味しない。
- 証跡なし・古い形式・壊れた一部イベントは「記録不足」。空集合を成功扱いしない。

### 本文と表示

- 通常出力は要求文、計画文、obligation の説明、program/argv、結果を含む。共有に必要な情報として扱うが、ユーザーが argv や要求文に秘密を入れた場合の完全な自動除去は保証しない。README に出力内容を具体的に記載する。
- `--include-content` 指定時だけ raw/effective directive、変更 diff、保存済み stdout/stderr excerpt を追加する。保存時に切り詰められた結果を全文と称さず、`output_truncated` を維持する。
- 会話全体、環境変数、API キー設定、Git remote URL、Observation Store 全体は両モードで除外する。
- cwd は project 相対を優先し、project 外は値を公開せず `outside_project` とする。構造化された file パスも project 相対。argv の任意文字列をパスとして勝手に書き換えない。
- Markdown は要約、要再確認事項、要求一覧、変更、検証履歴、記録の制約の順。長い履歴や任意本文は details 等で畳める。JSON と同じモデルから生成する。
- コード・ログ・要求文に含まれる `|`、改行、backtick、HTML、制御文字で表や fenced block が壊れないよう escape する。任意テキストをリンクや raw HTML として解釈させない。
- 出力を黙って切り詰めない。件数・メモリ上限は定数として設定し、超過時は成果物を書き出す前に説明付きエラーにする。LLM 用の数千文字上限は流用しない。

## Git 比較と対象範囲

`--base <REF>` の意味は「指定 REF が指す commit と現在の working tree の比較」。merge-base を暗黙に選ばない。PR の三点比較と同じとは表示しない。README に明記し、必要なら利用者が選んだ merge-base OID を指定できる。

- REF を安全な argv として `rev-parse --verify --end-of-options <REF>^{commit}` 相当で解決し、その後の処理は解決済み OID に固定する。fetch はしない。
- base 指定時は base commit と現在ファイルの差分を採取する。コミット済み・stage 済み・未stage の最終結果を含める。rename は v1 では delete/add として扱ってよいが、その方式を表示する。
- base 未指定時は HEAD と現在ファイルの比較。HEAD と index、index と working tree の途中状態は dirty 情報として別途保持し、相殺されている変更も隠さない。
- Git untracked の通常ファイルも列挙する。ignored ファイルと `.git/`、`.doge/` ランタイム状態は workspace 比較から除外し、その除外範囲を明記する。provenance が明示的に参照するファイルは別途対象とする。
- project root が Git root のサブディレクトリの場合、v1 の比較範囲は project root 内に限定する。親側や sibling の変更をカバーしたと表示しない。
- Git がない、Git repository でない、HEAD がない場合、base 未指定なら警告付きのセッション記録を出力できる。明示的な base 比較要求を満たせない場合はエラー。
- Git command は managed runner、明示 cwd、structured argv、有限 timeout を使う。`bash -c`、新規 `Command::output()` は使わない。
- status は porcelain と NUL 区切り、diff のファイル列挙も NUL 区切り。外部 diff/textconv を無効にする。optional index refresh を避ける Git 設定を使用する。
- Git capture の truncation、timeout、読み取りエラーを完全な一覧として使用しない。base 指定時の比較失敗はエラー、base 未指定時は比較 unavailable の警告とし、session 部分を保持してよい。
- non-UTF-8 path、binary、symlink、submodule、unmerged index は処理方針を型で区別する。対応できないものは `unsupported/unavailable` として completeness を下げる。symlink を辿って project 外の内容を hash しない。Git path の lossily な変換を一意な識別子として信頼しない。

比較行では `session_linked` と `unattributed` を使い分ける。ファイルに session event があるだけでファイル内の全 hunk を dgc 作成としない。file hash が一致しない場合はその不一致を表示し、作者や原因を推測しない。既存 semantic query がシンボルだけの一致で active を返す場合も、ファイル全体の一致とは別の属性にする。

## 読み取りと一貫性

1. セッション ID を既存 store の規則で解決し、`session_dir` から参照先を取得する。
2. mkdir/save/cleanup を伴わない `SessionStore::open_existing` 相当を追加する。存在しない `.doge/sessions` を作らない。既存書き込み経路は変えない。
3. セッション、plan、全 provenance をロードする。plan の legacy fallback を再利用する。plan 不在は空、壊れた plan は warning とし obligation の算出を unavailable として区別する。
4. event session ID 不一致や project 外への path は警告付きで除外し、query が外部ファイルを読む前に検査する。任意 ID を未検証で path join しない。
5. query 前に session/plan/event 集合の内容識別情報、Git OID・status・対象 path 集合、対象ファイル状態を採取する。query と DTO 構築後に再採取する。
6. 前後が変わっていれば全体を一度だけ再試行する。再度変化したらエラーで、stdout に途中の成功レポートを出さない。
7. file の欠落、読めない状態、非対応形式は区別し、現在状態が不明なものから肯定的な current-match を作らない。
8. 全 DTO を組み立ててから render / serialize し、最後に stdout へ出す。

これは optimistic な前後照合であり、ファイルシステムの atomic snapshot ではない。前後一致を transactional consistency と宣伝しない。既存 query が個別に再読する構造を大規模改修せず、前後照合で検出できる競合を扱う。

manifest の対象は、検査済み provenance が参照するファイル、session.changed_files、Git 比較と dirty/untracked 一覧に現れる project 内パスの和集合とする。すでに commit 済みで Git 差分が空の session ファイルも落とさない。

manifest は対象 path、kind、存在状態、byte length、取得できた exact-byte BLAKE3 hash を path 順に並べ、versioned な固定構造を serialize して hash する。欠落/読めない/binary等の状態も含める。時刻やレポートの整形は manifest digest に含めない。取得できないエントリーがある場合は `complete: false`。この digest を全 repository の tree hash と呼ばない。

JSON 配列の順序を固定する。events は store の `(timestamp, event_id)`、集合は ID/path 順、plan は保存済み順。HashMap の列挙順に依存しない。時計をテストで固定できるようにする。

## 実装ファイルと手順

推奨配置:

```text
src/features/evidence_report/
  mod.rs          # 型付き export API、収集の調停、エラー
  model.rs        # schema v1 DTO
  collect.rs      # 保存済みデータの読み取りと query の利用
  workspace.rs    # Git 比較、限定 manifest、前後照合
  render.rs       # Markdown と JSON
  tests.rs        # 複合ケース
```

小さく収まる箇所は統合してよい。機能本体を `src/tools/` や `session/cli.rs` に集約しない。

| 手順 | 作業 | 完了条件 |
|---|---|---|
| 1 | DTO、意味、警告型、report builder の入力境界 | 成功・失敗・pending が混在する fixture を JSON として表現できる |
| 2 | `open_existing`、セッション・plan・event collector | v1〜v4 と欠落/破損を扱い、読取りで保存物を変えない |
| 3 | 既存 coverage/query との接続 | 帰属の固定、stale、reverted、requirement と obligation の違いを維持 |
| 4 | Git 比較、manifest、読み取り前後照合 | session 外の差分を表示し、競合/不明を成功扱いしない |
| 5 | Markdown/JSON renderer と本文の opt-in | 両形式が同じ事実を表し、任意文字列で表示が壊れない |
| 6 | `SessionCommands::Evidence` と main の接続 | API キー無しの subprocess smoke test で stdout が成果物だけになる |
| 7 | README、必要なら architecture の module map 更新 | 利用例、比較方式、出力内容、制約、終了状態が説明されている |
| 8 | focused tests、fmt、全 Rust gate、guidance | 必須チェックの結果を残し、失敗や環境ブロックを区別して報告 |

現在 `session::cli::run` は同期。Git の managed runner を使うため、最小変更として async 化し main 側で await する案を推奨する。参照元を検索し、list/show/delete の互換性を確認する。既存 Tokio runtime 内で `block_on` や別 runtime を作らない。

`SessionStore::open_existing` や plan 読み取り補助は必要最小限の共通 API とする。raw JSON の独自再実装、wire 移行、既存 evidence state の仕様変更に広げない。依存追加は原則不要。

## 必須テスト

すべて `tempdir()` と明示的 project root を使用する。CLI テストの HOME/XDG/config は子プロセスだけに隔離し、実ユーザーの設定と認証情報を読まない。実 LLM やネットワークは使わない。

| 分類 | ケースと期待結果 |
|---|---|
| 基本 | directive→requirement→plan→change→verification を作り、リンク・時刻・結果が両形式で一致 |
| 状態 | pending、成功、失敗、timeout、stale、diverged、missing、reverted、superseded、mixed を維持 |
| 誤認防止 | 要求に成功観測があっても別 obligation が pending なら未確認項目が残る。plan completed でも同様 |
| 履歴 | 検証開始後の変更と、後から変更した plan/requirement/obligation 定義を過去の結果へ付け替えない |
| 古い形式 | v1〜v4 混在、未知版、重複 ID、壊れたイベント、イベント session ID 不一致を扱い、警告を保持 |
| 欠落 | 証跡なし、plan なし、壊れた plan、provenance_incomplete が成功保証に化けない |
| CLI | exact/prefix/曖昧/不明 ID、format 不正、include-content、JSON単独出力、既存 session コマンド |
| 読取のみ | セッション・plan・provenance・Git index の bytes を前後比較。存在しない session root を作らない |
| Git | commit 済み・stage・unstaged・untracked・削除・rename・相殺差分・dirty index・base指定を含む temp repo |
| 帰属 | 同じファイルの外部追記、session にないファイル、シンボル一致だが file 不一致、別セッション由来を正直に表示 |
| 範囲 | Git root と project root が違う場合、ignored/runtime 除外、project 外パス、symlink |
| Git 異常 | non-Git、Git 実行不可、unborn HEAD、無効 base、timeout、capture truncation、特殊形式の incomplete 表示 |
| 読取競合 | 注入可能な採取境界でファイル・plan・event 集合を変更し、再試行または明示エラー。sleep依存の flaky test は避ける |
| 整形 | 日本語、改行、pipe、backtick、HTML、制御文字。本文 opt-in 無しで directive/diff/log が漏れない |
| 不明値 | test count、実行時環境が記録されていない場合は null/not_recorded。現在環境から補完しない |
| 決定性 | 固定時刻と同一入力で同一出力。配列順が安定し、別形式でも manifest digest が同一 |
| 上限 | 入力/出力上限超過で部分 JSON や成功レポートを出さない |

test fixture の期待値は生成器の同じ関数で作らず、利用者が観測する挙動を検証する。Git hook や外部 diff helper が起動しないことも marker fixture で確認する。

## 検証コマンドと受け入れ条件

実装時の focused test 名は実在するモジュールに合わせる。ゼロ件を成功としない。

```bash
bash scripts/verify.sh test features::evidence_report::
bash scripts/verify.sh test session::
bash scripts/verify.sh test provenance::
cargo fmt --all
bash scripts/verify.sh rust
bash scripts/verify.sh guidance
```

- CLI の一時リポジトリ smoke test も実施する。Markdown を人間が読み、JSON を parser で確認する。
- TUI と依存グラフを変更しない限り、TUI screenshot や追加の dependency/MSRV gate は不要。範囲を変更した場合は AGENTS.md の追加 gate を適用する。
- 最終報告には実装した CLI、出力例、テストコマンドと結果、残る制約を含める。成功したテスト結果と、未実施/環境ブロックを混同しない。
- 完成条件は「成功した検証・不足・古い結果・追跡外変更を区別できる」「過去の帰属を保つ」「読み取りで対象を変えない」「保存記録にない保証を作らない」「必須 gate が通る」。

## 実装者への引き継ぎ

この計画に沿って初期版を実装する。最初に AGENTS.md、mutation/provenance と verification の Skills、必要な execution guidance を読む。新規 tool は追加しないため、tool schema/dispatch の拡張は不要。

利用者に見える CLI と意味は上記を基準にする。内部の命名・関数分割などは既存実装に合わせて判断し、繰り返しの承認確認は不要。仕様を大きく変える必要が出た場合は、問題と代替案を具体的に提示する。サブエージェント利用は本計画で許可していない。

計画作成時には runtime を変更していない。前の調査で provenance 139件、semantic_edit 12件、observation 9件の限定テストが成功したが、これは新機能の検証結果ではない。実装後は上記の gate を実行する。

## 参照資料

ローカル実装の事実は上表のコードを優先する。外部仕様は 2026-10-02 に確認した次の公式資料を参照する。Git の実装では利用環境の対応版も確認する。

- [Git diff](https://git-scm.com/docs/git-diff): commit と working tree の比較、NUL 出力、external diff/textconv の制御。
- [Git status](https://git-scm.com/docs/git-status): porcelain、NUL 区切り、optional locks に関する挙動。
- [Git rev parse](https://git-scm.com/docs/git-rev-parse): revision の検証と end-of-options。
- [開発契約](contracts.md)と[検証手順](workflow.md)。
