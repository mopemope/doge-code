package acceptancefixture

import "errors"

func AddPositive(a, b int64) (int64, error) {
	if a < 0 || b < 0 {
		return 0, errors.New("negative operand")
	}
	return a + b, nil
}
