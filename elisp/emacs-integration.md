# Doge-Code Emacs Integration

## 概要

Doge-CodeはRustで構築されたインタラクティブなCLI/TUIコーディングエージェントで、OpenAI互換のLLMを使用してコードの読み取り、分析、検索、編集をサポートします。このドキュメントでは、Doge-CodeとEmacsの統合機能について説明します。この統合により、Emacs内でDoge-CodeのAI支援をシームレスに利用できます。

統合は以下の2つの主要な部分で構成されます：
1. **CLIベースの最小実行可能統合 (MVI)**: EmacsからDoge-Codeの`--exec`サブコマンドをサブプロセスとして呼び出し、コード分析やリファクタリングを実行。
2. **MCPサーバーモード**: Doge-CodeをHTTPサーバーとして実行し、Emacsクライアントからツール（例: `search_repomap`、`fs_read`）をリアルタイムで呼び出し。

この統合はEmacsのプログラミングモードで自動的に有効化され、キーバインドで簡単に使用可能です。Doge-Codeのツールシステム（静的コード分析、ファイル操作など）を活用し、コードの分析、説明、リファクタリングを支援します。

## 要件

- **Doge-Code**: ビルド済みバイナリ（`dgc`または`doge-code`）。Cargo.tomlに依存関係が定義されています。
- **Emacs**: バージョン27.1以上。パッケージ: `json`, `async`, `request`, `popup` (MELPA経由でインストール)。
- **APIキー**: `OPENAI_API_KEY`環境変数にOpenAI互換APIキーを設定。
- **プロジェクトルート**: Doge-Codeはプロジェクトルート内で動作します。Emacsバッファのディレクトリがルートとなります。

## インストールと具体的な設定方法

### 1. Doge-Codeのビルドとセットアップ
1. リポジトリをクローン:
   ```
   git clone https://github.com/mopemope/doge-code.git
   cd doge-code
   ```
2. Rustツールチェーンをインストール (rustup経由):
   ```
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source ~/.cargo/env
   ```
3. 依存をインストールしビルド:
   ```
   cargo build --release
   ```
   - バイナリが生成されます: `target/release/dgc` (PATHに追加推奨、例: `export PATH="$PATH:$HOME/.cargo/bin:./target/release"`).
4. APIキーを設定 (環境変数):
   ```
   export OPENAI_API_KEY="sk-your-key-here"
   ```
   - 永続化: `~/.bashrc`または`~/.zshrc`に追加。

### 2. Emacsパッケージのインストール
#### オプション1: 手動インストール (推奨で簡単)
1. 以下のファイルを`~/.emacs.d/lisp/`に保存:
   - `doge-code.el` (CLI統合)。
   - `doge-mcp.el` (MCPクライアント)。
2. `init.el` (または`init.el`相当)に追加:
   ```elisp
   ;; Doge-Code統合のロード
   (add-to-list 'load-path "~/.emacs.d/lisp/")
   (require 'doge-code)
   (require 'doge-mcp)

   ;; モードをプログラミングモードで自動有効化
   (add-hook 'prog-mode-hook 'doge-code-mode)

   ;; バイナリパスをカスタマイズ (必要に応じて)
   (setq doge-code-executable "/path/to/doge-code/target/release/dgc")
   (setq doge-mcp-server-url "http://127.0.0.1:8000")  ; MCPサーバーURL

   ;; ポップアップ使用を有効化 (オプション)
   (setq doge-code-use-popup t)
   ```
3. Emacsを再起動、または`M-x eval-buffer`でinit.elを評価。
4. テスト: 新しいバッファで`M-x doge-code-mode`を実行 → モードラインに"Doge"表示。

#### オプション2: MELPA経由 (将来対応時)
- パッケージとして公開する場合: MELPAレシピを追加。
- 現在は手動推奨。

#### オプション3: straight.el使用 (Emacs 27+)
`init.el`に:
```elisp
(use-package straight
  :ensure t)

(straight-use-package
 '(doge-code
   :type git
   :host github
   :repo "mopemope/doge-code"
   :files ("doge-code.el" "doge-mcp.el")))

(require 'doge-code)
(add-hook 'prog-mode-hook 'doge-code-mode)
(setq doge-code-executable "/path/to/dgc")
(setq doge-mcp-server-url "http://127.0.0.1:8000")
```

### 3. MCPサーバーの設定
1. ターミナルでDoge-Code MCPサーバーを起動:
   ```
   dgc --mcp-server  # デフォルト: http://127.0.0.1:8000
   ```
   - バックグラウンド: `dgc --mcp-server &`。
   - カスタムポート: `dgc --mcp-server 127.0.0.1:9000`。
