package acceptancefixture

func StableUnique(v []string) []string {
	if v == nil {
		return nil
	}
	out := make([]string, 0, len(v))
	seen := map[string]bool{}
	for _, s := range v {
		if !seen[s] {
			seen[s] = true
			out = append(out, s)
		}
	}
	return out
}
