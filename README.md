# Doge-Code

Doge-Code はインタラクティブ AI コーディングエージェントで、ターミナル UI と MCP (Model Context Protocol) サーバーの両方を通じて、高度なコード分析、編集、プロジェクト管理機能を提供します。Rust で構築されたモダンなアーキテクチャは、tree-sitter パース、LLM 統合、永続セッションを組み合わせて、強力なコーディングアシスタント体験を提供します。

## 🚀 主な機能

### コア機能
- **インテリジェントコード分析**: tree-sitter ベースのコードパースとシンボル抽出（Rust、JavaScript/TypeScript、Python、Go、Java、C/C++、C#、Markdown を含む 10+ 言語対応）
- **インタラクティブターミナル UI**: 様々なシンタックスハイライト、diff レビュー、リアルタイム LLM インタラクションを備えた完全機能の TUI
- **MCP サーバー**: Claude Desktop などの MCP 対応クライアントと統合するための Model Context Protocol サーバー
- **永続セッション**: SQLite ベースのセッションストレージで実行間のコンテキストを維持
- **マルチモードインタラクション**: インタラクティブ TUI モードとコマンドライン実行の両方をサポート

### 対応言語
- **Rust** (tree-sitter-rust 0.24.0)
- **JavaScript/TypeScript** (tree-sitter-javascript/typescript 0.25.0/0.23.2)
- **Python** (tree-sitter-python 0.25.0)
- **Go** (tree-sitter-go 0.25.0)
- **Java** (tree-sitter-java 0.23.5)
- **C/C++** (tree-sitter-c/c++ 0.24.1/0.23.4)
- **C#** (tree-sitter-c-sharp 0.23.1)
- **Markdown** (tree-sitter-md 0.5.1)

### 実行モード

#### 1. インタラクティブ TUI モード（デフォルト）
```bash
cargo run --release
# または
dgc
```
コード探索・ナビゲーション、リアルタイム LLM チャットインターフェース、diff レビュー・承認ワークフロー、セッション管理、プロジェクト概要・シンボルブラウジングを提供する完全なターミナルインターフェースを起動します。

#### 2. コマンド実行モード
```bash
dgc exec "データベース接続関数にエラーハンドリングを追加"
```
単一の指示を実行して結果と共に終了します。

#### 3. MCP サーバーモード
```bash
dgc mcp-server 127.0.0.1:8000
```
Claude Desktop や他のクライアントとの統合用に MCP サーバーを起動します。

#### 4. ファイル監視モード
```bash
dgc watch
```
ファイル変更を監視し、自動的に LLM アシスタントを起動します。

#### 5. コードリライトモード
```bash
dgc rewrite --prompt "async/await に変換" --code-file /tmp/code.rs
```
LLM アシスタントで特定のコードスニペットをリライトします。

## 🔧 インストール

### 前提条件
- Rust 1.70+ (Rust Edition 2024)
- OpenAI 互換 API キー（`OPENAI_API_KEY` 環境変数で設定）

### ソースからビルド
```bash
git clone https://github.com/mopemope/doge-code.git
cd doge-code
cargo build --release
```

### 設定
プロジェクトディレクトリに `.doge/config.toml` を作成：
```toml
[llm]
model = "claude-3-5-sonnet-20241022"
base_url = "https://api.anthropic.com"

[project]
instructions_file = "PROJECT.md"
```

## 🛠️ ツールとコマンド

### ファイルシステムツール
- `fs_read`: 大きなファイル用にオプションのサマリーモードでファイルを読み込み
- `fs_write`: ファイルの作成または上書き
- `fs_list`: ページネーションでディレクトリ内容を一覧
- `find_file`: glob パターンでファイルを検索
- `execute_bash`: シェルコマンドを実行（安全警告付き）

### コード分析ツール
- `search_repomap`: 高度なフィルタリングでパースされたコードシンボルを検索
- `search_text`: ファイル全体のテキストベース検索
- `fs_read_many_files`: 予算管理でバッチファイル読み込み

### 編集ツール
- `apply_patch`: 統一 diff パッチを適用（複数ファイル対応）
- `edit`: 特定コードブロックを置換
- `edit_symbol`: シンボル全体（関数、構造体等）を編集

