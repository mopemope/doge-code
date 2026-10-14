package acceptancefixture

import (
	"reflect"
	"testing"
)

func TestFixedUnique(t *testing.T) {
	for _, c := range []struct {
		name        string
		input, want []string
	}{
		{"nil", nil, nil}, {"distinct", []string{"a", "b"}, []string{"a", "b"}},
		{"stable", []string{"b", "a", "b", "c", "a"}, []string{"b", "a", "c"}},
		{"case", []string{"A", "a", "A"}, []string{"A", "a"}},
		{"empty_unicode", []string{"", "界", "", "界", "é"}, []string{"", "界", "é"}},
	} {
		t.Run(c.name, func(t *testing.T) {
			before := append([]string(nil), c.input...)
			got := StableUnique(c.input)
			if !reflect.DeepEqual(got, c.want) {
				t.Fatalf("got %q want %q", got, c.want)
			}
			for i, s := range before {
				if c.input[i] != s {
					t.Fatal("input changed")
				}
			}
		})
	}
}
