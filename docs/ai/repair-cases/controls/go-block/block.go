package blockrounding

import "errors"

func Blocks(bytes, block int64) (int64, error) {
	if bytes < 0 || block <= 0 {
		return 0, errors.New("invalid size")
	}
	q := bytes / block
	if bytes%block != 0 {
		q++
	}
	return q, nil
}
