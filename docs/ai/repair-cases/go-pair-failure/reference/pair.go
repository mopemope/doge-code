package acceptancefixture

import (
	"errors"
	"strings"
)

func ParsePair(s string) (string, string, error) {
	key, value, ok := strings.Cut(s, "=")
	key = strings.TrimSpace(key)
	value = strings.TrimSpace(value)
	if !ok || key == "" {
		return "", "", errors.New("invalid pair")
	}
	return key, value, nil
}
