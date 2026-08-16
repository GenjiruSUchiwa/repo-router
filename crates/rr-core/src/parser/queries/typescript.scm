; TypeScript tags query, written for rr rather than taken from upstream.
;
; `tree-sitter-typescript` ships a `tags.scm` that is a *supplement* to
; `tree-sitter-javascript`'s — alone it has no function, class, method or field
; pattern at all. This file is the whole query, and it is compiled twice: once
; against the TypeScript grammar and once against TSX, whose node names are a
; superset of it.
;
; Four things decide the shape of what follows.
;
; - `tree-sitter-tags` keeps one tag per name node, preferring the tag from the
;   earliest matching pattern. That is the only tie-break available, because
;   the crate compiles `#eq?`/`#match?` and then never evaluates them. So the
;   patterns run outermost first: `export declare function f()` is matched by
;   all three of the sections below and only the first sees the whole thing.
;
; - A definition reaches as far left as one pattern can say. The declaration
;   sections tag the `export_statement` or the `ambient_declaration`, not the
;   declaration inside it, for the reason `python.scm` tags a
;   `decorated_definition`: the span then covers the decorators and the
;   `export` and `declare` keywords, so decorators become attributes of what
;   they decorate and the signature says what the file says.
;
; - Bindings are the exception, and tag the `variable_declarator`. A
;   declaration owns its statement and a binding does not: `export const A = 1,
;   B = 2;` is one statement holding two of them, and giving each the whole
;   statement's span would have them contain each other. The cost is that a
;   binding's signature does not show `export`, which is a keyword rr reads
;   nothing from — visibility here is the `#` prefix, not the export list.
;
; - Anything the query cannot say — `constructor` is not an ordinary method, a
;   `get`/`set` accessor is not either, `const f = () => …` is a function
;   wearing a variable's syntax, `private`/`protected` are modifiers no capture
;   reaches — is decided in `typescript_refine`, not here.
;
; Documentation is the preceding comment run, captured as `@doc` and trimmed to
; the adjacent lines by `#select-adjacent!`. Nothing in the body is
; documentation; TypeScript has no docstring.
;
; Deliberately not covered, so that nothing claims more than it read:
; `var` at module scope, enum members, destructured bindings, string- and
; computed-named members, string-named ambient modules (`declare module "x"`),
; parameter properties (`constructor(private repo: Repo)`, which declares a
; field in a parameter list), `export`-as-visibility, JSX component references,
; and `describe`/`it` as test scopes — those are calls, and file naming already
; answers the question.

; ---------------------------------------------------------------------------
; Exported declarations, plain and ambient. First, so they win the tag for
; their name node and nothing to the left of `export` is lost.
; ---------------------------------------------------------------------------

((comment)* @doc
  .
  (export_statement
    declaration: [
      (function_declaration name: (identifier) @name)
      (generator_function_declaration name: (identifier) @name)
      (function_signature name: (identifier) @name)
      (ambient_declaration [
        (function_declaration name: (identifier) @name)
        (generator_function_declaration name: (identifier) @name)
        (function_signature name: (identifier) @name)
      ])
    ]) @definition.function
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.function))

((comment)* @doc
  .
  (export_statement
    declaration: [
      (class_declaration name: (type_identifier) @name)
      (abstract_class_declaration name: (type_identifier) @name)
      (ambient_declaration [
        (class_declaration name: (type_identifier) @name)
        (abstract_class_declaration name: (type_identifier) @name)
      ])
    ]) @definition.class
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.class))

((comment)* @doc
  .
  (export_statement
    declaration: [
      (interface_declaration name: (type_identifier) @name)
      (ambient_declaration (interface_declaration name: (type_identifier) @name))
    ]) @definition.interface
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.interface))

((comment)* @doc
  .
  (export_statement
    declaration: [
      (enum_declaration name: (identifier) @name)
      (ambient_declaration (enum_declaration name: (identifier) @name))
    ]) @definition.enum
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.enum))

((comment)* @doc
  .
  (export_statement
    declaration: [
      (type_alias_declaration name: (type_identifier) @name)
      (ambient_declaration (type_alias_declaration name: (type_identifier) @name))
    ]) @definition.type
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.type))

((comment)* @doc
  .
  (export_statement
    declaration: [
      (internal_module name: (identifier) @name)
      (module name: (identifier) @name)
      (ambient_declaration [
        (internal_module name: (identifier) @name)
        (module name: (identifier) @name)
      ])
    ]) @definition.namespace
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.namespace))

