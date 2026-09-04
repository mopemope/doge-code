---
name: dgc-verify
description: Use when verifying changes in this Rust repo (dgc) before finishing or committing - running tests, clippy, fmt, CI merge gates, or manually checking TUI changes.
---

# dgc Verification Ladder

Run the narrowest check first, then broaden. Full `cargo test` (~390 tests) is
slow — only run it before finishing.

## Ladder

1. **Targeted tests**: `cargo test <module_or_name>` (e.g. `cargo test dispatch`)
2. **Clippy (merge gate)**: `cargo clippy --all-targets --all-features` — must be zero warnings (CI enforces `-D warnings`)
3. **Format (merge gate)**: `cargo fmt --all` (CI runs `--check`)
4. **Full suite before finishing**: `cargo test`

TUI changes: additionally verify manually with `cargo run --release -- <flags>`.

## CI gates (must stay green)

`.github/workflows/ci.yml` runs on push/PR:
- `cargo fmt --all --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`

## Notes

- Docs-only changes still trigger CI; the full pipeline is cheap to run when no Rust code changed (`cargo fmt --check` + clippy cache hit).
- Tests must use `tempdir()`, never write to the repo root or real CWD.
- PRs should list the verification performed; TUI changes need a screenshot or recording.
