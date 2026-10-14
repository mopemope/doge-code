package acceptancefixture

func roundedDiv(bytes, block int64) int64 {
	q := bytes / block
	if bytes%block != 0 {
		q++
	}
	return q
}
