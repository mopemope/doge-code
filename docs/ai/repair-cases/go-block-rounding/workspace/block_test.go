package blockrounding

import "testing"

func TestOrdinaryBlocks(t *testing.T) {
	for _, c := range [][3]int64{{0, 4, 0}, {1, 4, 1}, {8, 4, 2}, {9, 4, 3}} {
		got, err := Blocks(c[0], c[1])
		if err != nil || got != c[2] {
			t.Fatalf("Blocks(%d, %d) = %d, %v; want %d", c[0], c[1], got, err, c[2])
		}
	}
}