2. Emacs側でURLを設定 (init.el):
   ```elisp
   (setq doge-mcp-server-url "http://127.0.0.1:9000")
   ```
3. テスト: Emacsで`M-x doge-mcp-list-tools` → ツール一覧表示。

### 4. 基本設定の確認
- **環境変数確認**: Emacsで`M-x shell-command` → `echo $OPENAI_API_KEY` (出力がキーならOK)。
- **バイナリ確認**: `M-x shell-command` → `which dgc` (パス表示ならOK)。
- **モード確認**: rust-modeバッファで`C-h m` → "Doge"キーバインド確認。

## 機能詳細

### 1. CLIベースの統合 (MVI)
EmacsからDoge-Codeの`--exec`コマンドを非同期で呼び出し、選択したコード領域やバッファ全体を分析/リファクタリングします。JSON出力（`--json`フラグ）で構造化レスポンスを解析し、ポップアップまたはバッファに表示。

#### コマンド
- **doge-code-analyze-region** (C-c d a):
  - 選択領域を分析し、改善提案を表示。
  - 例: 選択した関数を分析 → ポップアップに「このコードの改善点: ...」を表示。
- **doge-code-refactor-region** (C-c d r):
  - 選択領域をリファクタリング。
  - 例: コードをベストプラクティスに適合。
- **doge-code-explain-region** (C-c d e):
  - 選択領域の説明（プレーンテキスト出力）。
- **doge-code-analyze-buffer** (C-c d b):
  - 現在のバッファ全体を分析。

#### 使用例
1. Rustファイルを開く。
2. 関数を選択。
3. `C-c d a` を実行 → *doge-output*バッファまたはポップアップに分析結果を表示。
4. JSONレスポンス: `{"success": true, "response": "分析結果", "tokens_used": 150}`。
5. エラー時: メッセージバーに「Doge-Code Error: ...」を表示。

#### カスタマイズ
- `doge-code-executable`: バイナリパス（デフォルト: "dgc"）。
- `doge-code-use-popup`: tでポップアップ表示、nilでバッファ表示。

### 2. MCPサーバーモード
Doge-CodeをHTTPサーバーとして実行（`dgc --mcp-server [address]`）。Emacsクライアントからツールを直接呼び出し、リアルタイム分析が可能。

#### サーバー起動
- ターミナルで: `dgc --mcp-server` (デフォルト: http://127.0.0.1:8000)。
- カスタムポート: `dgc --mcp-server 127.0.0.1:9000`。

#### Emacsクライアントコマンド
- **doge-mcp-search-repomap** (C-c d m s):
  - リポジトリマップをキーワードで検索（例: "function name"）。
- **doge-mcp-fs-read** (C-c d m f):
  - ファイルを読み取り（パス入力）。
- **doge-mcp-list-tools** (M-x):
  - 利用可能ツール一覧を表示。

#### 使用例
1. MCPサーバーを起動。
2. Emacsで: `M-x doge-mcp-search-repomap` → キーワード入力 → 結果バッファにシンボル/コードを表示。
3. レスポンス: JSON形式の検索結果（ファイルパス、シンボル）。

#### MCPツール
Doge-CodeのツールがMCP経由で利用可能:
- `search_repomap`: シンボル検索。
- `fs_read`: ファイル読み取り。
- `fs_list`: ディレクトリ一覧。
- など（rmcp経由で拡張可能）。

## トラブルシューティング

- **APIキーエラー**: `OPENAI_API_KEY`を設定。JSON出力で`{"success": false, "error": "..."}`。
- **サーバー接続失敗**: MCPサーバーが起動中か確認（ポート8000）。ファイアウォールチェック。
- **Emacsエラー**: `M-x load-file`で手動ロード。`M-x toggle-debug-on-error`でデバッグ。
- **出力表示**: *doge-output*バッファを確認、または`C-h v doge-code-use-popup`でポップアップ有効化。

## 将来の拡張

- **LSPサポート**: Doge-CodeをLSPサーバーとして実行（lsp-mode統合）。
- **ストリーミング**: MCP経由のリアルタイムストリーミング（WebSocket）。
- **自動適用**: 分析結果をEmacsバッファに自動挿入/パッチ適用。
- **バッファ統合**: インライン提案（Copilot風）。

詳細はソースコード（doge-code.el, doge-mcp.el）またはリポジトリを参照。問題があれば、Doge-Codeのissueを参照してください。

---

*Generated by Doge-Code Agent on [current date].*