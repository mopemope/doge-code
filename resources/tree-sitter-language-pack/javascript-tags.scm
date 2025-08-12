;; Functions
(function_declaration
  name: (identifier) @name
) @definition
(#set! "kind" "Function")

;; Classes
(class_declaration
  name: (identifier) @name
) @definition
(#set! "kind" "Struct")

;; Methods
(method_definition
  name: (property_identifier) @name
) @definition
(#set! "kind" "Method")
;; Parent context for methods: class_declaration
(class_declaration
  name: (identifier) @parent_type
  (method_definition
    name: (property_identifier) @name
  ) @definition
)
(#set! "parent" @parent_type)

;; Variables (var, let, const)
(variable_declarator
  name: (identifier) @name
) @definition
(#set! "kind" "Variable")