; Go imports, for rr's second extraction pass.
;
; Captures:
;   @import.import   the anchor; its span becomes Import::span
;   @import.path     the quoted module path, verbatim, quotes stripped
;   @import.alias    the local name this file chose, `_` included
;   @import.glob     presence marks the dot import
;
; One node type carries every form: a single `import "fmt"` and a member of an
; `import ( … )` block are both `import_spec`, so there is no factored and
; unfactored spelling to keep in step.
;
; `!name` on the first pattern is load-bearing: without it a named, blank or dot
; import matches twice and the declaration lands twice.

; import "fmt"
(import_spec !name path: (_) @import.path) @import.import

; import strs "strings"
(import_spec
  name: (package_identifier) @import.alias
  path: (_) @import.path) @import.import

; import _ "embed"
;
; The blank identifier is recorded as the alias rather than dropped: a blank
; import is a declaration that this file wants the package's side effects, and
; `alias: None` would make it indistinguishable from `import "embed"`.
(import_spec
  name: (blank_identifier) @import.alias
  path: (_) @import.path) @import.import

; import . "math"
;
; The one Go form that brings in names it does not write down.
(import_spec
  name: (dot) @import.glob
  path: (_) @import.path) @import.import
