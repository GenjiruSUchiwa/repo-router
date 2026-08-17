
(import_statement
  name: (dotted_name) @import.path @import.import)

(import_statement
  name: (aliased_import
          name: (dotted_name) @import.path
          alias: (identifier) @import.alias) @import.import)

(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.path
  name: (dotted_name) @import.name @import.from)

(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.path
  name: (aliased_import
          name: (dotted_name) @import.name
          alias: (identifier) @import.alias) @import.from)

(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.path
  (wildcard_import) @import.glob @import.from)
