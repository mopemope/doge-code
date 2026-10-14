package acceptancefixture

import "testing"

func TestFixedAdd(t *testing.T) {
	for _, c := range []struct {
		name       string
		a, b, want int64
		bad        bool
	}{{"zero", 0, 0, 0, false}, {"ordinary", 1, 2, 3, false}, {"max_zero", 9223372036854775807, 0, 9223372036854775807, false}, {"max_exact", 9223372036854775806, 1, 9223372036854775807, false}, {"overflow", 9223372036854775807, 1, 0, true}, {"negative_a", -1, 1, 0, true}, {"negative_b", 1, -1, 0, true}} {
		t.Run(c.name, func(t *testing.T) {
			got, e := AddPositive(c.a, c.b)
			if (e != nil) != c.bad || (!c.bad && got != c.want) {
				t.Fatalf("got %d,%v want %d bad=%v", got, e, c.want, c.bad)
			}
		})
	}
}