; `export` is only legal where a declaration is already top level, so this one
; needs no parent anchor to stay out of function bodies.
((comment)* @doc
  .
  (export_statement
    declaration: [
      (lexical_declaration
        (variable_declarator name: (identifier) @name) @definition.variable)
      (ambient_declaration
        (lexical_declaration
          (variable_declarator name: (identifier) @name) @definition.variable))
    ])
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.variable))

; ---------------------------------------------------------------------------
; Ambient declarations that are not exported — the body of a `.d.ts`, where
; every declaration is a promise about something compiled elsewhere. Second,
; so `declare` reaches the signature but `export declare` above still wins.
; ---------------------------------------------------------------------------

((comment)* @doc
  .
  (ambient_declaration [
    (function_declaration name: (identifier) @name)
    (generator_function_declaration name: (identifier) @name)
    (function_signature name: (identifier) @name)
  ]) @definition.function
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.function))

((comment)* @doc
  .
  (ambient_declaration [
    (class_declaration name: (type_identifier) @name)
    (abstract_class_declaration name: (type_identifier) @name)
  ]) @definition.class
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.class))

((comment)* @doc
  .
  (ambient_declaration
    (interface_declaration name: (type_identifier) @name)) @definition.interface
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.interface))

((comment)* @doc
  .
  (ambient_declaration
    (enum_declaration name: (identifier) @name)) @definition.enum
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.enum))

((comment)* @doc
  .
  (ambient_declaration
    (type_alias_declaration name: (type_identifier) @name)) @definition.type
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.type))

((comment)* @doc
  .
  (ambient_declaration [
    (internal_module name: (identifier) @name)
    (module name: (identifier) @name)
  ]) @definition.namespace
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.namespace))

; ---------------------------------------------------------------------------
; Declarations, wherever they sit. Each node type matches exactly one pattern.
; ---------------------------------------------------------------------------

((comment)* @doc
  .
  [
    (function_declaration name: (identifier) @name)
    (generator_function_declaration name: (identifier) @name)
    (function_signature name: (identifier) @name)
  ] @definition.function
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.function))

((comment)* @doc
  .
  [
    (class_declaration name: (type_identifier) @name)
    (abstract_class_declaration name: (type_identifier) @name)
  ] @definition.class
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.class))

((comment)* @doc
  .
  (interface_declaration
    name: (type_identifier) @name) @definition.interface
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.interface))

((comment)* @doc
  .
  (enum_declaration
    name: (identifier) @name) @definition.enum
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.enum))

((comment)* @doc
  .
  (type_alias_declaration
    name: (type_identifier) @name) @definition.type
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.type))

((comment)* @doc
  .
  [
    (internal_module name: (identifier) @name)
    (module name: (identifier) @name)
  ] @definition.namespace
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.namespace))

; ---------------------------------------------------------------------------
; Members. A class body and an interface body hold nothing else, so these need
; no anchor either.
; ---------------------------------------------------------------------------

((comment)* @doc
  .
  [
    (method_definition
      name: [(property_identifier) (private_property_identifier)] @name)
    (method_signature
      name: [(property_identifier) (private_property_identifier)] @name)
    (abstract_method_signature
      name: [(property_identifier) (private_property_identifier)] @name)
  ] @definition.method
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.method))

((comment)* @doc
  .
  [
    (public_field_definition
      name: [(property_identifier) (private_property_identifier)] @name)
    (property_signature
      name: [(property_identifier) (private_property_identifier)] @name)
  ] @definition.field
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.field))

; ---------------------------------------------------------------------------
; Bindings that are part of a surface. Anchored on the two scopes a module can
; declare into, because `statement_block` is also every function body and a
; local `const` is not API.
;
; Naming the parent costs these two their documentation, and that is the whole
; of the cost. A comment run written into a parent pattern only ever matches
; the *first* run under that parent: the query engine walks a named parent's
; children once and does not restart the sequence further along, so a second
; documented `const` in the same file silently loses its comment. A rule that
; holds only until something above it is commented is worse than no rule, so
; these two patterns read no documentation at all. Exported bindings — the
; surface anything outside the file can name — go through the anonymous group
; above, which has no parent to anchor to and no such limit.
; ---------------------------------------------------------------------------

(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name) @definition.variable))

(internal_module
  body: (statement_block
    (lexical_declaration
      (variable_declarator
        name: (identifier) @name) @definition.variable)))

; ---------------------------------------------------------------------------
; References.
; ---------------------------------------------------------------------------

(call_expression
  function: (identifier) @name) @reference.call

(new_expression
  constructor: (identifier) @name) @reference.call

; A call through a receiver, which the index leaves unresolved rather than
; guessing which same-named member was meant.
(call_expression
  function: (member_expression
    property: (property_identifier) @name)) @reference.method
