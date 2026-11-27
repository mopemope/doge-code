# Orchestrator M4: Profiles and Rate-limit Handoff Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Codex など superpowers が無い環境では、各タスクを上から順に実行し、完了ごとに `docs/orchestrator/PROGRESS.md` を更新してコミットする。

**Goal:** 名前付きの起動設定（プロファイル）でエージェントを作れるようにし、プロバイダが使用量上限に達したら別プロバイダ（例: Claude → Codex）への引き継ぎを提案して、ユーザーの確認後に同じワークスペースで新しいエージェントを起動できるようにする。あわせて、これらの前提になる M1/M2 の保留指摘を直す。

**Architecture:** プロファイルはデーモンの `config.toml` に置き、新しい RPC `profile.list` で公開する。上限検知は各アダプタで `EventKind::RateLimited` に正規化する。引き継ぎはデーモンの新しい RPC `agent.handoff` が機械的に引き継ぎ文を組み立てて新エージェントを作る（dgc は LLM を呼ばない）。CLI と TUI は提案と確認の UI を持つ。

**Tech Stack:** 既存の crates のみ（新しい依存なし）

**Spec:** `docs/superpowers/specs/2026-11-25-orchestrator-design.md` §10 M4（の前半）。ユーザー決定（2026-11-26）: M4 はプロファイル・引き継ぎ・前提修正、スケジュール／heartbeat／スクリプトは M5、エージェント向け MCP は M6。引き継ぎは「提案してユーザーが確認」（自動切替はしない）。

## Global Constraints

- 引き継ぎは自動で起動しない。必ずユーザーの明示操作（`dgc handoff` か TUI の確認）で起動する。
- 引き継ぎ文は LLM を呼ばず、元エージェントのタイトルと最初の指示、直近のユーザー指示（最大 3 件）、上限到達やエラーを含まない直近の回答の末尾（最大 4 KiB）、ワークスペースの `git status --short` と `git diff --stat`（各 4 KiB、各 30 秒まで）、`events.jsonl` の絶対パスから組み立てる。制御文字（`\n` と `\t` 以外）は除く。全体の上限は 32 KiB。最初の指示は UTF-8 の文字境界で先頭 16 KiB までとし、切った場合は `…(truncated; the full instruction is in the log below)` を付ける。前置き、タイトル、ログパスを保持し、全体の上限に合わせて回答、直近の指示、Git の出力の順に削る。
- TUI の起動要求は表示した引き継ぎ文を `expected_prompt` で渡す。デーモンがログと Git から再生成した内容に差があれば `INVALID_STATE`（`handoff_changed`）を返し、TUI が改めてプレビューと確認を行う。CLI の起動要求は `expected_prompt` を省略する。
- 引き継ぎ先のモードは、指定がなければ引き継ぎ先プロバイダの既定（自動承認なし）。自動承認系モードは明示指定のときだけで、CLI は従来どおり警告行を出す。
- プロファイルは `config.toml` の `[profiles.<name>]`（`provider`、`model`、`mode`、`notes` すべて文字列、provider 以外は任意）。未知のキーは起動エラー（既存の `deny_unknown_fields`）。プロファイル名は `[A-Za-z0-9_-]{1,32}`。
- 上限検知の条件（記録データとスキーマからの推定。実際の上限到達は未観測なので README に明記）:
  - Claude: `rate_limit_event` の `rate_limit_info.status` が `"rejected"` または未知の文字列（`"allowed"` と `"allowed_warning"` は無視）、または `result` / assistant エラー行の `api_error_status == 429`。
  - Codex: `willRetry` が false または省略された `error` 通知の `codexErrorInfo` が `usageLimitExceeded` か `rateLimitExceeded`、または `account/rateLimits/updated` の `rateLimitReachedType` が null 以外。
  - Codex の `resets_at` は `primary` / `secondary` のうち `usedPercent >= 100` のウィンドウにある最小の整数 `resetsAt`。該当するウィンドウがなければ両方を対象とする。
  - 同じターン内では 1 回だけ `RateLimited` を出す。`detail` は正規化した `{ "provider", "reason", "resets_at"? }`。`reason` は `[A-Za-z0-9_]` 以外を `_` に置換し、48 文字までとする。アカウント情報やプラン名は含めない。
