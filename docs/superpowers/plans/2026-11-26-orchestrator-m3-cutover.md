# Orchestrator M3: Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Codex など superpowers が無い環境では、各タスクを上から順に実行し、完了ごとに `docs/orchestrator/PROGRESS.md` を更新してコミットする。

**Goal:** 旧 LLM 直結ランタイム（ルートパッケージ `doge-code` と `src/`）を削除し、オーケストレーターのバイナリ `dgc-next` を `dgc` に改名する。旧ランタイム専用の依存、スクリプト、ドキュメント、skills、Emacs 連携、評価ツールも削除し、AGENTS.md・README・検証スクリプト・CI を新構成に合わせる。

**Architecture:** ルート `Cargo.toml` を仮想ワークスペース（`[workspace]` のみ、members = `crates/*`）にする。MSRV と edition は `[workspace.package]` が唯一の出どころになる。新 crate のコードは改名以外変えない。

**Tech Stack:** Cargo workspace, Python 3.11 検証スクリプト, GitHub Actions

**Spec:** `docs/superpowers/specs/2026-11-25-orchestrator-design.md` §10 M3。ユーザー決定（2026-11-26）: Emacs 連携（`elisp/`）は削除。旧エージェント評価・修復受け入れツールと関連文書は削除し、AGENTS.md の Product priority を書き直す。

## Global Constraints

- 削除はすべて git 上で行う（作業ツリー外のファイルや未追跡ファイルには触れない。`docs/ai/audit-2026-10-29.md` と `docs/superpowers/plans/2026-10-29-patch-safety.md` はユーザーの未追跡ファイルなので触らない）。
- バイナリ名 `dgc`。デーモンの状態・ソケット・設定の場所（`dgc` 名）は変えない。`DGC_STATE_ROOT`、`DGC_LOG` も変えない。
- MSRV `1.94`、開発ツールチェーン `1.99.0`（`rust-toolchain.toml`）は変えない。
- 新 crate の挙動は変えない（改名に伴う文言・パス変更のみ）。挙動の変更は M4 以降。
- 検証スクリプトは引き続き「ゼロ件実行を合格にしない」性質を保つ。
- `CLAUDE.md` は `@AGENTS.md` のまま。

## Review Focus

1. ルートが仮想ワークスペースになり `manifest["package"]` が無い → verify.py msrv、check_ci、check-tui-deps が `workspace.package.rust-version` / `dgc-cli` パッケージを読む（Task 1 にテスト）。
2. バイナリ改名漏れ → テスト・ヘルプ・エラーメッセージ・README に `dgc-next` が残らない（`rg dgc-next` がゼロ、ただし PROGRESS.md や過去の計画書など履歴文書は除く）。
3. Cargo.lock から旧依存（sea-orm、reqwest、tree-sitter 群、rmcp、axum 等）が消える。
4. CI の macOS ジョブが新しいモジュールのテストを実際に実行する（ゼロ件にならない）。
5. AGENTS.md・README が、存在しないコード・スクリプト・skills を参照しない（ガイダンス検査で検出）。

---

### Task 1: ビルドと検証の切替

**Files:**
- Delete: `src/`, `tests/`（ルート結合テスト）, `resources/`, `.doge/`, `.dogeignore`, `elisp/`, `scripts/{run-agent-evals.py,compare-agent-evals.py,agent_eval_api_budget.py,agent_eval_common.py,agent_eval_process.py,repair_acceptance.py,repair_acceptance_contracts.py,timeout_eval_contract.py,check-fixed-acceptance-fixtures.py}` と対応する `scripts/tests/test_*.py`
- Modify: ルート `Cargo.toml`（仮想ワークスペース化。`[workspace.package]` に edition/rust-version/version、`[workspace.dependencies]` は新 crate が使うものだけ）、`Cargo.lock`、`crates/dgc-cli/Cargo.toml`（`[[bin]] name = "dgc"`）、`crates/dgc-cli/**`（`dgc-next` → `dgc`：clap の name、ヘルプ、エラー文、テストの `CARGO_BIN_EXE_dgc`）、`scripts/verify.py`、`scripts/check-agent-guidance.py`、`scripts/check-sensitive-logging.py`、`scripts/check-test-integrity.py`、`scripts/check-tui-deps.py`、`scripts/tests/test_development_checks.py`（と残す checker のテスト）、`.github/workflows/ci.yml`、`.gitignore`（`.doge` 関連の行があれば整理）
- Test: 既存の新 crate テスト一式 + `scripts/tests`

