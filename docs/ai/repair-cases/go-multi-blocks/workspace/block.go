package acceptancefixture

import "errors"

func Blocks(bytes, block int64) (int64, error) {
	if bytes < 0 || block < 0 {
		return 0, errors.New("invalid size")
	}
	return roundedDiv(bytes, block), nil
}
