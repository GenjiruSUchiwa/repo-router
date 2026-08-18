(using_directive
  !name
  (identifier) @import.path) @import.import

(using_directive
  !name
  (qualified_name) @import.path) @import.import

(using_directive
  name: (identifier) @import.alias
  (qualified_name) @import.path) @import.import

(using_directive
  name: (identifier) @import.alias
  (identifier) @import.path) @import.import
