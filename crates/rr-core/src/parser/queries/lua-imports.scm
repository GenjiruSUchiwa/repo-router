; Lua imports, for rr's second extraction pass.
;
; `require "m"` and `require("m")` are one node: Lua lets a call with a single
; string argument drop its parentheses, and the grammar folds both into
; `function_call` with an `arguments` field.

(function_call
  name: (identifier) @import.callee
  arguments: (arguments . (string (string_content) @import.path))) @import.require
