package acceptancefixture

import "testing"

func TestFixedBlocks(t *testing.T) {
	for _, c := range []struct {
		name               string
		bytes, block, want int64
		bad, helper        bool
	}{
		{"ordinary", 4, 3, 2, false, false}, {"zero", 0, 1, 0, false, false}, {"negative", -1, 1, 0, true, false}, {"zero_block", 1, 0, 0, true, false},
		{"max_one", 9223372036854775807, 1, 9223372036854775807, false, false},
		{"max_two", 9223372036854775807, 2, 4611686018427387904, false, false}, {"max_max", 9223372036854775807, 9223372036854775807, 1, false, false},
		{"helper_ordinary", 4, 3, 2, false, true}, {"helper_max_two", 9223372036854775807, 2, 4611686018427387904, false, true},
		{"helper_max_max", 9223372036854775807, 9223372036854775807, 1, false, true},
	} {
		t.Run(c.name, func(t *testing.T) {
			defer func() {
				if r := recover(); r != nil {
					t.Fatalf("panic: %v", r)
				}
			}()
			if c.helper {
				if got := roundedDiv(c.bytes, c.block); got != c.want {
					t.Fatalf("helper got %d want %d", got, c.want)
				}
				return
			}
			got, e := Blocks(c.bytes, c.block)
			if (e != nil) != c.bad || (!c.bad && got != c.want) {
				t.Fatalf("got %d,%v want %d bad=%v", got, e, c.want, c.bad)
			}
		})
	}
}
