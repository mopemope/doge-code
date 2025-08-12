;; Functions
(function_item
  name: (identifier) @name
) @definition
(#set! "kind" "Function")

;; Structs
(struct_item
  name: (identifier) @name
) @definition
(#set! "kind" "Struct")

;; Enums
(enum_item
  name: (identifier) @name
) @definition
(#set! "kind" "Enum")

;; Traits
(trait_item
  name: (identifier) @name
) @definition
(#set! "kind" "Trait")

;; Impl blocks (the impl itself)
(impl_item
  (type) @name
) @definition
(#set! "kind" "Impl")

;; Methods (within impl blocks)
(impl_item
  (function_item
    name: (identifier) @name
    (parameters (self_parameter)) @receiver_param
  ) @definition
)
(#set! "kind" "Method")
;; Parent context for methods: capture the type being implemented
(impl_item
  type: (type) @parent_type
  (function_item
    name: (identifier) @name
    (parameters (self_parameter)) @receiver_param
  ) @definition
)
(#set! "parent" @parent_type)

;; Associated Functions (within impl blocks, no self_parameter)
(impl_item
  (function_item
    name: (identifier) @name
    (parameters) @params
    (#not-has-field! receiver) ;; Ensure no receiver
  ) @definition
)
(#set! "kind" "AssocFn")
;; Parent context for associated functions: capture the type being implemented
(impl_item
  type: (type) @parent_type
  (function_item
    name: (identifier) @name
    (parameters) @params
    (#not-has-field! receiver)
  ) @definition
)
(#set! "parent" @parent_type)

;; Modules
(mod_item
  name: (identifier) @name
) @definition
(#set! "kind" "Mod")

;; Variables (let declarations)
(let_declaration
  pattern: (identifier) @name
) @definition
(#set! "kind" "Variable")

;; Variables (tuple/struct patterns - basic extraction)
(let_declaration
  pattern: [
    (tuple_pattern (identifier) @name)
    (struct_pattern (identifier) @name)
  ]
) @definition
(#set! "kind" "Variable")