**Interfaces（Produces）:**
- `verify.py msrv` と `check_ci` は `tomllib` で `manifest["workspace"]["package"]["rust-version"]` を読む。
- `check-tui-deps.py` は `dgc-cli` パッケージ（または全ワークスペース）の crossterm 世代を検査する。旧 0.28 の検出ロジックは残す。
- `check-sensitive-logging.py`: 対象を新 crate の「プロンプト・エージェント出力を扱う範囲」（`crates/dgc-daemon/src/provider/`、`crates/dgc-daemon/src/agent*`、`crates/dgc-cli/src/tui/`）に付け替える。検査規則（tracing に本文を出さない）は既存の規則を流用し、新対象で誤検出が出る場合は規則ではなく対象範囲で調整する。付け替えが成立しない場合は削除し、理由を PROGRESS に書く（実装者判断、レポートに明記）。
- `check-test-integrity.py`: 旧ファイル固有の項目（`src/llm/...`、`DOGE_SKIP_HTTPTEST`）を削除し、`crates/**/tests/*.rs` と `crates/**/src/**/*.rs` に対して「無条件の `return` で終わるテスト」「`#[ignore]` の無断追加」等、既存の汎用検査だけを残す。汎用検査が無ければ checker ごと削除。
- `check-agent-guidance.py`: 旧ツールカタログ検査（`src/llm/tool_def.rs` 等）を削除。リンク検査、CLAUDE.md 検査、CI とラッパーの一致検査、workflow.md のコマンド一致検査は残す。skills 検査は skills が 0 件でも通るようにする（Task 2 で skills を削除するため）。
- `verify.py macos`: フィルタを新 crate のプロセス・ソケット系に変更（例 `process`, `server`, `tui::`）。ゼロ件拒否の仕組みはそのまま。
- CI: test ジョブは `cargo test --locked --workspace`。ripgrep の導入ステップが不要なら削除（旧 search ツール用）。

- [ ] **Step 1: 検証スクリプトのテストを先に更新** — `test_development_checks.py` に: 仮想ワークスペースの Cargo.toml（`[workspace.package] rust-version`）から msrv コマンドが作られる、`check_ci` が workspace の rust-version を読む、`check-tui-deps` が `dgc-cli` を対象にする、macos モードのフィルタが新モジュール名。旧カタログ検査のテスト（`GuidanceTests` の tool 系）を削除。
- [ ] **Step 2: 失敗を確認** — `python3 -m unittest discover -s scripts/tests -p 'test_*.py'`
- [ ] **Step 3: 削除と改名と付け替え**（`git rm -r` で削除。Cargo.lock は `cargo build --workspace --offline` で更新）
- [ ] **Step 4: 確認** — `cargo build --locked --workspace`、`cargo tree --workspace | rg -c "sea-orm|reqwest|tree-sitter|rmcp|axum"` が 0、`rg -n "dgc-next" crates README.md AGENTS.md scripts .github` が 0（README と AGENTS は Task 2 で直すので、この時点では crates/scripts/.github のみ 0）、`rg -n "CARGO_BIN_EXE_dgc\b|CARGO_BIN_EXE_dgc\"" crates` でテストが新名を使う。
- [ ] **Step 5: 全検証** — `cargo fmt --all`、`bash scripts/verify.sh rust`、`guidance`、`msrv`、`tui-deps`（旧 src が無いので `/tmp/.git` 由来の失敗は消えるはず。残れば記録）
- [ ] **Step 6: コミット** `refactor!: replace the legacy runtime with the orchestrator as dgc`（PROGRESS 更新を含む）

