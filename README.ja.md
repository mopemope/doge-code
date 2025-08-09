# doge-code（日本語版）

Rust（Edition 2024）で書かれたインタラクティブなTUI型コーディングエージェントです。OpenAI互換のLLMを活用し、コードの読み取り、検索、編集を行うほか、ストリーミング出力と基本的なリポジトリ解析を備えた高速かつミニマルなTUIを提供します。

## 主な機能

- TUIモード
- OpenAI互換のChat Completions API
  - ストリーミング出力（TUI）とツール利用エージェントループ
  - 基本的なエラー処理およびキャンセル機能
- 安全なファイルシステムツール（プロジェクトルートに制限）
  - `/read`、`/write`、`/search`（パス正規化とバイナリファイルガード付き）
- リポジトリマップ（Rustのみ、tree-sitter使用）
  - `/map` コマンドでRust関数を一覧表示（将来的に構造体/列挙型/トレイト対応予定）
- TUI UX
  - ステータスインジケーター付きリアルタイムストリーミング出力（Idle/Streaming/Cancelled/Done/Error）
  - Escキーでストリーミングをキャンセル
  - 最大ログサイズを超えると自動的に古いログを切り捨て
  - 入力欄で @-ファイル補完
  - 新機能：`/open <path>` でエディタを起動し、TUIに安全に戻る

## 要件

- 安定版Rustツールチェイン
- OpenAI互換エンドポイントへのネットワークアクセス（デフォルト：https://api.openai.com/v1）
- 環境変数 `OPENAI_API_KEY` または `--api-key` フラグで指定するAPIキー

## インストール

```bash
cargo build --release
```

ビルド後の実行ファイル:

```
target/release/doge-code
```

## 設定

TOML設定ファイル、CLIフラグ、環境変数（dotenv対応）を利用して設定できます。優先順位は CLI > 環境変数 > 設定ファイル > デフォルト値 です。

設定ファイルの検索順序（XDG Base Directory仕様）:
1. `$DOGE_CODE_CONFIG`（明示的な設定ファイルパス、最優先）
2. `$XDG_CONFIG_HOME/doge-code/config.toml`
3. `~/.config/doge-code/config.toml`
4. `$XDG_CONFIG_DIRS` の各ディレクトリ（コロン区切り）にある `dir/doge-code/config.toml`

サンプル `config.toml`:

```toml
# ~/.config/doge-code/config.toml
base_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"
api_key = "sk-..."
log_level = "info"
# プロジェクトルートを絶対パスで上書き（オプション）
# project_root = "/path/to/project"
```

- 設定ファイルが正常に読み込まれると、ファイルパスとともに `loaded config file` というログが出力されます（APIキーはログに含まれません）。
- パース失敗時は警告を出力し、環境変数/CLI/デフォルト設定で継続します。

CLIおよび環境変数:

- `--base-url` / `OPENAI_BASE_URL`（デフォルト：`https://api.openai.com/v1`）
- `--model` / `OPENAI_MODEL`（デフォルト：`gpt-4o-mini`）
- `--api-key` / `OPENAI_API_KEY`
- `--log-level` / `DOGE_LOG`（デフォルト：`info`）

例:
```bash
OPENAI_API_KEY=sk-... \
OPENAI_BASE_URL=https://api.openai.com/v1 \
OPENAI_MODEL=gpt-4o-mini \
DOGE_LOG=debug \
./target/release/doge-code
```

## 使い方

フラグなしで起動するとTUIが開始します:

```bash
./target/release/doge-code
```

- スラッシュなしの入力はLLMプロンプトとして送信されます。
- TUIコマンド:
  - `/help` — コマンド一覧を表示
  - `/map` — リポジトリマップを表示（Rust関数のみ）
  - `/tools` — 利用可能なツール一覧
  - `/clear` — ログエリアをクリア
  - `/open <path>` — ファイルをエディタで開く
  - `/retry` — 直前のプロンプトを再送信
  - `/cancel` — 実行中のストリーミングをキャンセル
  - `/quit` — TUIを終了
- ステータスはヘッダーに表示されます（Idle/Streaming/Cancelled/Done/Error）。
- Esc or Ctrl+C（1回）でキャンセル。3秒以内にCtrl+Cを2回で終了。
- 入力履歴は `~/.config/doge-code/...` に保存されます（XDG準拠）。

### @-ファイル補完

- `@` を入力するとプロジェクト内ファイルのパス補完が起動します。
- ジョイスティックや矢印キーで選択し、Enterで挿入。
- 最近使用したパスが優先されます。

### 新機能: `/open <path>`

- TUI画面を退避してエディタを起動し、終了後に復帰します。
- エディタ優先順: `$EDITOR` → `$VISUAL` → `vi`。
- パス解決:
  - 相対パスは `project_root` から
  - 絶対パスも可
  - 存在しないパスはログにエラー表示
- 例: `/open @src/tui/view.rs` などで@補完を活用。

## 安全性

- すべてのファイル操作はプロジェクトルート内に限定。
- 検索は一般的なバイナリ/大容量ファイルを除外。glob指定で範囲を絞れる。

## ツールモジュール構成

- `src/tools/mod.rs` — モジュール定義と再エクスポート
- `src/tools/common.rs` — `FsTools` 構造体（project_rootの保持）
- `src/tools/read.rs` — `fs_read` 実装とパス正規化
- `src/tools/write.rs` — `fs_write` 実装（project_root制限、親ディレクトリ作成）
- `src/tools/search.rs` — `fs_search` 実装（glob検索、バイナリ除外）

Public API は `tools::mod` で `pub use` されます。

## 開発者向け情報

- Rust 2024 Edition
- 同時実行: tokio + reqwest（HTTP、ストリーミングトークン処理）
- パース: `tree-sitter` + `tree-sitter-rust`（リポジトリマップ用）
- ロギング: `tracing` → `./debug.log`（`DOGE_LOG`設定に従う）
- テスト: `cargo test`

## ロードマップ

- リポジトリマップ強化: 構造体/列挙型/トレイト/implメソッドとフィルタ機能
- 構造化関数呼び出しを用いたリッチなツール連携
- テーマ/カラーモード切替、最大ログ行数設定
- セッション管理強化とUX改善

## ライセンス

MIT または Apache-2.0
