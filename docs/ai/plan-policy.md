# Execution plan policy

`plan_write` is a durable work-tracking tool, not a prerequisite for every
request or a source of authorization. Planning is required by agent guidance
for complex work and explicit user requests for a plan. There is no universal
runtime gate based on whether a plan file exists.

## Current implementation

- `resources/system_prompt.md` is the authoritative runtime prompt, rendered by
  `src/tui/commands/prompt.rs` for CLI, TUI, and the agent loop. The legacy raw
  `SYSTEM_PROMPT` export uses the same resource.
- `ToolCatalog` exposes `plan_write` and `plan_read` through deferred discovery
  when appropriate. Being registered does not mean a tool must be invoked.
  `tool_search` activation becomes callable on the next iteration.
- `src/tools/plan.rs` requires a nonempty `items` array when writing, unique ids,
  valid parent relationships/statuses, and at most one `in_progress` item.
  These are call-validity constraints, not task admission rules. There is no
  minimum of three steps. `strict: false` preserves optional fields/defaults
  and the HTTP 400 fix from PR #107.
- Plans are session-scoped JSON under `<project>/.doge/plans/<session-id>.json`,
  with a legacy read adapter. `replace` replaces the snapshot; `merge` replaces
  supplied items by id, so preserve fields and links on updated items. A
  successful write returns compact deltas; `plan_read` retrieves full state.
- `FsTools::plan_write_with_attribution` validates requirement links and emits
  plan transitions. Mutation provenance can record an observed directive with
  no plan; a current `in_progress` item adds plan/requirement attribution.
  A plan is therefore valuable traceability, not the safety mechanism that
  makes every write authorized. Completion warnings/verification obligations
  are evidence to inspect, not proof of correctness.
- TUI turn setup projects active saved plans as request-scoped context. It
  asks the agent to reconcile relevance with the current request, without claiming permission
  to implement. Empty/completed plans do not force creation on the next turn.
  Read failures are reported without asking for a replacement. Completed plans
  remain stored for explicit reads; startup/session switching still publish
  saved state. Successful planning tool calls refresh the UI from the store.
- Conversation checkpoints/cancellation and tool-call pairing work without a
  plan. Resume restores conversation and deferred activations; a relevant saved
  plan helps regain outstanding steps. `/quick` skips saved-plan projection,
  while the shared scope policy and explicit user instructions still apply.
  Existing provenance resolves the stored `in_progress` item without checking
  its relevance to the new directive, including `/quick`. Before editing for
  unrelated new work, move an unrelated active step back to `pending`
  (preserving its fields/links, not falsely completing it), or use a new
  session. Relevance is agent judgment, not an automatic attribution filter.
  `/plan show` displays saved state; it does not create a plan.

## Choice and tradeoffs

| Policy | Benefit | Cost / limitation | Decision |
|---|---|---|---|
| Always require a plan call | Uniform task lists | Adds discovery, writes and status calls to simple questions/fixes; invents steps; existence alone cannot establish relevance or safety | Do not use |
| Require planning for complex work or explicit requests | Tracks dependent milestones, uncertainty, verification, interruption and resumption | Complexity is contextual; prompts guide it, rather than brittle language/step-count heuristics | Use |
| Make planning entirely optional | Lowest imposed workflow cost | May lose task/verification visibility on long work or fail explicit user expectations | Allow only where the task does not need it |

A complex task has multiple substantive dependent steps, broad/uncertain scope,
or a realistic need to resume. The ordinary read/edit/verify sequence for one
localized fix is insufficient by itself. A short answer or focused read-only
check can proceed directly; a substantial read-only investigation may still
benefit from a plan. Scope first with read-only tools, then record the meaningful
steps before implementation. Update only when scope or status changes, preserve
stable ids/relevant links, and never mark unperformed work complete.

The system guidance applies equally across models and both request transports.
It does not add model-specific schema flags, classify task complexity by model,
force a planning tool through `tool_choice`, or remove the Luna reasoning-none
boundary. Model compliance remains an empirical limitation: tests validate
request projection and state behavior, not live model obedience. No live model
comparison was run and no token savings are claimed. Eliminating unconditional
planning removes avoidable call/state overhead in principle; its magnitude
requires matched evaluation.

## Checks

Core context regressions cover missing, active, completed, and corrupt plans,
including byte preservation, request-only context, and UI projection. Rendered
prompt tests retain small-task exceptions and the complex-work policy. Existing
plan/provenance/session/tool-activation and HTTP schema regressions remain
required. Use local mock interactions, `verify.sh rust`, `guidance`, and
`tui-deps`, plus a terminal recording for the visible TUI behavior.
