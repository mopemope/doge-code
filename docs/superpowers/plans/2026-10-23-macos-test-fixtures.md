# Portable macOS test fixtures implementation plan

> Execute natively with systematic-debugging and test-driven-development; request an independent whole-branch review before publication.

**Goal:** Remove five reproduced macOS fixture failures without weakening production safety assertions or changing runtime behavior.

**Architecture:** Keep real Git, filesystem and CLI paths. Represent unsupported filename bytes in the Git index, where they do not depend on filesystem encoding. Keep the long-path fixture within macOS path limits and seed the CLI cache in the platform-native location.

**Tech Stack:** Existing Rust tests, Git, tempfile; no dependencies or MSRV changes.

**Spec:** User authorized the highest-priority follow-up through PR creation. This slice fixes macOS fixtures only; provenance limits and shell lifecycle remain separate investigations.

## Constraints and acceptance

- Refresh develop, open PRs and CI before editing; preserve prior audit and branches.
- No real secrets, paid model calls, assertion weakening, ignored/skipped tests or production behavior changes.
- Both malformed filename bytes and actual U+FFFD paths must remain conservatively rejected by the real capture paths.
- Helper/textconv/fsmonitor must remain unexecuted; report/export must fail safely for unsupported paths.
- Read pagination must still exercise long escaped paths, continuation, JSON response limits, scope and symlink rejection; synthetic oversized metadata stays rejected.
- CLI must read owned offline cache without loading invalid configuration or leaking synthetic secret sentinels.
- Commit author/committer mopemope <yutaka.matsubara@gmail.com>; no Git configuration changes, develop push, force push or merge.

## Task 1: Preserve Git encoding coverage across filesystems

Files: src/test_support.rs, src/features/evidence_report/tests.rs, src/features/verification_snapshot/tests.rs.

- [x] Re-run the existing two tests and retain RED logs from macOS filename creation.
- [x] Add a Unix-only test helper inserting a regular blob with raw path bytes into the owned Git index via update-index; no invalid working-tree filename is required. Verify raw ls-files bytes contain the fixture path.
- [x] Replace only failing filesystem setup in both tests; preserve capture/report/export assertions. Cover actual U+FFFD filenames separately so lossy alias handling remains conservative.
- [x] Run both test modules, requiring nonzero executed tests.

## Task 2: Portable long path and native cache fixture

Files: src/tools/read_pagination_tests.rs, tests/model_catalog_cli.rs.

- [x] Retain RED logs for long-path fixture creation and two offline CLI failures.
- [x] Use three 190-character escaped directory components (over 600-byte absolute path, below macOS 1024-byte limit with an ordinary temp root). Keep all pagination/scope/metadata assertions.
- [x] On macOS seed HOME/Library/Caches/doge-code/openrouter/models-v1.json; on Linux keep XDG_CACHE_HOME/doge-code/openrouter/models-v1.json. Keep isolated env_clear HOME/cache/config and all CLI assertions.
- [x] Run the complete read-pagination and CLI catalog tests.

## Final verification and publication

- [x] Self-review: all five failures map to setup only, no unsupported-path safety checks removed; no runtime/API/dependency changes.
- [x] cargo fmt --all; TMPDIR=/private/tmp RUST_TEST_THREADS=4 CARGO_NET_OFFLINE=true bash scripts/verify.sh rust; guidance and native macOS focused checks where needed.
- [x] Independent reviewer checks diff, fixture equivalence and platform boundaries; fix material findings and re-run affected checks.
- [ ] Inspect effective push URL/refspecs/upstream/push.default/pushRemote without secrets; dry-run exact full feature refspec, inspect single destination, then identical real push. Verify remote feature head and unchanged develop.
- [ ] Create PR against develop using gh authenticated mopemope, verify and attach PR, monitor exact-head CI to completion; never merge.

## Plan self-review

No production code is needed. Existing tests already provide RED evidence and the behavioral regression gates. The Git helper has one responsibility and is cfg(test)/Unix only. The actual replacement-character path case adds a distinct existing safety contract rather than mirroring implementation. The long-path case still stresses escaped metadata and explicitly rejects oversized synthetic metadata independently of host path length. Cache path selection is confined to child-process fixtures; no global environment mutation. The user has authorized execution through PR, so proceed without another approval step.

## Validation/review checkpoint

All five original failures reproduced on current develop: two invalid filesystem names
(errno92), one overlong path (errno63), and two offline CLI cache lookups.
Focused after fixes: evidence report49, verification snapshot17, read pagination10,
CLI name-filter6 passed. Full native macOS before review correction:2382 unit and43
integration passed, no skipped/ignored tests. Guidance191 passed.
Independent reviewer found fixture Git could invoke configured fsmonitor. The added
marker assertion reproduced RED; Git fixture commands now disable fsmonitor and
untrackedCache, with assertions immediately after setup and after report/export.
The regression passed GREEN; reviewer rechecked and found no residual issues.
Final full Rust gate after that correction passed: fmt, warning-free Clippy,
2382 unit +43 integration tests, zero failures/ignored. Production code is unchanged.
