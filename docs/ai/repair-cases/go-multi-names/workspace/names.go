package acceptancefixture

func Names(values []string) []string {
	if values == nil {
		return nil
	}
	out := []string{}
	for _, v := range values {
		out = append(out, NormalizeName(v))
	}
	return out
}
