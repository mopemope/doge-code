#[cfg(test)]
mod tests {
    use crate::analysis::{Analyzer, SymbolKind};
    use std::fs::File;
    use std::io::Write;

    #[tokio::test]
    async fn parse_csharp_symbols_with_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.cs");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            r#"
// This is a namespace
namespace NS {
    // This is a class
    public class C {
        // This is a field
        private int x;
        // This is a property
        public int X { get; set; }
        // This is a method
        public void M() {}
        // This is a constructor
        public C() {}
        // Event field declarations
        public event System.EventHandler? Changed;
        public event System.Action Something, Another;
        // Event declaration with accessors
        public event System.EventHandler Changed2 { add { } remove { } }
    }
    // A record type
    public record R(int Id, string Name);
    // A record struct type
    public readonly record struct RS(int X, int Y);
    // A delegate
    public delegate void D(int x);
    // This is an enum
    enum E { A, B }
}
"#
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();

        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str(), s.parent.clone()))
            .collect();

        // namespace, class, enum
        assert!(names.iter().any(|(k, n, _)| *k == "mod" && *n == "NS"));
        assert!(names.iter().any(|(k, n, _)| *k == "struct" && *n == "C"));
        assert!(names.iter().any(|(k, n, _)| *k == "enum" && *n == "E"));
        // methods, fields, properties
        assert!(names.iter().any(|(k, n, p)| *k == "method" && *n == "M" && p.as_deref() == Some("C")));
        assert!(names
            .iter()
            .any(|(k, n, p)| *k == "var" && *n == "X" && p.as_deref() == Some("C")));
        assert!(names
            .iter()
            .any(|(k, n, p)| *k == "var" && *n == "x" && p.as_deref() == Some("C")));

        // records (treated as struct)
        assert!(names.iter().any(|(k, n, _)| *k == "struct" && *n == "R"));
        assert!(names.iter().any(|(k, n, _)| *k == "struct" && *n == "RS"));

        // delegate (treated as function)
        assert!(names.iter().any(|(k, n, _)| *k == "fn" && *n == "D"));

        // events (treated as variables under class context)
        for ev in ["Changed", "Changed2", "Something", "Another"] {
            assert!(names
                .iter()
                .any(|(k, n, p)| *k == "var" && *n == ev && p.as_deref() == Some("C")));
        }

        // comments present
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert!(comment_count >= 6);
    }
}
