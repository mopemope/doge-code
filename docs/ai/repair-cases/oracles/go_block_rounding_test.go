package blockrounding

import (
	"math"
	"testing"
)

func TestRepairAcceptance(t *testing.T) {
	cases := []struct {
		name               string
		bytes, block, want int64
		invalid            bool
	}{
		{"zero", 0, 4, 0, false},
		{"ordinary", 9, 4, 3, false},
		{"exact", 8, 4, 2, false},
		{"max_unit", math.MaxInt64, 1, math.MaxInt64, false},
		{"max_even", math.MaxInt64, 2, math.MaxInt64/2 + 1, false},
		{"max_equal", math.MaxInt64, math.MaxInt64, 1, false},
		{"large_block", math.MaxInt64 - 1, math.MaxInt64, 1, false},
		{"negative", -1, 2, 0, true},
		{"zero_block", 1, 0, 0, true},
		{"negative_block", 1, -1, 0, true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got, err := Blocks(c.bytes, c.block)
			if (err != nil) != c.invalid || (!c.invalid && got != c.want) {
				t.Fatalf("Blocks(%d,%d)=%d,%v; want %d invalid=%t", c.bytes, c.block, got, err, c.want, c.invalid)
			}
		})
	}
}
