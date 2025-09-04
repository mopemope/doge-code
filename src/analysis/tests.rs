#[cfg(test)]
mod tests {
    use crate::analysis::{Analyzer, SymbolKind};
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn parse_ts_symbols_with_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.ts");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
// This is a function
function foo() {{}}
/* This is an interface */
interface I {{ x: number }}
// This is a class
class C {{ 
    // This is a method
    bar() {{}} 
}}
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
        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 4); // 3 line comments + 1 block comment
    }

    #[test]
    fn parse_js_symbols_with_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.js");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
// This is a function
function foo() {{}}
// This is a class
class C {{ 
    // This is a method
    bar() {{}} 
}}
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
        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 3);
    }

    #[test]
    fn parse_py_symbols_with_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.py");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
# This is a class
class C:
    # This is a method
    def bar(self):
        pass

# This is a function
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
        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 3);
    }

    #[test]
    fn parse_go_symbols_with_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.go");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
package main

// This is a function
func foo() {{}}

// This is a struct
type S struct {{
    // This is a field
    X int
}}

// This is a method
func (s S) bar() {{}}
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
        assert!(names.contains(&("struct", "S")));
        assert!(names.contains(&("method", "bar")));
        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 3);
    }

    #[test]
    fn parse_rust_symbols_with_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lib.rs");
        let mut f = File::create(&path).unwrap();
        // Use write! with escaped braces in raw string to avoid format! placeholders
        write!(
            f,
            "
/// This is a function
fn alpha() {{}}
// This is a module
mod m {{ 
    // This is a function in module
    pub fn beta() {{}} 
}}
/// This is a struct
struct S {{ 
    /// This is a field
    x: i32 
}}
// This is an enum
enum E {{ A, B }}
/* This is a trait */
trait T {{ fn t(&self); }}
/// This is an impl
impl S {{ 
    /// This is an associated function
    fn new() -> Self {{ S {{ x: 0 }} }} 
    /// This is a method
    fn method(&self) {{}} 
}}
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
        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 7); // 4 line comments + 1 block comment + 2 doc comments
    }
}