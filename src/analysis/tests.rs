#[cfg(test)]
mod analysis_tests {
    use crate::analysis::{Analyzer, SymbolKind};
    use std::fs::File;
    use std::io::Write;

    #[tokio::test]
    async fn parse_ts_symbols_with_comments() {
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
        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&(SymbolKind::Function.as_str(), "foo")));
        assert!(names.contains(&(SymbolKind::Trait.as_str(), "I")));
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "C")));
        assert!(names.contains(&(SymbolKind::Method.as_str(), "bar")));
        assert!(names.contains(&(SymbolKind::Enum.as_str(), "E")));
        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 4); // 3 line comments + 1 block comment
    }

    #[tokio::test]
    async fn parse_js_symbols_with_comments() {
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
        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&(SymbolKind::Function.as_str(), "foo")));
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "C")));
        assert!(names.contains(&(SymbolKind::Method.as_str(), "bar")));
        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 3);
    }

    #[tokio::test]
    async fn parse_py_symbols_with_comments() {
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
        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "C")));
        assert!(names.contains(&(SymbolKind::Method.as_str(), "bar")));
        assert!(names.contains(&(SymbolKind::Function.as_str(), "foo")));
        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 3);
    }

    #[tokio::test]
    async fn parse_go_symbols_with_comments() {
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
        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();
        assert!(names.contains(&(SymbolKind::Function.as_str(), "foo")));
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "S")));
        assert!(names.contains(&(SymbolKind::Method.as_str(), "bar")));
        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 4);
    }

    #[tokio::test]
    async fn parse_rust_symbols_with_comments() {
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

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
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
        assert_eq!(comment_count, 10); // Updated count based on current implementation
    }

    #[tokio::test]
    async fn parse_c_symbols_with_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.c");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
// This is a struct
struct Point {{
    // This is a field
    int x;
    int y;
}};

// This is an enum
enum Color {{
    RED,
    GREEN,
    BLUE
}};

/* This is a function */
int add(int a, int b) {{
    return a + b;
}}

// This is a variable
int global_var = 10;

// This is another function
void print_message() {{
    printf(\"Hello, World!\\n\");
}}
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();

        // Check for function definitions
        assert!(names.contains(&(SymbolKind::Function.as_str(), "add")));
        assert!(names.contains(&(SymbolKind::Function.as_str(), "print_message")));

        // Check for struct definitions
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Point")));

        // Check for enum definitions
        assert!(names.contains(&(SymbolKind::Enum.as_str(), "Color")));

        // Check for variable declarations (global variables only)
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "global_var")));
        // Note: Struct fields (x, y) are not extracted as separate variables by the C collector

        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 6); // 5 line comments + 1 block comment
    }

    #[tokio::test]
    async fn parse_cpp_symbols_with_comments() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.cpp");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
// This is a function
int add(int a, int b) {{
    return a + b;
}}

/* This is a struct */
struct Point {{
    // This is a field
    int x;
    int y;
}};

// This is a class
class Rectangle {{
    // These are private fields
    Point origin;
    int width;
    int height;
    
public:
    // This is a constructor
    Rectangle(Point p, int w, int h) : origin(p), width(w), height(h) {{}}
}};

// This is an enum
enum Color {{
    RED,
    GREEN,
    BLUE
}};

// This is a variable
int global_var = 10;

// This is another function
void print_message() {{
    printf(\"Hello, World!\\n\");
}}
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();

        // Check for function definitions
        assert!(names.contains(&(SymbolKind::Function.as_str(), "add")));
        assert!(names.contains(&(SymbolKind::Function.as_str(), "print_message")));

        // Check for struct definitions
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Point")));

        // Check for class definitions (C++ classes are treated as structs)
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Rectangle")));

        // Check for enum definitions
        assert!(names.contains(&(SymbolKind::Enum.as_str(), "Color")));

        // Check for variable declarations (global variables only)
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "global_var")));

        // Check for constructor
        assert!(names.contains(&(SymbolKind::Function.as_str(), "Rectangle")));

        // Check for comments
        let comment_count = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .count();
        assert_eq!(comment_count, 9); // 9 line comments
    }

    #[tokio::test]
    async fn parse_cpp_symbols_comprehensive() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("comprehensive.cpp");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "// Test file for C++ symbol extraction