### Task 2: ドキュメントとガイダンスの切替

**Files:**
- Delete: `docs/ai/` のうち旧ランタイム専用の文書（`contracts.md`, `evaluation.md`, `eval-cases.json`, `eval-run.example.json`, `eval-timeout-validation-v2.md`, `repair-acceptance.md`, `repair-cases/`, `fixtures/`, `instruction-policy.md`, `plan-policy.md`, `main-agent-resource-budget.md`, `subagent-resource-budget.md`, `tool-routing-exceptions.json`, `responses-native-compaction.md`, `openai-subscription-*.md`, `evidence-report-*.md`, `verification-snapshot-work-note.md`, `review-handoff-plan.md`, `harness-efficiency-2026-11-06.md`, `develop-investigation-2026-10-11.md`）、`docs/tool-output-contract.md`, `docs/mcp-3x-migration.md`, `docs/ai/skills/`（5 skills）, `.agents/skills/*`, `.claude/skills/*`, `.opencode/skill/*`（skills へのリンク）。`docs/ai/audit-2026-10-29.md` は未追跡なので触らない。
- Rewrite: `README.md`（`dgc` オーケストレーターだけの内容に。旧機能の節は削除。インストール、デーモン、プロジェクト／ワークスペース、エージェント、権限、ログ、TUI、保存場所、セキュリティ上の性質、systemd/launchd 例、開発者向け検証コマンド）、`AGENTS.md`（Product priority を「Claude Code / Codex を安全に監督するローカルオーケストレーター」に書き直し。Orchestrator migration 節は「完了」に。Shared constraints を新 crate の規則に置き換え：crate 構成と責務、依存方針、権限・ソケットのセキュリティ不変条件、tracing に本文を出さない、テストは tempdir と DGC_STATE_ROOT、TUI の権限安全規則を壊さない、固定ツールチェーン 1.99.0 の注意。ガイダンスの表は新しい文書へ）、`docs/ai/workflow.md`（skills の節を削除し、検証マトリクスを新構成に）、`docs/ai/architecture.md`（新構成の概要に書き直し：crate、デーモン、プロトコル、アダプタ、TUI、保存、セキュリティ境界。設計書と PROGRESS 決定事項へのリンク）
- Modify: `docs/orchestrator/PROGRESS.md`（M3 完了）

- [ ] **Step 1:** 削除と書き直し。README の各コマンド・キー・挙動は実装（`crates/dgc-cli`）と一致させる（M1/M2 の README 節を土台にし、`dgc-next` → `dgc`）。
- [ ] **Step 2:** `rg -n "dgc-next|src/llm|src/features|FsTools|MutationReceipt|JobManager|RepoMap|dgc exec|mcp-server" README.md AGENTS.md docs/ai docs/orchestrator/m2-tui-screen.txt` が 0（PROGRESS.md と docs/superpowers 配下の履歴文書は除外）。
- [ ] **Step 3:** `bash scripts/verify.sh guidance` が PASS。
- [ ] **Step 4: コミット** `docs: describe dgc as the orchestrator and drop legacy guidance`

### Task 3: M3 受け入れ

- [ ] **Step 1:** `cargo install --path crates/dgc-cli --root <scratch>` 相当でビルドした `dgc` を、スクラッチの `DGC_STATE_ROOT` で `dgc daemon status`、`dgc project add`、`dgc workspace create --worktree`、偽プロバイダでの `dgc run --follow`、`dgc tui` の起動と終了（tmux）まで確認し、PROGRESS 検証記録に書く。実プロバイダは使わない。
- [ ] **Step 2:** 全検証（固定ツールチェーン 1.99.0）: fmt、`verify.sh rust`（標準設定で通ること）、guidance、msrv、tui-deps。結果を記録。
- [ ] **Step 3: コミット** `docs(orchestrator): complete M3 cutover acceptance`、PROGRESS の M3 を完了にし、「現在の作業」を M4（プロファイル、スケジュール、heartbeat、スクリプト、レート制限時の引き継ぎ提案）の計画作成にする。
