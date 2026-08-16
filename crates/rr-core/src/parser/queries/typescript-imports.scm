; TypeScript and TSX imports, for rr's second extraction pass.
;
; Compiled twice, once per grammar, for the reason `typescript.scm` is compiled
; twice: a `tree_sitter::Query` holds the language it was built against, so one
; shared `OnceLock` would hand every .tsx file the .ts parser.
;
; Captures:
;   @import.import / @import.from / @import.require  anchor; span -> Import::span
;   @import.path     the source specifier, verbatim, quotes stripped
;   @import.name     the leaf selected out of that source
;   @import.alias    the local name this file chose
;   @import.glob     presence marks a declaration bringing in unwritten names
;   @import.public   presence marks a re-export
;   @import.callee   guard: the match is dropped unless its text is `require`
;
; Deliberately not covered, so that nothing claims more than it read:
;   - dynamic `import(expr)`: its argument is an arbitrary expression, and
;     recording only the string-literal case is a partial list that reads as
;     complete.
;   - `import type` / `export type` modifiers: neither changes which module is
;     named, and rr records no type-only distinction.
;   - `export = x`: it exports, it imports nothing.

; import x from "m"
(import_statement
  (import_clause (identifier) @import.alias @import.import)
  source: (string) @import.path)

; import * as ns from "m"
;
; Not a glob: it binds one name. `is_glob` marks a declaration that brings in
; names it does not write down, and this one writes its name down.
(import_statement
  (import_clause (namespace_import (identifier) @import.alias) @import.import)
  source: (string) @import.path)

; import { A } from "m"
;
; `!alias` is load-bearing: without it this pattern also matches `{ A as B }`
; and the declaration lands twice.
(import_statement
  (import_clause
    (named_imports
      (import_specifier !alias name: (_) @import.name) @import.from))
  source: (string) @import.path)

; import { A as B } from "m"
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (_) @import.name
        alias: (_) @import.alias) @import.from))
  source: (string) @import.path)

; import "m"
;
; The `.` anchor is what separates a bare import from every clause form: a bare
; import's first named child is its source, a clause form's is its clause.
(import_statement
  . source: (string) @import.path @import.import)

; import x = require("m")
(import_require_clause
  (identifier) @import.alias
  source: (string) @import.path) @import.require

; require("m")
;
; No alias, even in `const x = require("m")`: that binding is already a
; module-level variable definition in `typescript.scm`, and naming it here too
; would file one declaration under two names. `import x = require("m")` above
; declares no variable, so there the identifier is the alias.
(call_expression
  function: (identifier) @import.callee
  arguments: (arguments . (string) @import.path)) @import.require

; export { A } from "m"
(export_statement
  (export_clause
    (export_specifier !alias name: (_) @import.name) @import.from)
  source: (string) @import.path) @import.public

; export { A as B } from "m"
(export_statement
  (export_clause
    (export_specifier
      name: (_) @import.name
      alias: (_) @import.alias) @import.from)
  source: (string) @import.path) @import.public

; export * from "m"
;
; The `*` is an anonymous token with nothing to capture, so the specifier is
; the anchor. The `source:` field is required as well as the `.` anchor:
; `export default "hi"` also opens with a string, under the `value` field.
(export_statement
  . source: (string) @import.path @import.from @import.glob) @import.public

; export * as ns from "m"
(export_statement
  (namespace_export (identifier) @import.alias) @import.import
  source: (string) @import.path) @import.public