- 新しい依存なし。既存のセキュリティ不変条件と TUI の権限安全規則（AGENTS.md）を弱めない。
- テストは偽プロバイダとシナリオ（上限到達のシナリオは合成と明記）で行う。

## Review Focus

1. 上限検知の誤検知: `allowed`、`allowed_warning`、Codex の `willRetry: true` のエラーや `rateLimitReachedType: null` で `RateLimited` を出さない（Task 2 にテスト）。
2. 引き継ぎ文に秘密が混ざる経路: アカウント情報・環境変数・認証ファイルは含めない（Task 4 にテスト。含めるのは元エージェントのイベントと git の出力だけ）。
3. 引き継ぎ中に元エージェントが動いている: 元エージェントは止めない（ユーザーが判断）。引き継ぎ先は同じワークスペースで並行に動きうることを README に書く。
4. TUI の提案表示が権限ダイアログや入力の安全規則を崩さない（提案は表示だけ、確認ダイアログは別、キーは `H` のみ）。
5. プロファイルの自動承認モードが、作成時の警告・一覧の `auto_approve`・TUI の `[auto]` に必ず反映される。

---

### Task 1: 前提修正（M1/M2 の保留指摘）

**Files:** `crates/dgc-protocol/src/methods.rs`、`crates/dgc-daemon/src/{events_log.rs,agent.rs,agent/actor.rs,handlers.rs}`、`crates/dgc-cli/src/tui/{state.rs,update.rs,backend.rs}`、`crates/dgc-cli/src/commands/agent.rs`。テストは各 crate の既存テストファイル。

**Interfaces（Produces）:**
- `AgentEventsParams` に `tail: Option<u32>` を追加（指定時は `since_seq` を無視して末尾 N 件を昇順で返す。上限は既存の clamp と 768 KiB の予算）。`EventLog::read_tail(n)` は回転ファイルを含め、ファイル全体を毎回読み直さずに末尾から読む（逆方向のブロック読み。壊れた行は従来どおり飛ばす）。
- TUI: エージェント選択時の履歴読み込みを「末尾 `STREAM_CAP` 件（tail）→ 以降はライブ」にする（M2 保留: 毎回 seq 0 から読む問題）。`logs` コマンドは従来どおり全件（`--tail N` を追加）。
- TUI: steer / interrupt で送信したら送信モードを queue に戻す（M2 保留: モードがエージェントをまたいで残る）。ステータスに戻したことを表示しない（入力欄のタイトルでモードは常に見える）。
- TUI: スナップショット要求を合流させる（M2 保留: 遅いデーモンで積み上がる）。定期要求の pending は失効させない。backend はキューにある `LoadSnapshot` を 1 つにまとめる。
- デーモン: 起動に失敗したエージェントの初期プロンプトを queue に残し、`agent.resume` で送る（M1 保留: 起動失敗で初期プロンプトが失われる）。

- [ ] **Step 1: テスト** — `read_tail_returns_last_n_across_rotation`、`read_tail_does_not_scan_whole_file`（巨大ログで読み取りバイト数が末尾付近に限られることを、計測用のフックかファイルサイズと時間の上限で確認）、`events_tail_param_returns_last_n`、`tui_selection_loads_tail_then_live`、`steer_or_interrupt_send_resets_mode_to_queue`、`periodic_snapshots_do_not_pile_up_behind_a_slow_daemon`、`failed_start_keeps_initial_prompt_for_resume`。
- [ ] **Step 2–4:** 失敗確認 → 実装 → 通過（`bash scripts/verify.sh test <名前>` か `cargo test --locked -p <crate> --test <file>`）
- [ ] **Step 5: コミット** `fix: tail history reads and other prerequisites for handoff`（PROGRESS の保留一覧の該当行に「M4 Task 1 で解消」）

