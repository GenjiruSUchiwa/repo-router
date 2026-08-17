(module
  (expression_statement
    (assignment
      left: (identifier) @name) @definition.constant))

(decorated_definition
  definition: (class_definition
    name: (identifier) @name
    body: (block . (expression_statement (string) @doc)?))) @definition.class

(decorated_definition
  definition: (function_definition
    name: (identifier) @name
    body: (block . (expression_statement (string) @doc)?))) @definition.function

(module
  (class_definition
    name: (identifier) @name
    body: (block . (expression_statement (string) @doc)?)) @definition.class)

(module
  (function_definition
    name: (identifier) @name
    body: (block . (expression_statement (string) @doc)?)) @definition.function)

(block
  (class_definition
    name: (identifier) @name
    body: (block . (expression_statement (string) @doc)?)) @definition.class)

(block
  (function_definition
    name: (identifier) @name
    body: (block . (expression_statement (string) @doc)?)) @definition.function)

(call
  function: (identifier) @name) @reference.call

(call
  function: (attribute
    attribute: (identifier) @name)) @reference.method
