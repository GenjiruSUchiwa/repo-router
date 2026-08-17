
(import_declaration
  (scoped_identifier
    scope: (_) @import.path
    name: (identifier) @import.name) .) @import.from

(import_declaration
  (scoped_identifier) @import.path
  (asterisk) @import.glob) @import.import

(import_declaration
  . (identifier) @import.path .) @import.import
