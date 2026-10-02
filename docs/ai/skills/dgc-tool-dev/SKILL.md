---
name: dgc-tool-dev
description: Add or change doge-code (dgc) tool schemas, registration, dispatch, or output contracts; diagnose unknown or unreachable tools.
---

# dgc tool development

Read the affected tool, its callers, and the relevant section of the
[output contract](../../../../docs/tool-output-contract.md).

- Implement the entry point and `tool_def()` under `src/tools/`; wire the module,
  `default_tools_def`, dispatch arm, and handler. Runtime-discovered tools use the
  catalog route; document any deliberate exception in the routing exception file.
- Return `ToolOutput { value, is_success, result_summary }`. Bound output at its
  source; the JSON-safe global truncator is a fallback. Use pagination and
  `response_budget_chars` for large results. Read current caps from the contract.
- State-mutating tools return compact acknowledgements; explicit read tools
  supply full state. Use the [mutation contract](../../../../docs/ai/contracts.md)
  when the tool edits workspace text.
- Update the README tool list and add behavior/error tests beside the tool.
  Dispatch changes also need dispatch regression tests using isolated fixtures.

Run `python3 scripts/check-agent-guidance.py` for schema/dispatch/README parity,
then `bash scripts/verify.sh test <matching_filter>` for changed behavior.
The static parity check does not prove handler semantics or mutation tracking.
