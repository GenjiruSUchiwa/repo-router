; Java imports, for rr's second extraction pass.
;
; Captures:
;   @import.from / @import.import   the anchor; its span becomes Import::span
;   @import.path   everything left of the last dot
;   @import.name   the leaf, when the declaration names one
;   @import.glob   presence marks an on-demand import
;
; The trailing `.` anchor on the first pattern is load-bearing: it requires the
; scoped identifier to be the declaration's last named child, which is what
; separates `import a.b.C;` from `import a.b.*;` — the latter holds the same
; scoped identifier followed by an asterisk, and without the anchor it would
; also match here with path `a` and name `b`.
;
; `import static a.b.C.method;` needs no pattern of its own: `static` is an
; anonymous token, so it parses as the first pattern with path `a.b.C` and name
; `method`, which is what it means. rr records no static/instance distinction
; on an import, and inventing one here would be a claim `Import` cannot hold.

; import java.util.List;   /   import static java.util.Arrays.asList;
(import_declaration
  (scoped_identifier
    scope: (_) @import.path
    name: (identifier) @import.name) .) @import.from

; import java.util.*;
(import_declaration
  (scoped_identifier) @import.path
  (asterisk) @import.glob) @import.import

; import Foo;  — the default package
(import_declaration
  . (identifier) @import.path .) @import.import
