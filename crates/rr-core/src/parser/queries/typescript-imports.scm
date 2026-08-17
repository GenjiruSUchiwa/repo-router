
(import_statement
  (import_clause (identifier) @import.alias @import.import)
  source: (string) @import.path)

(import_statement
  (import_clause (namespace_import (identifier) @import.alias) @import.import)
  source: (string) @import.path)

(import_statement
  (import_clause
    (named_imports
      (import_specifier !alias name: (_) @import.name) @import.from))
  source: (string) @import.path)

(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (_) @import.name
        alias: (_) @import.alias) @import.from))
  source: (string) @import.path)

(import_statement
  . source: (string) @import.path @import.import)

(import_require_clause
  (identifier) @import.alias
  source: (string) @import.path) @import.require

(call_expression
  function: (identifier) @import.callee
  arguments: (arguments . (string) @import.path)) @import.require

(export_statement
  (export_clause
    (export_specifier !alias name: (_) @import.name) @import.from)
  source: (string) @import.path) @import.public

(export_statement
  (export_clause
    (export_specifier
      name: (_) @import.name
      alias: (_) @import.alias) @import.from)
  source: (string) @import.path) @import.public

(export_statement
  . source: (string) @import.path @import.from @import.glob) @import.public

(export_statement
  (namespace_export (identifier) @import.alias) @import.import
  source: (string) @import.path) @import.public
