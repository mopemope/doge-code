package acceptancefixture

import "strings"

func TrimSuffixOnce(value, suffix string) string { return strings.TrimRight(value, suffix) }
