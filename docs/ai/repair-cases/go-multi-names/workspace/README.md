Synthetic fixed acceptance fixture. No historical issue or production feature claim.

Fix NormalizeName in normalize.go and Names in names.go. NormalizeName trims surrounding Unicode whitespace and lowercases text. Names uses that helper, drops blank names, keeps the first normalized occurrence in order, returns nil for nil input, and does not modify input. Both helper and caller behavior are tested; keep other files unchanged.
Do not edit kept.go, tests or go.mod.
