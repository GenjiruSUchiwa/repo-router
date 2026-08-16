; Python imports, for rr's second extraction pass.
;
; Not a tags query and not runnable as one: `tree-sitter-tags` accepts only
; @name, @ignore, @doc, @local.*, @definition.* and @reference.*, and rejects
; everything else with InvalidCapture. This is a plain `tree_sitter::Query`,
; run by `TagsExtractor::collect_imports`.
;
; Captures:
;   @import.import / @import.from   the anchor; its span becomes Import::span
;   @import.path                    the source, verbatim and unresolved
;   @import.name                    the leaf selected out of that source
;   @import.alias                   the local name this file chose
;   @import.glob                    presence marks a star import
;
; `path` is never joined to `name` and never resolved: `.` and `..pkg` are
; relative to the importing module's package, which is a module graph rr does
; not build.
;
; Deliberately not covered: `from __future__ import annotations`. It enables a
; compiler flag rather than naming a module, and its `future_import_statement`
; holds no node whose text is `__future__` for @import.path to point at.

; import os        /  import os.path        /  import a, b
(import_statement
  name: (dotted_name) @import.path @import.import)

; import os.path as p
(import_statement
  name: (aliased_import
          name: (dotted_name) @import.path
          alias: (identifier) @import.alias) @import.import)

; from x import y   /  from . import y   /  from ..pkg import y
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.path
  name: (dotted_name) @import.name @import.from)

; from x import y as z
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.path
  name: (aliased_import
          name: (dotted_name) @import.name
          alias: (identifier) @import.alias) @import.from)

; from x import *
(import_from_statement
  module_name: [(dotted_name) (relative_import)] @import.path
  (wildcard_import) @import.glob @import.from)