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
;   statement's span would have them contain each other. The signature
;   therefore does not show `export`; visibility still does, because the
;   exported and non-exported patterns are different captures
;   (`@definition.variable` vs `@definition.local-variable`) rather than a
;   keyword read off the signature.
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
; destructured bindings, string- and computed-named members, string-named
; ambient modules (`declare module "x"`), JSX component references, and
; `describe`/`it` as test scopes — those are calls, and file naming already
; answers the question.
;
; The `local-` captures read one thing only: no `export` keyword sits on this
; declaration. Two shapes make that a weaker claim than it sounds, and both are
; recorded rather than guessed at:
;
; - A declaration exported from somewhere else in the file — `function join() {}`
;   followed by `export { join };`, or `export default join;` — is `Private`
;   here. Binding an export list to the declaration it names is resolution, and
;   this tier does not resolve.
;
; - A member of an ambient namespace — `export declare namespace S { const x; }`
;   — is `Private` here, though TypeScript exports every ambient member whether
;   or not the keyword is written.

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
      (variable_declaration
        (variable_declarator name: (identifier) @name) @definition.variable)
      (ambient_declaration
        (lexical_declaration
          (variable_declarator name: (identifier) @name) @definition.variable))
      (ambient_declaration
        (variable_declaration
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
  ] @definition.local-function
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.local-function))

((comment)* @doc
  .
  [
    (class_declaration name: (type_identifier) @name)
    (abstract_class_declaration name: (type_identifier) @name)
  ] @definition.local-class
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.local-class))

((comment)* @doc
  .
  (interface_declaration
    name: (type_identifier) @name) @definition.local-interface
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.local-interface))

((comment)* @doc
  .
  (enum_declaration
    name: (identifier) @name) @definition.local-enum
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.local-enum))

((comment)* @doc
  .
  (type_alias_declaration
    name: (type_identifier) @name) @definition.local-type
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.local-type))

((comment)* @doc
  .
  [
    (internal_module name: (identifier) @name)
    (module name: (identifier) @name)
  ] @definition.local-namespace
  (#strip! @doc "^[/\\*\\s]+|[\\s\\*/]+$")
  (#select-adjacent! @doc @definition.local-namespace))

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

; Parameter properties: a field declared in the constructor's parameter list,
; which is the one place TypeScript declares state somewhere other than a class
; body. `constructor(private readonly repo: Repo)` is a `repo` field and a
; `repo` parameter at once, and dropping it loses a field that the rest of the
; class refers to as if it had been written out.
;
; What separates one from an ordinary parameter is a modifier, and both spelling
; are matched structurally rather than by predicate: `accessibility_modifier` is
; a named node, `readonly` an anonymous token, and a plain parameter has
; neither. `pattern:` is named explicitly so that a default value —
; `private repo: Repo = fallback`, whose `fallback` is an `identifier` sibling —
; is not read as a second field.
[
  (required_parameter (accessibility_modifier) pattern: (identifier) @name)
  (required_parameter "readonly" pattern: (identifier) @name)
  (optional_parameter (accessibility_modifier) pattern: (identifier) @name)
  (optional_parameter "readonly" pattern: (identifier) @name)
] @definition.field

; Enum members. A TypeScript enum member is a named constant (`Ceiling.High`),
; not a constructor, and is a stored member of the enum type.
;
; The definition is the member, not the `enum_body` holding it: tagging the body
; would give every member the same span — the whole `{ … }` — so each member
; would read its siblings as its own body text and land on the opening brace
; when navigated to. A valueless member is a bare `property_identifier`, so
; there the name node and the definition node are the same node.
(enum_body
  name: (property_identifier) @name @definition.field)

(enum_body
  (enum_assignment
    name: (property_identifier) @name) @definition.field)

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
  [
    (lexical_declaration
      (variable_declarator
        name: (identifier) @name) @definition.local-variable)
    (variable_declaration
      (variable_declarator
        name: (identifier) @name) @definition.local-variable)
  ])

(internal_module
  body: (statement_block
    [
      (lexical_declaration
        (variable_declarator
          name: (identifier) @name) @definition.local-variable)
      (variable_declaration
        (variable_declarator
          name: (identifier) @name) @definition.local-variable)
    ]))

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
