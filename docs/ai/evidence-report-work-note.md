# 検証記録エクスポートの作業メモ

- 受け入れ条件は [実装計画](evidence-report-plan.md) に従う。
- 実装済み境界: features/evidence_report、session store/CLI、main の早期分岐、README。
- 既存 wire / writer / query の意味を維持。Git は managed runner の読み取りのみ。
- evidence は dotenv/config/logging 初期化前に分岐し、debug.log・default config・RepoMap・MCP を作らない。診断は stderr。
- 読み取り専用 store は mutation を拒否し、メタデータの件数・容量を制限する。
- manifest は限定範囲の出力時識別情報。前後照合は一度再試行する optimistic check。
- Git の非 UTF-8 / U+FFFD path は lossless な識別ができないため比較 unavailable。submodule は unsupported。明示 base の比較失敗はエラー。
- 検証済み: features::evidence_report:: 24件、evidence_cli 2件、session:: 46件、guidance 28件。
- 最終 Rust gate: `env TMPDIR=/var/tmp bash scripts/verify.sh rust` 成功。fmt、警告なし Clippy、locked tests 1225件を通過。ログ: `/var/tmp/dgc-verify-of3wzx46`。
- 通常 sandbox の gate はローカル HTTP bind の EPERM と `/tmp/.git` の親リポジトリ検出で既存テスト20件が失敗。権限承認後、Git 管理外の TMPDIR と sandbox 外の待受けで再実行した。プロダクトコードの回避変更はしていない。
- 追加重点検証: `bash scripts/verify.sh test provenance::` 139件成功。
- 最終 CLI smoke: API キーなしの隔離環境で JSON parse、Markdown の要確認事項・制約表示、保存済み session の不変、debug.log 非生成を確認。成果物: `/tmp/dgc-evidence-smoke-gtoyk6mw`。
- 最終 guidance gate を作業メモ更新後に実行。実装と必須検証は完了、コミットはしていない。
