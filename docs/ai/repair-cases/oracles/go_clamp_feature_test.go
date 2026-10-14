package acceptancefixture

import "testing"

func TestFixedClamp(t *testing.T) {
	for _, c := range []struct {
		name               string
		v, low, high, want int64
		bad                bool
	}{
		{"inside", 2, 0, 4, 2, false}, {"below", -1, 0, 4, 0, false}, {"above", 6, 0, 4, 4, false},
		{"equal", 99, 5, 5, 5, false}, {"invalid", 0, 2, 1, 0, true},
		{"minimum", -9223372036854775808, -9223372036854775808, 9223372036854775807, -9223372036854775808, false},
		{"maximum", 9223372036854775807, -9223372036854775808, 9223372036854775807, 9223372036854775807, false},
	} {
		t.Run(c.name, func(t *testing.T) {
			v, e := Clamp(c.v, c.low, c.high)
			if (e != nil) != c.bad || (!c.bad && v != c.want) {
				t.Fatalf("got %d,%v; want %d bad=%v", v, e, c.want, c.bad)
			}
		})
	}
}
