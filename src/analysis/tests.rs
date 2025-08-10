#[cfg(test)]
mod tests {
    use crate::analysis::{Analyzer, SymbolKind};
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn parse_ts_symbols_minimal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.ts");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
function foo() {{}}
interface I {{ x: number }}
class C {{ bar() {{}} }}
enum E {{ A, B }}
"
        )
        .unwrap();
        let mut analyzer = Analyzer::new(tmp.path()).unwrap();
        let map = analyzer.build().unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&("fn", "foo")));
        assert!(names.contains(&("trait", "I")));
        assert!(names.contains(&("struct", "C")));
        assert!(names.contains(&("method", "bar")));
        assert!(names.contains(&("enum", "E")));
    }

    #[test]
    fn parse_js_symbols_minimal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.js");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
function foo() {{}}
class C {{ bar() {{}} }}
"
        )
        .unwrap();
        let mut analyzer = Analyzer::new(tmp.path()).unwrap();
        let map = analyzer.build().unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&("fn", "foo")));
        assert!(names.contains(&("struct", "C")));
        assert!(names.contains(&("method", "bar")));
    }

    #[test]
    fn parse_py_symbols_minimal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.py");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
class C:
    def bar(self):
        pass

def foo():
    pass
"
        )
        .unwrap();
        let mut analyzer = Analyzer::new(tmp.path()).unwrap();
        let map = analyzer.build().unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&("struct", "C")));
        assert!(names.contains(&("method", "bar")));
        assert!(names.contains(&("fn", "foo")));
    }

    // keep Rust test last; avoid duplicate imports
    #[test]
    fn parse_rust_symbols_various() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lib.rs");
        let mut f = File::create(&path).unwrap();
        // Use write! with escaped braces in raw string to avoid format! placeholders
        write!(
            f,
            "
fn alpha() {{}}
mod m {{ pub fn beta() {{}} }}
struct S {{ x: i32 }}
enum E {{ A, B }}
trait T {{ fn t(&self); }}
impl S {{ fn new() -> Self {{ S {{ x: 0 }} }} fn method(&self) {{}} }}
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).unwrap();
        let map = analyzer.build().unwrap();
        let by_kind = |k: SymbolKind| -> Vec<String> {
            map.symbols
                .iter()
                .filter(|s| s.kind == k)
                .map(|s| s.name.clone())
                .collect()
        };
        assert!(by_kind(SymbolKind::Function).contains(&"alpha".to_string()));
        assert!(by_kind(SymbolKind::Struct).contains(&"S".to_string()));
        assert!(by_kind(SymbolKind::Enum).contains(&"E".to_string()));
        assert!(by_kind(SymbolKind::Trait).contains(&"T".to_string()));
        // impl symbol present and methods/assoc fns captured
        assert!(map.symbols.iter().any(|s| s.kind == SymbolKind::Impl));
        assert!(by_kind(SymbolKind::AssocFn).contains(&"new".to_string()));
        assert!(by_kind(SymbolKind::Method).contains(&"method".to_string()));
    }
}
