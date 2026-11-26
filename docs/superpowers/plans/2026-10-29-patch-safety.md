# Patch Safety Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 調査で確認したP1のパッチ誤適用2件を修正する。
**Architecture:** 既存のexact→fuzzy適用と共有mutation commitを維持する。fuzzy照合の根拠を必須とし、旧座標に実際の適用位置のずれと増減を反映する。
**Tech Stack:** Rust 2024 / diffy 0.5.2 / Tokio。
**Spec:** [調査結果](../../ai/audit-2026-10-29.md)の問題1・2。ユーザーが優先度順の修正を指示済み。

## Constraints and choices

- まずP1の2件を独立してレビュー可能な変更にする。中・低優先度4件は別変更。
- 全fuzzy機能を削除する案より、照合根拠と座標系の修正を選ぶ。既存の文脈ずれへの対応を残せるため。
- 新ライブラリやmutation/provenance形式の変更は不要。
- 拒否したパッチはファイルを変更せずreceiptも返さない。
- 原初からcontextなしの挿入、空ファイル、末尾への挿入は維持する。
- 作業は現在のcheckoutで行う。既存の監査メモを保持し、コミットは行わない。

## Review focus

- 追加と削除の両方で先行hunkの増減を二重加算しない。
- leading contextを削った位置と、先行hunkの実際の位置ずれを正しく伝播する。
- 通常fuzzとaggressive fuzzの双方で文脈を全部失った挿入を拒否する。
- 複数hunkの途中失敗で、先行hunkだけをcommitしない。
- 正当なcontextなし挿入と曖昧な照合の拒否を保つ。

## Task 1: Patch safety

Files: `src/tools/apply_patch.rs`（実装とテスト）、`README.md`（利用者向け挙動）。
Interfaces: `apply_patch_with_recovery_and_receipt`と2つのfuzz entry pointの型は不変。

- [x] 回帰テストを追加し `bash scripts/verify.sh test fuzzy_safety` でRED確認（7件中6件が期待した理由で失敗）。
- [x] 照合根拠・旧座標・leading trim・位置ずれを修正。同コマンドでGREEN確認（7件成功）。
- [x] `bash scripts/verify.sh test tools::apply_patch::`（38件）と `tools::mutation::`（19件）で周辺確認。
- [x] README更新。`cargo fmt --all`、`bash scripts/verify.sh rust`、`bash scripts/verify.sh guidance` を実行（全体テストの環境依存失敗は下記）。
- [x] diffの独立レビューと検証結果を記録する。

## Baseline

前ターンの全体テスト: 2385成功・2失敗。失敗は `utils::tests::test_get_git_repository_root_without_git` と `utils::tests::test_is_git_repository_without_git_dir`。この環境の `/tmp/.git` に依存する既存失敗で、P1修正とは別に報告する。

## Validation log

- RED: `/tmp/dgc-verify-9tsn7vmj/01-test.log`
- GREEN: `/tmp/dgc-verify-2s8diq57/01-test.log`
- パッチ周辺: `/tmp/dgc-verify-rcudpmj6/01-test.log`
- mutation周辺: `/tmp/dgc-verify-8f7u2gd1/01-test.log`
- guidance: `/tmp/dgc-verify-0_92cdfv/`（チェック3種とPython 191件が成功）
- Rust全体チェック: `/tmp/dgc-verify-v4ixn0m1/`。fmt / warning-free Clippy成功。全体テストは2393成功・既存のGit判定2件が失敗。
- 独立レビュー: `review_patch_safety`（読み取り専用、実装修正必須の指摘なし）。計画の曖昧候補ケースを補強する提案に従い8件目の回帰テストを追加。最終全体テストで8件すべて成功。
- レビュー用agentは1件。総トークン使用量はツールから取得できず未計測。
- サンドボックス外の全体テストも同じGit判定2件が失敗: `/tmp/dgc-patch-safety-full-unsandboxed.log`。
- `/tmp/.git` を変更せず、Cargoの `env.TMPDIR.value="/var/tmp"` と `env.TMPDIR.force=true` で一時ファイル先を変更するとGit判定2件は成功。
- `/var/tmp` での並列全体テストは2393成功・別の2件が失敗: `features::openai_subscription::tests::browser_launch_failure_and_timeout_allow_manual_sign_in`（Unavailable / Failedの不一致）、`logging::tests::retention_bounds_reserve_active_runs_and_preserve_unmanaged_files`（drop後のopenが失敗）。両者は同じ設定での単独実行で成功。原因は確定していない。
- 全体を同設定＋`-- --test-threads=1` で再検証し、全2438件成功（2395 + 2 + 1 + 8 + 5 + 27）。ログ `/tmp/dgc-patch-safety-serial.log`。

最終コマンド:

```sh
cargo test --locked --config 'env.TMPDIR.value="/var/tmp"' --config 'env.TMPDIR.force=true' -- --test-threads=1
```

これは通常の `/tmp`・並列実行における失敗を解決したという意味ではない。既存のGit fixture依存と並列実行時の2件は残課題として保持し、今回のP1変更に混ぜていない。中・低優先度の監査4件も未変更。

## PR作成（追加のユーザー指示）

- ユーザーの「レビューして、指摘事項を修正したらPR作成して」により、コミット・push・PR作成を実施。
- 新しい独立レビュー `pr_review` で修正必須指摘なし。パッチ関連39件も再実行で成功。
- コミット `ea7e05c`、ブランチ `fix/fuzzy-patch-safety`、base `develop`。
- [PR #182](https://github.com/mopemope/doge-code/pull/182)。READMEと実装・テストの2ファイルのみを提出。
- 調査・作業メモはローカルに保持し、PRには含めていない。
