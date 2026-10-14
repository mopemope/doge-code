package acceptancefixture

import (
	"reflect"
	"testing"
)

func TestFixedNormalize(t *testing.T) {
	for _, c := range []struct{ in, want string }{{"  A\t", "a"}, {" É ", "é"}, {" \n\t", ""}, {"", ""}} {
		t.Run(c.in, func(t *testing.T) {
			if got := NormalizeName(c.in); got != c.want {
				t.Fatalf("got %q want %q", got, c.want)
			}
		})
	}
}
func TestFixedNames(t *testing.T) {
	for _, c := range []struct {
		name     string
		in, want []string
	}{{"nil", nil, nil}, {"stable", []string{" B ", "a", "b"}, []string{"b", "a"}}, {"blanks", []string{" ", "A", " a ", "", " B\t"}, []string{"a", "b"}}, {"unicode", []string{"É", " é", "界"}, []string{"é", "界"}}} {
		t.Run(c.name, func(t *testing.T) {
			before := append([]string(nil), c.in...)
			if got := Names(c.in); !reflect.DeepEqual(got, c.want) {
				t.Fatalf("got %q want %q", got, c.want)
			}
			for i, s := range before {
				if c.in[i] != s {
					t.Fatal("input changed")
				}
			}
		})
	}
}
