; TypeScript / TSX tag query — authored for Lumen, not copied.
;
; The upstream tree-sitter-typescript `tags.scm` is unusable for outlining real code. It
; captures only declaration forms — `function_signature`, `method_signature`,
; `abstract_class_declaration` — so ordinary `class`, `function`, `method_definition` and
; every call expression produce nothing. Measured on this repository: 0 definitions in a
; 652-line Angular service, 0 in a 57-line component, and the only file that yielded
; anything was one consisting entirely of `interface` declarations.
;
; Written from scratch against the grammar's node names so the project stays MIT-clean;
; the instruction to prefer upstream was about licensing, and copying from an
; Apache-2.0 source is what it ruled out.
;
; Capture names follow the upstream convention exactly — `@name` on the identifier,
; `@definition.<kind>` / `@reference.<kind>` on the enclosing node — so the extractor
; treats all three languages through one code path.

; ── definitions ──────────────────────────────────────────────────────────────

(function_declaration
  name: (identifier) @name) @definition.function

(generator_function_declaration
  name: (identifier) @name) @definition.function

; Declared-only signatures, which is all upstream covered.
(function_signature
  name: (identifier) @name) @definition.function

(class_declaration
  name: (type_identifier) @name) @definition.class

(abstract_class_declaration
  name: (type_identifier) @name) @definition.class

(interface_declaration
  name: (type_identifier) @name) @definition.interface

(enum_declaration
  name: (identifier) @name) @definition.class

(type_alias_declaration
  name: (type_identifier) @name) @definition.type

(method_definition
  name: (property_identifier) @name) @definition.method

(method_signature
  name: (property_identifier) @name) @definition.method

(abstract_method_signature
  name: (property_identifier) @name) @definition.method

; `const handler = () => {}` and `const f = function () {}`. Extremely common in
; TypeScript and entirely absent upstream.
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function

; Angular-style class fields holding a function: `onClick = () => {}`.
(public_field_definition
  name: (property_identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.method

; Everything else declared at class level is state, not behaviour, but it is part of the
; type's shape and worth ranking below methods.
(public_field_definition
  name: (property_identifier) @name) @definition.constant

(internal_module
  name: (identifier) @name) @definition.module

; ── references ───────────────────────────────────────────────────────────────

(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (member_expression
    property: (property_identifier) @name)) @reference.call

(new_expression
  constructor: (identifier) @name) @reference.class

(type_annotation
  (type_identifier) @name) @reference.type

(extends_clause
  value: (identifier) @name) @reference.implementation

(implements_clause
  (type_identifier) @name) @reference.implementation