// This comment should be associated with the following function
int calculateSum(int a, int b) {{
    return a + b;
}}

// This comment should be associated with the struct
struct Person {{
    // Field comment
    std::string name;
    int age;
}};

/* Block comment for class */
class Calculator {{
    // Private field comment
    int result;
    
public:
    // Constructor comment
    Calculator() : result(0) {{}}
}};

// Enum comment
enum class Direction {{
    NORTH,
    SOUTH,
    EAST,
    WEST
}};

// Variable declaration comment
double pi = 3.14159;

// Another function with comment
std::string formatName(const std::string& first, const std::string& last) {{
    return first + \" \" + last;
}}
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();

        // Test 1: Function definition extraction
        let function_names: Vec<_> = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function)
            .map(|s| s.name.as_str())
            .collect();
        assert!(function_names.contains(&"calculateSum"));
        assert!(function_names.contains(&"formatName"));
        assert!(function_names.contains(&"Calculator")); // Constructor

        // Test 2: Struct definition extraction
        let struct_names: Vec<_> = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Struct)
            .map(|s| s.name.as_str())
            .collect();
        assert!(struct_names.contains(&"Person"));
        assert!(struct_names.contains(&"Calculator"));

        // Test 3: Enum definition extraction
        let enum_names: Vec<_> = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Enum)
            .map(|s| s.name.as_str())
            .collect();
        assert!(enum_names.contains(&"Direction"));

        // Test 4: Variable declaration extraction
        let var_names: Vec<_> = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Variable)
            .map(|s| s.name.as_str())
            .collect();
        assert!(var_names.contains(&"pi"));

        // Test 5: Comment extraction (line comments and block comments)
        let comments: Vec<_> = map
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Comment)
            .collect();
        assert!(comments.len() >= 8); // Should have at least 8 comments

        // Check for specific comments
        let comment_texts: Vec<_> = comments.iter().map(|s| s.name.as_str()).collect();
        assert!(comment_texts.contains(&"// Test file for C++ symbol extraction"));
        assert!(
            comment_texts
                .contains(&"// This comment should be associated with the following function")
        );
        assert!(comment_texts.contains(&"/* Block comment for class */"));

        // Test 6: Comment and symbol association
        // Find the calculateSum function and check if it has associated keywords from comments
        let calc_func = map
            .symbols
            .iter()
            .find(|s| s.name == "calculateSum" && s.kind == SymbolKind::Function);
        assert!(calc_func.is_some());
        if let Some(func) = calc_func {
            // Should have keywords from the comment "This comment should be associated with the following function"
            assert!(!func.keywords.is_empty());
        }

        // Find the Person struct and check if it has associated keywords from comments
        let person_struct = map
            .symbols
            .iter()
            .find(|s| s.name == "Person" && s.kind == SymbolKind::Struct);
        assert!(person_struct.is_some());
        if let Some(person) = person_struct {
            // Should have keywords from the comment "This comment should be associated with the struct"
            assert!(!person.keywords.is_empty());
        }

        // Find the Calculator class and check if it has associated keywords from comments
        let calc_class = map
            .symbols
            .iter()
            .find(|s| s.name == "Calculator" && s.kind == SymbolKind::Struct);
        assert!(calc_class.is_some());
        if let Some(calc) = calc_class {
            // Should have keywords from the comment "Block comment for class"
            assert!(!calc.keywords.is_empty());
        }
    }

    #[tokio::test]
    async fn parse_c_preprocessor_directives() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("preprocessor.c");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
#define MAX_SIZE 100
#define SQUARE(x) ((x) * (x))
#define STRINGIFY(x) #x

#include <stdio.h>
#include \"myheader.h\"

#ifdef DEBUG
int debug_mode = 1;
#endif

