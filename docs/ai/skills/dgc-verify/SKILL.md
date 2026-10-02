---
name: dgc-verify
description: Select and run verification for doge-code (dgc) changes, or classify local and CI check failures.
---

# dgc verification

Choose checks from the actual change, using [the verification matrix](../../../../docs/ai/workflow.md).

- Start with `bash scripts/verify.sh test <module_or_name>`. Zero executed tests
  is an error. Extend scope for shared callers and boundary risks.
- Before finishing Rust changes, run `cargo fmt --all` and
  `bash scripts/verify.sh rust`: fmt check, locked Clippy with `-D warnings`, and
  the full locked test suite. Reuse passing checks while relevant inputs are unchanged.
- For docs, Skills, or development scripts alone, run
  `bash scripts/verify.sh guidance`; no Rust rebuild is required.
- Dependency/MSRV changes: `bash scripts/verify.sh msrv`. It reads the MSRV from
  Cargo.toml and requires the toolchain to be installed; report a missing toolchain
  as an environment blocker. TUI/dependency changes: `bash scripts/verify.sh tui-deps`.
- On macOS, `bash scripts/verify.sh macos` runs the CI-focused TUI, execution, and
  jobs checks. A Linux run cannot substitute for a macOS result.
- TUI changes also need manual verification with
  `cargo run --release -- <flags>` and a screenshot or terminal recording.

CI configuration is authoritative: `.github/workflows/ci.yml`. The wrapper keeps
full command logs in a temporary directory and emits short summaries. Preserve
failure exit codes; distinguish failed tests from compile/startup/environment
failures. Report skipped or blocked checks explicitly.
