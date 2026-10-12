package blockrounding

import "errors"

// Blocks returns the number of blocks needed for a nonnegative byte count.
func Blocks(bytes, block int64) (int64, error) {
	if bytes < 0 || block <= 0 {
		return 0, errors.New("invalid size")
	}
	return (bytes + block - 1) / block, nil
}
