# develop 調査・機能提案（2026-10-11）

> 履歴資料：2026-10-11 時点の調査です。当時の再現・測定・提案は保存していますが、
> 現行コードの残タスクや現在の優先順を示しません。現在の方向は
> [実修正の採用評価](evaluation.md#practical-acceptance-evaluation)と
> [オフライン評価手順](repair-acceptance.md)を参照してください。

## 対象と結論

対象は `f290364e5b3782d2c12fb39183ccdf6ec1e6689f`（2026-10-10 更新）。
GitHub の `refs/heads/develop` とローカル HEAD の一致を
`git ls-remote https://github.com/mopemope/doge-code.git refs/heads/develop` で確認した。
調査開始時の作業ツリーはクリーン。実装・既存テスト・設定は変更していない。
この文書だけを調査成果として追加した。

最も追加価値が大きいと判断したのは、既存の影響分析・検証義務・実行証跡を
利用者が一度の操作で扱える「検証ワークフロー」である。
先に TSX の文法選択と対応言語の表記を修正し、評価ケースを具体化する。
日常の開発支援を想定した順序であり、未信頼リポジトリへの利用拡大が目的なら
プロジェクト信頼設定と OS 実行隔離を最優先に繰り上げる。

これは網羅的な安全性認証ではない。下記では再現済みの問題、コードから確認した
仕様上の限界、提案を区別する。工数は相対的な規模であり、日数の見積もりではない。
性能向上・トークン削減率は実測していない。

## すでにある強み

直近75件の非マージコミット、README、CLI/TUI の入口、主要実装と対象テストを確認した。
特に次の機能は追加提案と重複させない。

- 要求→計画→実際の変更→検証の provenance、検証義務、古い証拠の識別。
- `impact_analyze` の呼び出し元探索、検証コマンドの提案と広範囲検証へのフォールバック。
- Markdown/JSON の証跡レポート、スナップショットに結び付くローカルの人間レビュー。
- TUI の差分行コメント、修正バッチ、履歴、コメントの編集・削除。
- 原子的セッション保存、排他的所有権、未保存状態の再保存、モデル選択の保存・復元。
- 通常16 MiBのチェックポイント、容量超過時の最大64 MiBの復旧エクスポート。
- Observation Store、未参照結果の保護、コンテキスト予算と実行全体の利用量予算。
- MCP の接続・キャンセル・遅延公開、比較評価ハーネス、独立した検証の仕組み。

実装の入口は [architecture](architecture.md)、利用者向け契約は [README](../../README.md)。

## 先に直すべき具体的な点

### 1. TSX の文法選択（再現済み）

[language_config.rs](../../src/analysis/language_config.rs) は `ts` と `tsx` の両方に
`LANGUAGE_TYPESCRIPT` を割り当てる。[parser.rs](../../src/analysis/parser.rs) は
この登録からパーサを選ぶ。

現在のロック済み依存をリンクした一時 Rust プローブで次を解析した。

```tsx
export function App() { return <div>Hello</div>; }
```

```text
current LANGUAGE_TYPESCRIPT: has_error=true
LANGUAGE_TSX:                has_error=false
```

現在の文法ではサンプルが ERROR ノードになり、TSX 文法では
function_declaration と jsx_element が得られた。
RepoMap のシンボル抽出や意味編集の構文判定への影響が懸念されるが、
今回の動的再現は文法選択と構文木までで、全利用経路の失敗を主張するものではない。

既存の `every_registered_grammar_parses_with_current_runtime` は各登録の先頭拡張子を使い、
TypeScript のサンプルだけを試すため、このケースを検出しない。
修正案は ts と tsx の文法登録を分け、JSXを含む関数抽出と意味編集の回帰を追加すること。
`.jsx` も現在の拡張子マップには登録されていないため、対応範囲を明示して追加を検討する。

再現用ファイル: `/tmp/dgc-tsx-probe-sf9b9mim/probe.rs`。
一次資料: [tree-sitter-typescript](https://github.com/tree-sitter/tree-sitter-typescript)。

### 2. 言語・保存方式のドキュメント不一致（静的確認済み）

README は Java と tree-sitter-java を対応一覧に載せるが、Cargo.toml と
`language_configs()` に Java はない。Java の構造解析を実装するか、対応表記を修正する。
一般的なテキスト読み書きまで不可という意味ではない。

`docs/ai/architecture.md` のモジュール表は `src/session/` を SQLite persistence と
記載するが、実際の SessionStore と最新 README は JSON チェックポイントである。
同一情報の重複を整理し、言語レジストリとドキュメントの一致を検証する小さなチェックが有用。

### 3. RepoMap の解析器更新に伴うキャッシュ無効化（設計上の不足）

[analyzer.rs](../../src/analysis/analyzer.rs) の本番経路 `build_with_cache` は
保存済みと現在のファイルハッシュが同じならキャッシュをそのまま返す。
[cache.rs](../../src/analysis/cache.rs) の `is_cache_valid` にもバージョン比較の TODO が残る。
この判定には grammar / collector / 解析スキーマの版が含まれない。
解析器だけ更新しソースが変わらない場合に、古い解析結果を再利用する経路が残る。

提案は分析フォーマットと collector の明示的リビジョンによる fingerprint。
変更時には rebuildable な RepoMap だけを無効化し、セッションと provenance は維持する。
今回、実バージョン間アップグレードでの再現は行っていない。

## 追加機能の優先順位

| 優先 | 提案 | 主な価値 | 相対規模 |
|---|---|---|---|
| 1 | 検証ワークフローとテスト結果の統合 | 変更後に何を実行し、何を確認できたかを一貫して示す | 中 |
| 2 | 永続変更履歴と再開後の undo | 中断・終了をまたいで安全に変更を戻せる | 大 |
| 3 | プロジェクト信頼設定と実行隔離 | 許可済みの範囲で継続的に作業できる | 大 |
| 4 | パス別 AGENTS.md / Skills の解決と表示 | 大きなリポジトリの局所規則を確実に適用できる | 中 |
| 5 | セッション一覧・保持・復旧の改善 | セッション増加と容量超過に対応する | 中〜大 |
| 6 | monorepo 向け検証・意味解析の改善 | パッケージに合った検証と呼び出し元の精度向上 | 中〜大 |

### 1. 検証ワークフロー

現状: `impact_analyze` は提案専用で、検証を実行しない。
構造化した永続テスト結果の [ResultFormat](../../src/features/structured_test_results/mod.rs) は
GoTestJson のみ。`/test` に Rust/Python 等の解析があっても、これは永続証跡の共通形式とは別。
[obligations.rs](../../src/provenance/obligations.rs) の observed_passing は、
リンクした変更を観測したコマンドの成功で決まる。
現行仕様も「正しさの証明」とはしておらず、それ自体を実装バグとは扱わない。

最小版の提案:

- 仮称 `/verify` で検証候補、根拠、広範囲検証への切替理由を表示する。
- 既存の `execute_process` / JobManager を使い、許可された範囲で実行する。
- 終了コード、実行されたテスト件数、skip、出力不足、タイムアウトを別々に保存する。
- 未実行・0件・失敗・結果不明・変更後で古い証拠を区別する。
- Rust/Python から結果アダプタを追加し、JavaScript 系へ拡張する。
- 同じ変更に対する有効な結果は再利用し、後続の変更で必要なものだけ stale にする。

受入条件: 0件のフィルタ、skipのみ、切れた出力、途中キャンセル、実行中の追加変更、
正常な複数件成功で結果が区別される。モデルの自己申告だけで検証状態を更新しない。
実行結果の証跡収集を先に実装し、自動的な失敗修正ループはその後に比較評価する。

境界: change_impact → execution/jobs → structured_test_results → provenance → TUI/evidence。

### 2. 永続変更履歴と再開後の undo

現状: [undo.rs](../../src/tools/undo.rs) は最大20件のメモリ内 VecDeque。
[common.rs](../../src/tools/common.rs) の FsTools 初期化時に空で作られる。
会話や変更の証跡を復元できても、そのまま再起動前の UndoStack を復元する設計ではない。

提案はセッション配下の変更ジャーナルと内容アドレス付き preimage 保存。
まず1ファイルの tracked mutation を対象に、内容ハッシュ・対象パス・現在状態を再確認して戻す。
外部編集との衝突では拒否し、作成したファイルだけを安全に削除する。
provenance のハッシュ・diff を万能なバックアップと扱わず、復元データを別途保存する。

受入条件: 再起動後の undo、外部編集との競合、ディスク満杯、記録中クラッシュ、
保持期限で preimage が消えた場合を検証する。
複数ファイルの操作単位 undo は次段階とし、任意のシェル・MCP の副作用まで
巻き戻せるとは約束しない。既存共有 commit helper と MutationReceipt を利用する。

### 3. プロジェクト信頼設定と実行隔離

現状: [execution config](../../src/config/execution.rs) の既定値は unrestricted / shell 有効。
プロジェクト設定がユーザー設定をフィールド単位で上書きする。
[policy](../../src/execution/policy.rs) はプログラム、cwd、環境変数の上書きを確認するが、
通常の [runner](../../src/execution/runner.rs) は親環境を継承してプロセスを起動する。
許可した cargo や git から起動される任意コードのファイル・ネットワークアクセスを
OS レベルで隔離するものではない。今回、攻撃や情報流出の実証はしていない。

最小版は信頼済みプロジェクトの識別、設定の出所表示、ユーザー側の上限制約を
プロジェクトから緩和できないマージ規則、明示的な環境変数継承ポリシー。
続いて filesystem/network の隔離を設け、未対応環境では保護の不足を明示する。
MCP stdio プロセスも検討対象に含め、リモートサービスの副作用は接続先・ツール別の権限で管理する。
すべての呼び出しに確認を増やすのでなく、一度認めた範囲と永続する規則を明確にする。

受入条件: 未信頼のプロジェクト設定による権限拡張を拒否し、許可外書込み・通信・
環境変数継承を合成 fixture で検証する。設定変更時の信頼更新も明示する。

一次資料: [Anthropic の OS 隔離設計](https://www.anthropic.com/engineering/claude-code-sandboxing)。
MCP の annotation を権限の証明として使わないという境界は現実装にもあり、維持すべき。
[MCP 公式の annotation 解説](https://blog.modelcontextprotocol.io/posts/2026-03-16-tool-annotations/)。

### 4. パス別の指示・スキル解決

現状: [prompt.rs](../../src/features/agent_prompt.rs) は指定ファイル、またはルートの
AGENTS.md / QWEN.md / GEMINI.md を選択する。
ネストした指示とスキルは自動探索・適用されず、必要に応じて読む運用。
これは [instruction-policy](instruction-policy.md) に明記された仕様である。

提案は編集対象パスに適用する祖先ディレクトリの指示を決定的に解決し、
読み込んだパス・ハッシュ・優先順位を表示すること。
Skills は metadata → 選択した本文 → 必要な参照の順に読み込む。
スキルの記述だけで実行権限を拡大せず、予算超過・読取失敗・途中の変更を明示する。
変更した指示を圧縮後・再開後にも取り違えないテストが必要。

一次資料: [Agent Skills specification](https://agentskills.io/specification)。

### 5. セッションの運用改善

現状の [SessionStore](../../src/session/store.rs) は一覧のために各 session.json を全文読み、
合計128 MiBまたは10,000エントリを超えると明示的に失敗する。
モデル選択付きの新しい保存経路には古い100件自動削除を適用しない。
保持件数だけを一律に「100件」と考えると運用を誤る。
容量超過用 export は存在するが、そのファイルの import / oversized resume はない。

段階的な提案:

1. 再構築可能なセッションサマリー索引とページング、サイズ・更新時刻表示。
2. `archive` / `prune --dry-run`、pin、明示的な保持設定。active と復旧データは保護する。
3. 復旧インポートは新しいセッションへ取り込み、元データと provider binding を保持・検証する。
4. 別モデルへは会話内部形式を流用せず、要求・変更・未完了検証・参照をまとめた引き継ぎを作る。

受入条件: 大きな履歴、破損・legacy・active セッション、quota 超過、provider 不一致で
勝手な削除や隠れたデータ欠落を生じない。
索引はキャッシュとして再構築できるようにし、耐久状態を RepoMap DB に混ぜない。
一覧が遅いという性能値は今回測定していない。

### 6. monorepo と意味解析

[verification.rs](../../src/features/change_impact/verification.rs) の Rust 提案は `cargo test` の
プロジェクト単位。TypeScript はルート package.json の scripts を見て npm run を提案する。
Cargo workspace の package、packageManager/lockfile、子 package の cwd を解決すると
既存 impact_analyze の実用性を上げられる。

変更範囲も現状はファイル内シンボルが候補で、Precise は将来用。
呼び出しグラフは名前と静的関係に依存する。
まず差分範囲→シンボル対応を改善し、その後 LSP の定義・参照情報を任意で補完する。
名前の一意性を型解決や実行時カバレッジと混同しない。
LSP 未使用・失敗・曖昧な場合の現行フォールバックは維持する。

## 開発プロセス・評価の改善

評価基盤の新設は不要。既存の [evaluation](evaluation.md) と
[eval-cases.json](eval-cases.json) を具体化する方が効果的。
現在13ケース中、ケース定義に独立 post_checks があるのは1件。
一部の impact ケースは「単一ファイルのバグを直す」などで対象が具体化されていない。
これは人間の受入判定やハーネスの健全性を否定するものではないが、
同じ問題を各 variant に解かせる比較には弱い。

- 固定された壊れた fixture、正確な依頼、エージェントが変更できない独立チェックを追加する。
- 今回の TSX、0件テスト、再開後の undo、ネストした指示、monorepo の検証選択をケース化する。
- 成功率、契約違反、失敗・途中停止、受入成功あたりの利用量・時間を比較する。
- 実モデル評価は手動で予算を決めて実行し、CI は deterministic な回帰とハーネス検証を行う。

独立した評価が重要という考えは
[Anthropic の長時間タスク向け設計](https://www.anthropic.com/engineering/harness-design-long-running-apps)
にもある。ただし、そこでの多エージェント構成や成果・コストを dgc にそのまま適用しない。

追加の保守改善として、約2,000行のREADMEの詳細契約をトピック別文書へ分け、
READMEを導入・主要な流れ・索引にする。agent_loop/history はそれぞれ3,000行以上
（テストを含む）なので、振る舞いを変えないテスト分離と境界整理を段階的に行う。
全面書き直しの根拠にはしない。

## 当面後回しにする案

- 書込み可能な多数の並列エージェント: 変更所有権、証拠統合、競合と回復を先に固める。
- 新しい独自ブラウザ: まず既存 MCP 接続を利用できるかを評価する。
- 自動モデル切替: 現行の provider-bound セッションを安全に引き継ぐ仕組みが先。
- 無条件に長いプロンプトや細かな必須計画: 同条件評価で効果を示してから採用する。

## 推奨する着手順と検証

1. TSX/JSX と対応表記、解析器キャッシュ無効化の仕様を整理する。
2. 固定 fixture と独立した評価チェックを増やす。
3. 検証ワークフロー v1: まず Rust/Python の結果とUI・証跡を接続する。
4. 永続変更履歴、セッション運用、指示解決をそれぞれ小さな契約で追加する。
5. 外部利用を拡大する前に信頼設定と実行隔離を完成させる。

必要な境界ごとに focused tests を実行した。すべて
`bash scripts/verify.sh test <filter>` 経由で、合計181件が成功した。

| filter | 成功件数 |
|---|---:|
| `features::structured_test_results::` | 6 |
| `features::change_impact::` | 44 |
| `tools::mutation::` | 19 |
| `provenance::obligations::` | 20 |
| `config::execution::` | 6 |
| `tui::commands::prompt::` | 5 |
| `analysis::language_config::` | 1 |
| `session::` | 80 |

TSX の一時プローブは別途実行した。既存テストの成功はこの再現を打ち消さない。
`bash scripts/verify.sh guidance` は最初の実行で静的チェック3件に成功し、
Pythonテストはsandboxのlocalhost socket禁止による5エラーで失敗した。
localhost のテスト用サーバーが起動できる環境で再実行し、静的チェック3件と
Pythonテスト131件すべてに成功した。ログ: `/tmp/dgc-verify-i9nr_5ep/`。
全Rustテスト、Clippy、実モデル比較、macOS、手動TUI操作、起動性能測定は未実施。

## 優先修正の実装メモ

ユーザーの修正開始指示により、最初の作業単位を TSX/JSX、対応表記、
解析リビジョンによるキャッシュ無効化に絞った。

受入条件:

- TSX/JSX の React 関数が構文エラーなしに解析され、プロジェクトの RepoMap に含まれる。
- React 関数の意味編集が隣接関数を保持し、構文を壊す候補は書き込まない。
- ソースが同じでも解析リビジョンが未記録・不一致なら全体を再解析する。
- 現行リビジョンでは既存キャッシュを再利用する。
- リビジョン保存が失敗したらキャッシュ全体の変更をロールバックする。
- 旧 SQLite の migration history とデータを保持し、セッション状態に触れない。

影響境界: 拡張子登録→parser/collector→意味編集、および
RepomapStore→DAO→SQLite migration。ユーザー向け契約は README、
解析更新時のリビジョン更新規則は architecture に記録した。
対象検証:

- `bash scripts/verify.sh test analysis::`: 72件成功。
- `bash scripts/verify.sh test features::semantic_edit::`: 13件成功。
- `bash scripts/verify.sh guidance`: 静的チェック3件、Python131件成功。
- `cargo fmt --all`: 実施済み。
- `bash scripts/verify.sh rust`: fmt check、警告なしの全target/feature Clippy、
  全Rustテスト2130件が成功。ログ: `/tmp/dgc-verify-18ehr4yf/`。

この作業単位の受入条件を満たした。次の機能候補は検証ワークフロー v1。
解析器更新時には明示的な `REPOMAP_ANALYSIS_REVISION` の更新が必要で、
その規則を architecture に記載した。依存関係とTUIレイアウトの変更はない。
