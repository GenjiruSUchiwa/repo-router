; JavaScript and JSX imports, for rr's second extraction pass.
;
; Compiled twice, once per `Lang`, for the reason `TYPESCRIPT_IMPORTS` and
; `TSX_IMPORTS` are two statics: a `tree_sitter::Query` holds the language it
; was built against, and one shared `OnceLock` would hand every file the other
; language's registry slot. Here both slots hold the *same* grammar, so the two
; compilations are identical work — kept apart anyway, because the day a JSX
; grammar diverges from the JavaScript one is not a day to discover this.
;
; This is `typescript-imports.scm` minus one pattern. `import x = require("m")`
; is `import_require_clause`, a TypeScript node; the JavaScript grammar has no
; such node type and `Query::new` rejects the file with it in. Everything else
; is shared syntax and is copied verbatim, so a .js and a .ts file with the same
; import produce the same `Import`.
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
; Deliberately not covered, for `typescript-imports.scm`'s reasons: dynamic
; `import(expr)`, and `export = x`.

; import x from "m"
(import_statement
  (import_clause (identifier) @import.alias @import.import)
  source: (string) @import.path)

; import * as ns from "m"
;
; Not a glob: it binds one name.
(import_statement
  (import_clause (namespace_import (identifier) @import.alias) @import.import)
  source: (string) @import.path)

; import { A } from "m"
;
; `!alias` is load-bearing: without it this also matches `{ A as B }` and the
; declaration lands twice.
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
(import_statement
  . source: (string) @import.path @import.import)

; require("m")
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
(export_statement
  . source: (string) @import.path @import.from @import.glob) @import.public

; export * as ns from "m"
(export_statement
  (namespace_export (identifier) @import.alias) @import.import
  source: (string) @import.path) @import.public
