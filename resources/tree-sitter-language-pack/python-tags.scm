;; Functions
(function_definition
  name: (identifier) @name
) @definition
(#set! "kind" "Function")

;; Classes
(class_definition
  name: (identifier) @name
) @definition
(#set! "kind" "Struct")

;; Methods (within classes, with self/cls as first param)
(class_definition
  name: (identifier) @parent_type
  (function_definition
    name: (identifier) @name
    parameters: (parameters
      (identifier) @first_param
      (#match? @first_param "^(self|cls)$")
    )
  ) @definition
)
(#set! "kind" "Method")
(#set! "parent" @parent_type)

;; Associated Functions (within classes, without self/cls as first param)
(class_definition
  name: (identifier) @parent_type
  (function_definition
    name: (identifier) @name
    parameters: (parameters
      (identifier) @first_param
      (#not-match? @first_param "^(self|cls)$")
    )
  ) @definition
)
(#set! "kind" "AssocFn")
(#set! "parent" @parent_type)

;; Variables (assignments)
(assignment
  left: (identifier) @name
) @definition
(#set! "kind" "Variable")

;; Variables (multiple assignments, tuple/list unpacking)
(assignment
  left: [
    (pattern_list (identifier) @name)
    (tuple_pattern (identifier) @name)
    (list_pattern (identifier) @name)
  ]
) @definition
(#set! "kind" "Variable")