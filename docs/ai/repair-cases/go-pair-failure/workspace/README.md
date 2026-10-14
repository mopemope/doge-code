Synthetic fixed acceptance fixture. No historical issue or production feature claim.

Repair ParsePair(string) (key, value string, err error) so it splits at the first equals sign, trims surrounding whitespace on key/value, rejects missing equals or an empty key, and permits an empty value or additional equals in the value. Change only pair.go; do not alter failing tests or build files.
Do not edit kept.go, tests or go.mod.
