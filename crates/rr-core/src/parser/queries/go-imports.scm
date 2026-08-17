
(import_spec !name path: (_) @import.path) @import.import

(import_spec
  name: (package_identifier) @import.alias
  path: (_) @import.path) @import.import

(import_spec
  name: (blank_identifier) @import.alias
  path: (_) @import.path) @import.import

(import_spec
  name: (dot) @import.glob
  path: (_) @import.path) @import.import
