; Ruby imports, for rr's second extraction pass.
;
; Captures:
;   @import.require  the anchor; its span becomes Import::span
;   @import.callee   guard: dropped unless the text is one of RUBY's callee_names
;   @import.path     the argument, quotes stripped
;
; Ruby has no import statement. `require` and `require_relative` are ordinary
; method calls on Kernel, which is why this is a call pattern with a callee
; guard rather than a declaration pattern, and why a file that defines its own
; `require` gets its calls recorded as imports. That is the same trade
; `typescript-imports.scm` makes for CommonJS, and it is the right one: the
; alternative is resolving `Kernel`, which the tags tier does not do.
;
; Deliberately not covered: `load`, `autoload`, and `require` with a non-literal
; argument. The first two have different semantics from `require`, and the third
; names a module rr would have to evaluate Ruby to learn.

(call
  method: (identifier) @import.callee
  arguments: (argument_list . (string (string_content) @import.path))) @import.require
