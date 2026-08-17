; C includes, for rr's second extraction pass.
;
; Captures:
;   @import.include  the anchor; its span becomes Import::span
;   @import.path     the header, as the file wrote it
;
; A system include keeps its angle brackets and a local include loses its
; quotes. That asymmetry is deliberate. `unquote` strips `"`, `'` and backtick
; and nothing else, so `<stdio.h>` arrives intact — and the brackets are the
; only surviving record of which search path the file asked for, which `Import`
; has no field to hold. Normalising them away would make `#include <string.h>`
; and `#include "string.h"` indistinguishable, and those are two different files.
;
; Deliberately not covered: `#include MACRO`, whose `path` field can be an
; `identifier` or a `call_expression`. What it expands to is a preprocessor
; question, and recording the macro's name as a header would be a path that
; names no file.

(preproc_include
  path: [(string_literal) (system_lib_string)] @import.path) @import.include
