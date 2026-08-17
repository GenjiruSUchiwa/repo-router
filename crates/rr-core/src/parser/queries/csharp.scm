(class_declaration
  name: (identifier) @name) @definition.class

(struct_declaration
  name: (identifier) @name) @definition.struct

(interface_declaration
  name: (identifier) @name) @definition.interface

(record_declaration
  name: (identifier) @name) @definition.record

(enum_declaration
  name: (identifier) @name) @definition.enum

(namespace_declaration
  name: (identifier) @name) @definition.module

(namespace_declaration
  name: (qualified_name) @name) @definition.module

(file_scoped_namespace_declaration
  name: (identifier) @name) @definition.module

(file_scoped_namespace_declaration
  name: (qualified_name) @name) @definition.module

(constructor_declaration
  name: (identifier) @name) @definition.constructor

(method_declaration
  name: (identifier) @name) @definition.method

(local_function_statement
  name: (identifier) @name) @definition.function

(property_declaration
  name: (identifier) @name) @definition.property

(field_declaration
  (variable_declaration
    (variable_declarator
      name: (identifier) @name))) @definition.field

(event_field_declaration
  (variable_declaration
    (variable_declarator
      name: (identifier) @name))) @definition.field

(invocation_expression
  function: (identifier) @name) @reference.call

(invocation_expression
  function: (member_access_expression
    name: (identifier) @name)) @reference.method-call

(object_creation_expression
  type: (identifier) @name) @reference.class

(base_list
  (identifier) @name) @reference.implementation
