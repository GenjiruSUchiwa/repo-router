
(namespace_use_declaration
  (namespace_use_clause
    (qualified_name
      prefix: (namespace_name) @import.path
      (name) @import.name) !alias) @import.from)

(namespace_use_declaration
  (namespace_use_clause
    (qualified_name
      prefix: (namespace_name) @import.path
      (name) @import.name)
    alias: (name) @import.alias) @import.from)

(namespace_use_declaration
  (namespace_name) @import.path
  body: (namespace_use_group
          (namespace_use_clause (name) @import.name) @import.from))

(require_expression [(string) (encapsed_string)] @import.path) @import.require
(require_once_expression [(string) (encapsed_string)] @import.path) @import.require
(include_expression [(string) (encapsed_string)] @import.path) @import.require
(include_once_expression [(string) (encapsed_string)] @import.path) @import.require