#ifndef MAX_SIZE
#define MAX_SIZE 50
#endif

#if MAX_SIZE > 50
int large_buffer = 1;
#else
int small_buffer = 1;
#endif
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();

        // Test that preprocessor directives are handled appropriately
        // Note: Currently the C collector doesn't extract #define as symbols,
        // but we should verify they don't interfere with other parsing
        let symbols_count = map.symbols.len();
        assert!(symbols_count > 0); // At minimum, should have some symbols
    }

    #[tokio::test]
    async fn parse_c_pointer_declarations() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pointers.c");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
// Simple pointer
int *ptr;

// Pointer to const
const int *ptr_to_const;

// Const pointer
int * const const_ptr;

// Pointer to struct
struct Point {{
    int x;
    int y;
}};
struct Point *point_ptr;

// Function pointer
int (*func_ptr)(int, int);

// Array of pointers
int *ptr_array[10];

// Double pointer
int **double_ptr;
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();

        // Check for struct definitions
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Point")));

        // Check for variable declarations (pointers should be recognized as variables)
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "ptr")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "ptr_to_const")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "const_ptr")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "point_ptr")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "func_ptr")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "ptr_array")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "double_ptr")));
    }

    #[tokio::test]
    async fn parse_c_array_declarations() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("arrays.c");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
// Single-dimensional array
int numbers[10];

// Multi-dimensional array
int matrix[3][4];

// Array with initialization
int initialized[5] = {{1, 2, 3, 4, 5}};

// Character array (string)
char name[50];

// Array of structs
struct Point {{
    int x;
    int y;
}};
struct Point points[100];

// Array of pointers
int *ptr_array[20];
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();

        // Check for struct definitions
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Point")));

        // Check for variable declarations (arrays should be recognized as variables)
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "numbers")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "matrix")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "initialized")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "name")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "points")));
        assert!(names.contains(&(SymbolKind::Variable.as_str(), "ptr_array")));
    }

    #[tokio::test]
    async fn parse_c_nested_declarations() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested.c");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
// Nested struct
struct Outer {{
    int x;
    struct Inner {{
        int y;
        int z;
    }} inner;
}};

// Union
union Data {{
    int i;
    float f;
    char str[20];
}};

// Struct containing union
struct Container {{
    int id;
    union Data data;
}};

// Union containing struct
union Mixed {{
    struct Point {{
        int x;
        int y;
    }} point;
    int array[4];
}};
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();

        // Check for struct definitions
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Outer")));
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Inner")));
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Container")));
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Point")));

        // Check for union definitions
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Data"))); // Unions are treated as structs
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Mixed"))); // Unions are treated as structs
    }

    #[tokio::test]
    async fn parse_c_function_declarations() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("declarations.c");
        let mut f = File::create(&path).unwrap();
        write!(
            f,
            "
// Forward declaration
int forward_func(int x);

// Function prototype with complex parameters
struct Point;
int process_point(struct Point *p, int count);

// External function declaration
extern void external_func(void);

// Function definition (should still be detected)
int actual_func() {{
    return 42;
}}

// Another function using forward declared types
struct Point {{
    int x;
    int y;
}};
int process_point(struct Point *p, int count) {{
    return p->x + p->y + count;
}}
"
        )
        .unwrap();

        let mut analyzer = Analyzer::new(tmp.path()).await.unwrap();
        let map = analyzer.build().await.unwrap();
        let names: Vec<_> = map
            .symbols
            .iter()
            .map(|s| (s.kind.as_str(), s.name.as_str()))
            .collect();

        // Check for function definitions (only functions with bodies should be extracted)
        assert!(names.contains(&(SymbolKind::Function.as_str(), "actual_func")));
        assert!(names.contains(&(SymbolKind::Function.as_str(), "process_point")));

        // Check for struct definitions
        assert!(names.contains(&(SymbolKind::Struct.as_str(), "Point")));

        // Forward declarations without bodies should not be extracted as Function symbols
        // but we should verify they don't cause parsing errors
    }
}
