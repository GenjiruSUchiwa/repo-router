; Java tags query, derived from tree-sitter-java's but not pinned to it.
;
; Upstream captures three constructs — class, interface, method — and stops.
; Everything below the first blank line is upstream's, unchanged. Everything
; after it is what upstream leaves out and a Java reader still expects to find:
;
; - A constructor. `method_declaration` does not match one, so upstream's query
;   answers "no definitions" for a class that only declares constructors, and a
;   search for the member that builds an instance finds nothing in any Java
;   repository.
;
; - An `enum`. A Java enum is a top-level nominal type like a class, and it went
;   entirely unindexed: neither the type nor anything it declares was reachable.
;
; - A `record`. Same, and it carries its components in its header, so the
;   signature the extractor already stores is the whole shape of the type.
;
; Enum constants are deliberately not captured. They are implicitly public,
; static and final, but they state no modifier, so `java_visibility` would file
; every one of them under `Visibility::Package` — a wrong answer is worse here
; than a missing one, and reading the enclosing declaration is beyond what
; `refine` can see.

(class_declaration
  name: (identifier) @name) @definition.class

(method_declaration
  name: (identifier) @name) @definition.method

(method_invocation
  name: (identifier) @name
  arguments: (argument_list) @reference.call)

(interface_declaration
  name: (identifier) @name) @definition.interface

(type_list
  (type_identifier) @name) @reference.implementation

(object_creation_expression
  type: (type_identifier) @name) @reference.class

(superclass (type_identifier) @name) @reference.class

(constructor_declaration
  name: (identifier) @name) @definition.constructor

(enum_declaration
  name: (identifier) @name) @definition.enum

(record_declaration
  name: (identifier) @name) @definition.record