### セッション管理
- `todo_write`/`todo_read`: タスクリスト管理
- `session`: 自動セッション永続化と再開

## 🎯 使用例

### 基本インタラクティブ使用
```bash
# インタラクティブセッション開始
dgc

# 最後のセッションを再開
dgc --resume

# 高速起動のために repomap 生成をスキップ
dgc --no-repomap
```

### コマンドライン操作
```bash
# 単一指示を実行
dgc exec "ユーザ認証モジュールに単体テストを追加"

# プログラム使用用に JSON 出力
dgc exec --json "データベースレイヤをリファクタ"

# 特定コードをリライト
dgc rewrite --prompt "この関数をパフォーマンス最適化" \
    --code-file /tmp/algorithm.rs \
    --json
```

### Claude Desktop 統合
1. MCP サーバー開始: `dgc mcp-server 127.0.0.1:8000`
2. Claude Desktop を `http://127.0.0.1:8000` に接続設定
3. Claude Desktop のチャットインターフェースでプロジェクトツールに完全アクセス

### Emacs 統合
Doge-Code は Elisp による Emacs 統合を含みます：
```elisp
;; doge-code.el と doge-mcp.el をロード
;; M-x doge-mcp-connect で MCP サーバーに接続
;; M-x doge-chat でチャットインターフェースを開く
```

## 🔍 search_repomap チートシート

- `result_density`: 既定の `"compact"` ではスニペットを返さず、1 ファイルあたり 5 シンボルに圧縮。詳細が欲しいファイルだけ `"full"` に切り替えるとコンテキスト節約になります。
- `response_budget_chars`: 「5,000 文字以内」のように上限を渡すと、limit／シンボル数／スニペット長を自動で削り、結果が大きくなりすぎるのを防ぎます。予算内に収まらない場合は `warnings` と `next_cursor` で続きが取得できます。
- `cursor` / `page_size`: ソート済み結果をページ分割できます。`cursor` は 0 ベースの次の位置、`page_size` は取得件数です。`next_cursor` が `Some(x)` なら次ページを同じクエリ + `cursor=x` で取得してください。
- レスポンスは `SearchRepomapResponse` で返り、`results`（従来の `RepomapSearchResult` 群）に加えて `warnings` と `applied_budget`（実際に適用された制限の概要）が含まれます。

これらを組み合わせることで、LLM のコンテキストを圧迫せずに最大効果のコード探索が可能です。

## 📂 ファイル系ツールの軽量モード

- `fs_read`: 既定モードは `mode="summary"` で 400 行 & 6,000 文字までを返し、残りは `next_cursor` で追跡できます。全文が必要な時だけ `mode="full"` や `page_size`/`cursor` を指定してください。
- `fs_read_many_files`: `paths` で解決されたファイル群を `mode="summary"` では 1 ページ 5 件・各ファイル 40 行までで返し、`response_budget_chars` を超えそうな場合は自動的に `warnings` + `next_cursor` を返します。
- `fs_list`: ディレクトリ一覧も `FsListResponse` で返り、`entries` には `path` と `is_dir` だけを載せるためコンパクトです。`cursor`/`page_size`/`response_budget_chars` を活用して深い木構造を段階的に取得してください。

## 🎯 シンボル限定編集 /edit-symbol

- `/edit-symbol` を実行すると、現在表示中の差分レビューまたは直近の `@path:line`／`@path#Lline` で指定されたファイル・行からシンボル（関数/impl/struct など）を特定します。差分レビューが開いていればスクロール位置が対象になり、ファイル指定がなければ十分です。
- 認識したシンボルを LLM に渡し、diff またはシンボル全体の置換を受け取って `apply_patch` で適用します。結果は `diff-review` ペインで確認でき、`a` で承認、`r` で戻すことで反映状況を確認できます。
- 失敗（パッチがない、パーサが壊れている、ファイルが更新された）時にはログに生レスポンスが出力されるので、指示を修正して再度 `/edit-symbol` を呼び出してください。

## 🏗️ アーキテクチャ

