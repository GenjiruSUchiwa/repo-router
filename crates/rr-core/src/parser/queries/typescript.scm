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

[
  (required_parameter (accessibility_modifier) pattern: (identifier) @name)
  (required_parameter "readonly" pattern: (identifier) @name)
  (optional_parameter (accessibility_modifier) pattern: (identifier) @name)
  (optional_parameter "readonly" pattern: (identifier) @name)
] @definition.field

(enum_body
  name: (property_identifier) @name @definition.field)

(enum_body
  (enum_assignment
    name: (property_identifier) @name) @definition.field)

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

(call_expression
  function: (identifier) @name) @reference.call

(new_expression
  constructor: (identifier) @name) @reference.call

(call_expression
  function: (member_expression
    property: (property_identifier) @name)) @reference.method
