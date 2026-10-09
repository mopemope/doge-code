# Agent instruction audit (2026-10-09)

Base: develop `06c7aaf371b6f12238d6b5b9084ba0bf462dbcc1`, PR #108 merged
2026-10-09T00:40:57Z. Scope: model-neutral instruction corrections and advisory
context projection; no provider settings, permission policy, storage format,
tool schema, planning gate, or executor concurrency changes.

## Actual instruction path

| Source | Runtime behavior and priority |
|---|---|
| `resources/system_prompt.md` | Embedded authoritative template; `tui/commands/prompt.rs::build_system_prompt` renders it for CLI/TUI/main loop. The legacy `llm::SYSTEM_PROMPT` exports the same raw template. |
| Project instruction file | Configured path first (existing cwd-relative/absolute path, then project-relative); otherwise root `AGENTS.md`, `QWEN.md`, `GEMINI.md` in order. Configured missing/unreadable files do not select another file. Appended project procedures stay within base constraints/permissions; explicit user instructions override workflow preferences. |
| Nested instructions / skills | Not recursively loaded or automatically activated by dgc. Selected project instructions and explicit user requests can route to relevant files, read on demand. Host skill symlinks support other agents' discovery; they do not imply dgc has read the bodies. |
| Tool definitions | Trusted built-in descriptions plus configured MCP catalog, activated on demand. Tool result content is evidence, not authority. External MCP descriptions still rely on the user's configured server trust and existing permission controls. |
| Runtime files/memory | `RuntimeContextBuilder` fetches recent file/symbol metadata and matching memory keys, not full memory bodies. First-request-only overlay; no durable history mutation. |
| TUI shell/plan | `agent_job.rs` / `core.rs` supply recent shell output / unfinished saved-plan summary. Canonical bootstrap messages are dropped by the existing system-message durable projection. The wire projection lowers only explicit advisory envelopes to user role. |
| Loop/stall/budget hints | Generated policy remains scoped to authorized work; a warning grants no new permission. Budget finalization requests a tools-free partial report, not a success claim. Cancellation and resource limits remain runtime-controlled. |
| Research worker | `tools/task.rs::subagent_system_prompt` is a separate prompt, not the full main prompt. Tool allowlist enforces read-only/no-process capability. The main agent supplies task constraints; the worker now receives trust, evidence, dependent-call, and stop guidance. |

## Concrete causes and corrections

1. File/symbol names and memory keys were interpolated as raw system context;
   TUI plan text and shell logs were also mixed into system messages. Those
   strings can contain instruction-like text and closing markup. They now form
   escaped JSON payloads with an explicit data-only envelope and user role on
   provider requests. Authority messages, tool blocks, canonical session state,
   and activation markers stay intact. Serialized runtime strings are budgeted
   after escaping, including control characters. History compaction truncates
   advisory payloads through the JSON-safe budget helper instead of slicing
   serialized strings. Missing context keeps the
   existing borrow fast path.
2. The prompt said errors contain the solution and always instructed retries.
   It now treats errors as diagnostic evidence, requires a reason for a changed
   attempt, and prohibits bypassing permission denial through another tool.
   Patch mismatch can reflect concurrent changes or an incorrect patch, not
   simply failure to read recently. Persistent shells remain conditional.
3. The generic verification instruction required builds even for read-only
   questions and did not distinguish empty test selections from useful checks.
   It now scales checks to changed behavior and project requirements, records
   passed/failed/blocked/unrun outcomes, and reuses evidence while its inputs are
   unchanged. Write success or a recommendation is not verification success.
4. A batch is executed sequentially, but the model chooses all arguments before
   observing intermediate results. Guidance now batches only independent
   operations: read then choose an edit, mutation then verification selection,
   and discovery then a discovered tool belong in separate iterations. No new
   parallel executor or scheduler was added.
5. Worker instructions lacked the main trust and honest-reporting boundaries.
   They now distinguish facts, inference, unsupported/truncated evidence, and
   inability to execute tests; stop once the bounded question is answered or
   concretely blocked. Delegation remains optional. Stall warnings no longer
   suggest unrequested edits merely to manufacture progress.

## Retained behavior and limits

Small localized edits, short questions, and focused read-only checks still need
no plan or requirements ledger. Complex dependent work and explicit planning
requests retain #108's contextual requirement. Saved-plan content supplies no
authorization; completed/corrupt-plan behavior and pending old-step handling
remain as documented in [plan policy](plan-policy.md). #107's schema repair and
model-compatible wire behavior remain unchanged.

The selected instruction source is intentionally project guidance, not an
arbitrary retrieved file. There is no new hierarchy resolver, nested-file loader,
skill discovery engine, mandatory approval for every read, browser requirement,
or automated safety classifier. Prompt instructions and JSON quoting are partial
mitigations: they cannot guarantee resistance to malicious data or compliance by
every model. Existing executor validation and permissions are still necessary.

Deterministic regressions cover payload round-trip with hostile markup/newlines,
escaped-budget limits, lower wire roles, canonical/durable separation, tool-call
pairing, first-request-only behavior, root/config discovery, and main/worker
policy. Compaction regressions cover long hostile shell/plan data remaining
parseable and lower-privilege after pruning. HTTP fixtures also cover a tools-free request. The full suite checks
existing cancellation, context failures, deferred activation and session recovery.
Localhost mock TUI checks wiring rather than real-model judgment. No paid/live
model evaluation, prompt-injection resistance rate, or comparative token savings
was measured. The embedded base template grew from 9,689 to 12,470 characters
(+2,781), before project guidance/variable rendering; this is a context cost, not
a measured token count. Improved model choices remain an inference requiring representative
multi-model runs under [the evaluation protocol](evaluation.md).

## Official design references

- [Anthropic: Effective context engineering (2025-09-29)](https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents): focused context, progressive retrieval, clear instructions; avoid unconditional ceremony.
- [OpenAI: Safety in building agents](https://developers.openai.com/api/docs/guides/agent-builder-safety): keep untrusted content out of high-priority instructions and constrain data flow. Agent Builder-specific universal MCP approval or model recommendations were not copied into dgc.
- [OpenAI: Using Goals in Codex (2026-05-09)](https://developers.openai.com/cookbook/examples/codex/using_goals_in_codex): completion depends on observable outcomes and explicit limits. This does not add a persistent Goals feature to dgc.
- [Anthropic: Harness design (2026-03-24)](https://www.anthropic.com/engineering/harness-design-long-running-apps): assess scaffolding against actual task/model capability; a particular model's results do not establish dgc performance.
- [OpenAI: Rethinking skills and prompts for GPT-6 Astra (2026-09-11)](https://developers.openai.com/blog/rethinking-skills-and-prompts-for-gpt-6-astra): parent investigation supplied the model-specific findings. They motivate reconsidering broad triggers; no Astra-specific prompt, API field, or fixed tool budget was adopted.

AGENTS.md and the relevant context/tool/verification skills already use scoped
reads and proportional checks; no formal rewrite of those files was necessary.
