# Orchestrator M0: Workspace Scaffold Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Codex など superpowers が無い環境では、各タスクを上から順に実行し、完了ごとに `docs/orchestrator/PROGRESS.md` を更新してコミットする。

**Goal:** ルートを Cargo workspace にし、新しい 4 crate の空実装を置き、検証スクリプトと CI が workspace 全体を検査するようにする。旧 `dgc` の挙動は変えない。

**Architecture:** ルートの `doge-code` パッケージはそのまま workspace メンバーとして残し、`crates/*` を追加する。共有依存のバージョンは `[workspace.dependencies]` に集約する。

**Tech Stack:** Rust 2024, Cargo workspace, Python 3.11 検証スクリプト, GitHub Actions

**Spec:** `docs/superpowers/specs/2026-11-25-orchestrator-design.md`（§5.1, §5.3, §10 M0, §11）

## Global Constraints

- Edition 2024。ルートパッケージの `rust-version = "1.94"` は**リテラルのまま**残す（`scripts/verify.py` の msrv モードが `manifest["package"]["rust-version"]` を読むため）。新 crate は `rust-version.workspace = true`。
- 新バイナリ名は `dgc-next`。旧バイナリ `dgc` は変更しない。
- 新 crate の依存は設計書 §5.1 の表にあるものだけ。追加時は PROGRESS.md「決定事項」に理由を書く。
- 旧 `src/` のコードを新 crate から参照しない。

## Review Focus

- `cargo test --locked` を workspace なしで実行すると新 crate のテストが走らない → verify.py と CI が `--workspace` を付けていることをテストで固定する（Task 1 Step 1）。
- MSRV チェックが workspace 化で `rust-version` を読めなくなる → msrv モードの既存テストが通ることを確認する（Task 1 Step 6）。
- `Cargo.lock` の更新漏れで `--locked` が CI で落ちる → Task 1 で lock を更新してコミットに含める。
- 旧 `dgc` バイナリが壊れる → `cargo run --bin dgc -- --version` を確認する。
- 新 crate が誤って旧パッケージの重い依存を引き込む → `cargo tree -p dgc-daemon` で sea-orm/reqwest/tree-sitter が出ないことを確認する。

---

### Task 0: 進捗表と AGENTS.md の入口（計画作成時に完了済み）

`docs/orchestrator/PROGRESS.md` と `AGENTS.md` の「Orchestrator migration」節は計画と同じコミットで作成済み。実行者は確認のみ。

### Task 1: Cargo workspace 化と新 crate の足場

**Files:**
- Modify: `Cargo.toml`（`[workspace]`、`[workspace.package]`、`[workspace.dependencies]` を追加）
- Create: `crates/dgc-protocol/{Cargo.toml,src/lib.rs}`
- Create: `crates/dgc-daemon/{Cargo.toml,src/lib.rs}`
- Create: `crates/dgc-client/{Cargo.toml,src/lib.rs}`
- Create: `crates/dgc-cli/{Cargo.toml,src/main.rs}`（`[[bin]] name = "dgc-next"`）
- Modify: `scripts/verify.py`（`test` と `rust` モードの cargo test / clippy に `--workspace`）
- Modify: `scripts/tests/test_development_checks.py`（コマンド期待値を更新）
- Modify: `.github/workflows/ci.yml`（`cargo test --locked` → `cargo test --locked --workspace`、clippy と msrv の cargo check に `--workspace`）
- Modify: `README.md`（「Current development focus」節を、オーケストレーター移行中である旨と設計書へのリンクに書き換える）

**Interfaces:**
- Produces: `dgc_protocol::PROTOCOL_VERSION: u32 = 1`（M1 Task 1 で中身を足す）。`dgc-next --version` が `dgc-next 0.1.0` を出す。
- `[workspace.dependencies]` に置くもの: `tokio`, `serde`, `serde_json`, `thiserror`, `tracing`, `tracing-subscriber`, `clap`, `uuid`, `libc`, `toml`, `tempfile`（バージョンはルートの既存指定と同一）。ルートパッケージ側は既存の書き方のままでよい。

- [ ] **Step 1: 検証スクリプトのテストを先に更新する**

`scripts/tests/test_development_checks.py` に追加:

```python
def test_rust_and_test_modes_cover_workspace(self):
    for mode, flt in (("rust", None), ("test", "x")):
        argvs = [argv for argv, _ in verify.commands(mode, flt, SCRIPTS.parent)]
        cargo_tests = [a for a in argvs if a[:2] == ["cargo", "test"]]
        self.assertTrue(cargo_tests and all("--workspace" in a for a in cargo_tests))
        clippy = [a for a in argvs if a[:2] == ["cargo", "clippy"]]
        self.assertTrue(all("--workspace" in a for a in clippy))
```

- [ ] **Step 2: 失敗を確認する**

Run: `python3 -m unittest discover -s scripts/tests -p 'test_development_checks.py'`
Expected: FAIL（`--workspace` が無い）

- [ ] **Step 3: workspace 化、4 crate の作成、verify.py と ci.yml の更新**

`crates/dgc-cli/src/main.rs` は clap の `#[command(name = "dgc-next", version)]` だけを持つ。lib crate は doc コメントと `PROTOCOL_VERSION` 以外空でよい。CI の変更が `scripts/check-agent-guidance.py` の CI 整合チェックに触れる場合はそのチェックの期待値も合わせる。

- [ ] **Step 4: lock 更新と依存確認**

Run: `cargo build --workspace && cargo tree -p dgc-daemon | rg -c "sea-orm|reqwest|tree-sitter" || true`
Expected: build 成功、カウント 0（出力なし）

- [ ] **Step 5: 旧バイナリと新バイナリの確認**

Run: `cargo run -q --bin dgc -- --version && cargo run -q --bin dgc-next -- --version`
Expected: `doge-code 0.1.0` と `dgc-next 0.1.0`

- [ ] **Step 6: 全検証**

Run: `cargo fmt --all && bash scripts/verify.sh guidance && bash scripts/verify.sh rust && bash scripts/verify.sh msrv`
Expected: すべて PASS（msrv は rustup に 1.94.0 が無ければ「環境で実行不可」と PROGRESS.md に記録）

- [ ] **Step 7: PROGRESS.md 更新とコミット**

PROGRESS.md の M0 を「完了」にし、検証記録に Step 6 の結果を書く。

```bash
git add Cargo.toml Cargo.lock crates scripts .github README.md docs/orchestrator/PROGRESS.md
git commit -m "build(workspace): add orchestrator crate scaffold"
```