### Task 2: 上限検知（アダプタ）

**Files:** `crates/dgc-daemon/src/provider/{claude.rs,codex.rs}`、`crates/dgc-daemon/tests/{claude_adapter.rs,codex_adapter.rs}`、合成シナリオ `crates/dgc-daemon/tests/fixtures/{claude,codex}/scenarios/rate_limited_*.jsonl`（ヘッダに合成と明記。形は fixtures README とスキーマから）。

**Interfaces（Produces）:** `EventKind::RateLimited { detail }` を Global Constraints の条件で 1 ターン 1 回出す。`detail = {"provider": "claude"|"codex", "reason": "<rate_limit_event status | usageLimitExceeded | rateLimitExceeded | <rateLimitReachedType> | http_429>", "resets_at": <unix秒, 任意>}`。fixtures README の「未観測」節を更新（検知条件と根拠）。

- [ ] **Step 1: テスト** — `claude_rate_limit_rejected_status_emits_rate_limited`、`claude_429_result_emits_rate_limited`、`claude_allowed_status_is_ignored`（既存の routine テストを維持）、`codex_usage_limit_error_emits_rate_limited`、`codex_rate_limit_reached_type_emits_rate_limited`、`codex_null_reached_type_is_ignored`、`rate_limited_is_emitted_once_per_turn`、`rate_limited_detail_carries_no_account_data`。
- [ ] **Step 2–4**、**Step 5: コミット** `feat(daemon): detect provider usage limits`

### Task 3: プロファイル

**Files:** `crates/dgc-daemon/src/config.rs`、`crates/dgc-protocol/src/methods.rs`（`profile.list`、`ProfileInfo { name, provider, model, mode, notes, auto_approve }`）、`crates/dgc-daemon/src/{handlers.rs,server.rs,lib.rs}`、`crates/dgc-cli/src/commands/{agent.rs,profile.rs}`、`crates/dgc-cli/src/tui/{state.rs,update.rs,render.rs}`、README。

**Interfaces（Produces）:**
- `DaemonConfig.profiles: BTreeMap<String, ProfileConfig>`。起動時に検証（名前の形式、provider が claude|codex、mode がそのアダプタで有効）。不正は起動エラー（理由つき）。
- `AgentCreateParams` に `profile: Option<String>` を追加。デーモンがプロファイルを展開し、明示された `provider`/`model`/`mode` が優先（provider をプロファイルと矛盾させたら INVALID_PARAMS）。CLI の `run` は `--provider` を `--profile` 指定時は省略可に。
- `dgc profile ls [--json]`（notes も表示、自動承認系は `auto-approve` と表示）。
- TUI の新規エージェントダイアログ: 先頭にプロファイル選択（Tab/Shift-Tab で「なし」と各プロファイルを循環）。選ぶと provider/model/mode 欄に反映（編集可）。自動承認モードの赤い警告は従来どおり。

- [ ] **Step 1: テスト** — `profiles_load_and_validate`、`invalid_profile_mode_fails_startup`、`profile_list_reports_auto_approve`、`create_with_profile_applies_fields_and_explicit_flags_win`、`profile_provider_conflict_is_invalid_params`、CLI `run_with_profile`、TUI `profile_selection_fills_form_and_warns_on_auto_approve`。
- [ ] **Step 2–4**、**Step 5: コミット** `feat: launch agents from named profiles`

### Task 4: 引き継ぎ（デーモン、CLI、TUI）

