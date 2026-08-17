(function_item
  name: (identifier) @def.name
  body: (block)? @def.body) @def.item

(function_signature_item
  name: (identifier) @def.name) @def.item @def.signature

(struct_item
  name: (type_identifier) @def.name
  body: [
    (field_declaration_list)
    (ordered_field_declaration_list)
  ]? @def.body) @def.item

(enum_item
  name: (type_identifier) @def.name
  body: (enum_variant_list)? @def.body) @def.item

(union_item
  name: (type_identifier) @def.name
  body: (field_declaration_list)? @def.body) @def.item

(trait_item
  name: (type_identifier) @def.name
  body: (declaration_list)? @def.body) @def.item

(type_item
  name: (type_identifier) @def.name
  type: (_)? @def.body) @def.item

(associated_type
  name: (type_identifier) @def.name
  bounds: (trait_bounds)? @def.body) @def.item

(const_item
  name: (identifier) @def.name
  value: (_)? @def.body) @def.item

(static_item
  name: (identifier) @def.name
  value: (_)? @def.body) @def.item

(mod_item
  name: (identifier) @def.name
  body: (declaration_list)? @def.body) @def.item

(macro_definition
  name: (identifier) @def.name) @def.item

(call_expression
  function: (identifier) @reference.name) @reference.call

(call_expression
  function: (scoped_identifier) @reference.name) @reference.call

(call_expression
  function: (generic_function
    function: (identifier) @reference.name)) @reference.call

(call_expression
  function: (generic_function
    function: (scoped_identifier) @reference.name)) @reference.call

(call_expression
  function: (field_expression
    field: (field_identifier) @reference.name)) @reference.method

(call_expression
  function: (generic_function
    function: (field_expression
      field: (field_identifier) @reference.name))) @reference.method

(macro_invocation
  macro: (identifier) @reference.name) @reference.macro

(macro_invocation
  macro: (scoped_identifier) @reference.name) @reference.macro

(impl_item
  trait: (_) @reference.name) @reference.implementation

(use_declaration) @import.declaration
(extern_crate_declaration) @import.declaration

(identifier) @identifier
(type_identifier) @identifier
(field_identifier) @identifier

(attribute_item) @attribute

(ERROR) @syntax.error
(MISSING) @syntax.missing
