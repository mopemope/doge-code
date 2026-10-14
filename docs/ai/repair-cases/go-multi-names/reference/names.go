package acceptancefixture

func Names(values []string) []string {
	if values == nil {
		return nil
	}
	out := []string{}
	seen := map[string]bool{}
	for _, v := range values {
		n := NormalizeName(v)
		if n != "" && !seen[n] {
			seen[n] = true
			out = append(out, n)
		}
	}
	return out
}