**Files:** `crates/dgc-protocol/src/methods.rs`（`agent.handoff`: `AgentHandoffParams { agent_id, provider: Option<ProviderKind>, profile: Option<String>, model, mode, dry_run: bool, expected_prompt: Option<String> }` → `{ prompt: String, agent: Option<Agent> }`）、`crates/dgc-daemon/src/handoff.rs`（新規: 引き継ぎ文の組み立て）、`crates/dgc-daemon/src/{handlers.rs,server.rs}`、`crates/dgc-cli/src/commands/handoff.rs`、`crates/dgc-cli/src/tui/{state.rs,update.rs,render.rs}`、README。

**Interfaces（Produces）:**
- 引き継ぎ文は Global Constraints の内容と上限。新エージェントは同じワークスペース、`title = "handoff: <元タイトル>"`、`labels = {"handoff_from": <元 id>}`、`parent_id` は使わない（M6 のサブエージェント用）。`provider` も `profile` も無ければ「元と別のプロバイダ」を既定にする。`dry_run` は文だけ返して作成しない。
- `dgc handoff ID [--to claude|codex] [--profile P] [--model M] [--mode MODE] [--dry-run] [--follow]`。自動承認系モードは警告行。
- 提案: `RateLimited` を受けたエージェントについて、CLI の `run --follow` / `logs --follow` は 1 行の提案（`rate limited by claude — hand off with: dgc handoff <id> --to codex`）を表示。TUI は該当エージェントの行に `⇄` 印、ストリームの上に 1 行の提案（`H: hand off to codex`）。`H`（Tree フォーカスのみ）で確認ダイアログ: 引き継ぎ先、モード（自動承認なら赤警告）、引き継ぎ文のプレビュー（`dry_run` の結果をエスケープ・折り返し表示、スクロール可）→ `Enter` で作成、`Esc` で取消。提案はエージェントが新しいターンを正常に完了したら消える。
- 権限ダイアログとの関係: 確認ダイアログが開いている間は権限ダイアログを自動で開かない（既存の「別のダイアログの上に開かない」規則）。ペーストは無視。

- [ ] **Step 1: テスト** — デーモン: `handoff_prompt_contains_instructions_answer_git_and_log_path`、`handoff_prompt_is_bounded_and_strips_control_chars`、`handoff_prompt_excludes_env_and_account_data`、`handoff_defaults_to_the_other_provider`、`handoff_dry_run_creates_nothing`、`handoff_creates_agent_in_same_workspace_with_label`。CLI: `handoff_dry_run_prints_prompt`、`follow_prints_handoff_suggestion_on_rate_limit`。TUI: `rate_limited_agent_shows_marker_and_suggestion`、`h_opens_confirmation_with_preview_and_enter_creates`、`suggestion_clears_after_a_completed_turn`、`paste_and_tree_keys_do_not_confirm_handoff`。
- [ ] **Step 2–4**、**Step 5: コミット** `feat: suggest and run a handoff to another provider`

### Task 5: M4 受け入れ

- [ ] **Step 1:** 偽プロバイダの合成シナリオで: 上限到達 → CLI と TUI に提案 → `dgc handoff --dry-run` の文を確認 → TUI の `H` で確認して作成 → 新エージェントが同じワークスペースで動く。プロファイル（通常と自動承認）での作成と警告。画面を `docs/orchestrator/m4-tui-screen.txt` に記録（tmux）。
- [ ] **Step 2:** 実プロバイダで 1 回だけ: 通常の Claude（haiku）エージェントで短い作業 → `dgc handoff <id> --to codex`（Codex の安いモデル、自動承認なし）→ Codex が引き継ぎ文を受けて 1 ターン完了。上限到達は起こさない（検知は合成シナリオでのみ確認したと記録）。
- [ ] **Step 3:** 全検証（固定ツールチェーン 1.99.0）: fmt、`verify.sh rust`、guidance、msrv、tui-deps。README（プロファイル、引き継ぎ、検知条件と未観測の注意）と PROGRESS を更新。
- [ ] **Step 4: コミット** `docs(orchestrator): complete M4 acceptance`、PROGRESS の M4 を完了、「現在の作業」を M5（スケジュール、heartbeat、ワークスペーススクリプト）の計画作成に。
