package acceptancefixture

import "errors"

func Clamp(v, low, high int64) (int64, error) {
	if low > high {
		return 0, errors.New("invalid bounds")
	}
	if v < low {
		return low, nil
	}
	if v > high {
		return high, nil
	}
	return v, nil
}
