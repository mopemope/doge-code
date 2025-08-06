; Class definitions
(class_definition
  name: (identifier) @name.definition.class) @definition.class

; Function definitions (including methods)
(function_definition
  name: (identifier) @name.definition.function) @definition.function

; Method definitions (functions inside classes)
(class_definition
  body: (block
    (function_definition
      name: (identifier) @name.definition.method))) @definition.method

; Variable assignments (global level)
(module 
  (expression_statement 
    (assignment 
      left: (identifier) @name.definition.variable))) @definition.variable

; Constants (uppercase variables)
(module 
  (expression_statement 
    (assignment 
      left: (identifier) @name.definition.constant))) @definition.constant
  (#match? @name.definition.constant "^[A-Z][A-Z0-9_]*$")

; Import statements
(import_statement
  name: (dotted_name) @name.definition.import) @definition.import

(import_from_statement
  module_name: (dotted_name) @name.definition.import) @definition.import

; Function calls
(call
  function: [
      (identifier) @name.reference.call
      (attribute
        attribute: (identifier) @name.reference.call)
  ]) @reference.call

; Decorators
(decorator
  (identifier) @name.reference.decorator) @reference.decorator

; Class inheritance
(class_definition
  superclasses: (argument_list
    (identifier) @name.reference.class)) @reference.class
