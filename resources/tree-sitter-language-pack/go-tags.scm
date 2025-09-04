;; Functions
(function_declaration
  name: (identifier) @name
) @definition
(#set! "kind" "Function")

;; Methods
(method_declaration
  name: (field_identifier) @name
  receiver: (parameter_list (parameter_declaration type: (type) @parent_type))
) @definition
(#set! "kind" "Method")
(#set! "parent" @parent_type)

;; Structs (type declarations with struct_type)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (struct_type)
  ) @definition
)
(#set! "kind" "Struct")

;; Interfaces (type declarations with interface_type)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (interface_type)
  )
) @definition
(#set! "kind" "Trait") ;; Using Trait for interface

;; Variables (var declarations)
(var_declaration
  (var_spec
    name: (identifier) @name
  ) @definition
)
(#set! "kind" "Variable")

;; Constants (const declarations)
(const_declaration
  (const_spec
    name: (identifier) @name
  ) @definition
)
(#set! "kind" "Variable")

;; Line comments
(comment) @comment
(#set! "kind" "Comment")