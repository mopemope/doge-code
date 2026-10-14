package acceptancefixture

import "errors"

func Clamp(v, low, high int64) (int64, error) {
	if low > high {
		return 0, errors.New("invalid bounds")
	}
	return v, nil
}
