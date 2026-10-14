package acceptancefixture

import (
	"errors"
	"strings"
)

func ParsePair(s string) (string, string, error) {
	p := strings.Split(s, "=")
	if len(p) != 2 {
		return "", "", errors.New("invalid pair")
	}
	return p[0], p[1], nil
}
