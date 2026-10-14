Synthetic fixed acceptance fixture. No historical issue or production feature claim.

Fix Blocks and roundedDiv across block.go and division.go. The public wrapper rejects negative bytes and nonpositive block sizes. The helper, called only with valid inputs, computes ceil(bytes/block) without overflow for all nonnegative int64 values. Both direct helper and public caller behavior are tested. Preserve kept.go and tests.
Do not edit kept.go, tests or go.mod.
