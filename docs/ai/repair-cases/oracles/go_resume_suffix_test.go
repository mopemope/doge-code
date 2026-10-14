package acceptancefixture

import "testing"

func TestFixedSuffix(t *testing.T) {
	for _, c := range []struct{ name, value, suffix, want string }{{"ordinary", "foobar", "bar", "foo"}, {"repeat", "fooooo", "oo", "fooo"}, {"extension", "file.txt.txt", ".txt", "file.txt"}, {"missing", "xyz", "a", "xyz"}, {"unicode", "a界界", "界", "a界"}, {"empty", "abc", "", "abc"}, {"all_repeat", "aaa", "aa", "a"}} {
		t.Run(c.name, func(t *testing.T) {
			if got := TrimSuffixOnce(c.value, c.suffix); got != c.want {
				t.Fatalf("got %q want %q", got, c.want)
			}
		})
	}
}
