package humanize

import (
	"math"
	"testing"
)

// Expected strings are fixed contract examples, not a reference implementation.
func TestRepairAcceptanceFtoaLimits(t *testing.T) {
	max := int(^uint(0) >> 1)
	min := -max - 1
	cases := []struct {
		name   string
		value  float64
		digits int
		want   string
	}{
		{"ordinary_positive", 1.25, 2, "1.25"},
		{"ordinary_negative", -1.25, 2, "-1.25"},
		{"one_positive", 1.25, 1, "1.2"},
		{"one_negative", -1.25, 1, "-1.2"},
		{"zero_positive", 1.25, 0, "1"},
		{"zero_negative", -1.25, 0, "-1"},
		{"negative_digits_positive", 1.25, -1, "1"},
		{"negative_digits_negative", -1.25, -1, "-1"},
		{"minimum_digits_positive", 1.25, min, "1"},
		{"minimum_digits_negative", -1.25, min, "-1"},
		{"maximum_digits_positive", 1.25, max, "1.25"},
		{"maximum_digits_negative", -1.25, max, "-1.25"},
		{"near_maximum_positive", 1.25, max - 1, "1.25"},
		{"near_maximum_negative", -1.25, max - 2, "-1.25"},
		{"large_digits_positive", 1.25, 1000000, "1.25"},
		{"large_digits_negative", -1.25, 1000000, "-1.25"},
		{"positive_zero", 0, max, "0"},
		{"negative_zero", math.Copysign(0, -1), max, "-0"},
		{"integer_positive", 42, max, "42"},
		{"integer_negative", -42, max, "-42"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			defer func() {
				if r := recover(); r != nil {
					t.Fatalf("FtoaWithDigits(%v,%d) panicked: %v", c.value, c.digits, r)
				}
			}()
			if got := FtoaWithDigits(c.value, c.digits); got != c.want {
				t.Fatalf("FtoaWithDigits(%v,%d)=%q; want %q", c.value, c.digits, got, c.want)
			}
		})
	}
}
