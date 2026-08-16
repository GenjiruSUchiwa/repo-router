; Python tags query, derived from tree-sitter-python's but not pinned to it.
;
; Two deliberate departures from upstream:
;
; - A decorated definition is tagged at its `decorated_definition` wrapper, so
;   the span covers the decorators and their identifiers become attributes of
;   the decorated definition instead of body identifiers of whatever encloses
;   it. The bare patterns anchor on a `module`/`block` parent — the only other
;   parents a definition can have — so a decorated definition matches exactly
;   one pattern.
;
; - The docstring that opens a body is captured as `@doc`, so its prose lands
;   in documentation identifiers rather than polluting body identifiers.

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

; A call through a receiver, which the index leaves unresolved rather than
; guessing which same-named member was meant. Spelled as TypeScript spells it
; (`typescript.scm`), because the two languages disagreed and this one was wrong.
(call
  function: (attribute
    attribute: (identifier) @name)) @reference.method
