# Synthetic Go repair task

Fix `Blocks`: it must return ceil(bytes / block) for every nonnegative int64 byte
count and positive int64 block size, without integer overflow. Preserve invalid
input errors and existing tests. Keep changes in block.go. This is a synthetic
fixture, not an issue or patch from a historical project.
