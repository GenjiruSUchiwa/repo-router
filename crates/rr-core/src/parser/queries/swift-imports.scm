; Swift imports, for rr's second extraction pass.
;
; `import class UIKit.UIView` records `UIKit.UIView` as the path and drops the
; `class` kind modifier: Swift's submodule-kind imports name the same module a
; plain `import` would, and `Import` has no field for the restriction.

(import_declaration (identifier) @import.path) @import.import
