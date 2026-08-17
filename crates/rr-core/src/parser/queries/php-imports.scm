; PHP imports, for rr's second extraction pass.
;
; Captures:
;   @import.from / @import.require   the anchor; its span becomes Import::span
;   @import.path     the namespace prefix, or the required file
;   @import.name     the leaf the clause selects
;   @import.alias    the local name this file chose
;
; Every namespace form is `ImportKind::Import` or `From`, never
; `ImportKind::Use`, however much `use App\Contracts\Runner;` looks like Rust.
; `ImportKind::resolves_by_path` answers `true` for `Use` alone, and
; `index::build`'s resolver splits a path on `::` and rejoins it onto the
; importing file's module path. A PHP path separated by `\` put through that
; resolver would not fail — it would *find* an unrelated local symbol, which is
; the failure `Import::path` is documented against.
;
; The four require/include forms are one kind. `require` aborts and `include`
; warns when the file is missing, and `_once` deduplicates; none of the three
; differences is about *which* file is named, which is all `Import` records.
;
; Deliberately not covered: `use A\B as C;` where the clause names no leaf
; separately (a single-segment `use Foo;`), and trait `use` inside a class body,
; which imports methods into a type rather than names into a file.

; use App\Contracts\Runner;
(namespace_use_declaration
  (namespace_use_clause
    (qualified_name
      prefix: (namespace_name) @import.path
      (name) @import.name) !alias) @import.from)

; use App\Contracts\Other as Alias;
(namespace_use_declaration
  (namespace_use_clause
    (qualified_name
      prefix: (namespace_name) @import.path
      (name) @import.name)
    alias: (name) @import.alias) @import.from)

; use App\{One, Two};
(namespace_use_declaration
  (namespace_name) @import.path
  body: (namespace_use_group
          (namespace_use_clause (name) @import.name) @import.from))

; require 'boot.php'  /  require_once  /  include  /  include_once
;
; Four patterns and not one bracketed alternation: an alternation at a pattern's
; root cannot carry a child, and the bracketed spelling compiles cleanly, matches
; nothing, and takes a golden down with it.
(require_expression [(string) (encapsed_string)] @import.path) @import.require
(require_once_expression [(string) (encapsed_string)] @import.path) @import.require
(include_expression [(string) (encapsed_string)] @import.path) @import.require
(include_once_expression [(string) (encapsed_string)] @import.path) @import.require