### モジュール構成
- **`src/main.rs`**: CLI エントリーポイントとアプリケーションオーケストレーション
- **`src/analysis/`**: tree-sitter ベースのコード分析とシンボル抽出
- **`src/tools/`**: ファイルシステムとコード操作ツール
- **`src/tui/`**: ratatui によるターミナルユーザインターフェース
- **`src/llm/`**: OpenAI 互換 LLM クライアントとツール実行
- **`src/session/`**: SQLite ベースのセッション永続化
- **`src/mcp/`**: Model Context Protocol サーバー実装
- **`src/config/`**: 設定管理と TOML パース

### 主な特徴
- **非同期アーキテクチャ**: 高性能の Tokio ベース
- **メモリ効率的**: 大規模コードベース用の遅延読み込みとページネーション
- **拡張可能**: 追加言語とツールのプラグインシステム
- **型安全**: Rust の強力な型付けで一般的なエラーを防止
- **クロスプラットフォーム**: Linux、macOS、Windows で動作

## 🔍 高度な機能

### RepoMap システム
Doge-Code はプロジェクトの包括的シンボルマップを構築：
- 自動言語検出
- シンボル抽出（関数、構造体、クラス等）
- 相互相関分析
- ファイル変更時の増分更新

### スマート編集
- **シンボル認識編集**: 関数、クラス、モジュール全体を編集
- **diff レビュー**: 変更適用前のプレビュー
- **コンテキスト保持**: コードスタイルとパターンを維持
- **複数ファイル協調**: 関連変更をファイル間で適用

### LLM 統合
- **OpenAI 互換**: OpenAI、Anthropic、他の API で動作
- **ストリーミング応答**: LLM 生成中にリアルタイム出力
- **ツール使用**: LLM が自律的にツールを呼び出し
- **会話履歴**: インタラクション間のコンテキストを維持

## 📚 設定

### 環境変数
- `OPENAI_API_KEY`: API キー
- `OPENAI_BASE_URL`: API ベース URL（デフォルトは OpenAI）
- `OPENAI_MODEL`: モデル名（デフォルトは gpt-4）
- `DOGECODE_CONFIG`: 設定ファイルパス

### 設定ファイル（`.doge/config.toml`）
```toml
[llm]
model = "claude-3-5-sonnet-20241022"
base_url = "https://api.anthropic.com"
max_tokens = 4000
temperature = 0.1

[project]
instructions_file = "PROJECT.md"
exclude_patterns = ["target/", "node_modules/", "*.log"]

[mcp]
address = "127.0.0.1:8000"
```

## 🧪 開発

### ビルドとテスト
```bash
# コードフォーマット
cargo fmt --all

# リンティング実行
cargo clippy --all-targets --all-features

# テスト実行
cargo test

# リリース版ビルド
cargo build --release
```

### 新しい言語の追加
1. `Cargo.toml` に tree-sitter パーサ依存を追加
2. `LanguageSpecificExtractor` トレイトを実装
3. `src/analysis/mod.rs` の言語検出に追加
4. `src/analysis/tests/` でテストを追加

### 新しいツールの追加
1. `src/tools/` ディレクトリでツールを実装
2. `FsTools` トレイト実装に追加
3. `src/tools/mod.rs` で登録
4. テストとドキュメントを追加

## 📖 ドキュメント

- **システムプロンプト**: `resources/system_prompt.md` - AI 行動ガイドライン
- **エージェントガイドライン**: `AGENTS.md` - 統合手順
- **Emacs 統合**: `elisp/emacs-integration.md`
- **API ドキュメント**: `cargo doc` で生成

## 🤝 コントリビューション

1. リポジトリをフォーク
2. 機能ブランチを作成
3. テスト付きで変更を実装
4. `cargo fmt` と `cargo clippy` が通ることを確認
5. プルリクエストを提出

## 📄 ライセンス

このプロジェクトは MIT ライセンスに基づきライセンスされています - 詳細は [LICENSE](LICENSE) ファイルをご覧ください。

## 🙏 謝辞

- 優れたパース機能の tree-sitter
- MCP 仕様の Claude Desktop チーム
- 優れたツール群の Rust コミュニティ
- 全てのコントリビュータとテストユーザ

---

**注**: これは活発な研究プロジェクトです。AI サポート開発の境界を探る中で、機能は変更される可能性があります。