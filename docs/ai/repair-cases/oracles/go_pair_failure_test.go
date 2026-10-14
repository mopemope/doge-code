package acceptancefixture

import "testing"

func TestFixedPair(t *testing.T) {
	for _, c := range []struct {
		in, key, value string
		bad            bool
	}{{"a=b", "a", "b", false}, {" a = b=c ", "a", "b=c", false}, {"a=", "a", "", false}, {"=b", "", "", true}, {"abc", "", "", true}, {"=", "", "", true}} {
		t.Run(c.in, func(t *testing.T) {
			k, v, e := ParsePair(c.in)
			if (e != nil) != c.bad || (!c.bad && (k != c.key || v != c.value)) {
				t.Fatalf("got %q,%q,%v want %q,%q bad=%v", k, v, e, c.key, c.value, c.bad)
			}
		})
	}
}
