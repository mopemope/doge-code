package acceptancefixture

import "testing"

func TestKept(t *testing.T) {
	if Kept(42) != 42 {
		t.Fatal("unrelated behavior changed")
	}
}
