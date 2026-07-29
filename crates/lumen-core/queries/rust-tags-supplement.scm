; Rust supplement — authored for Lumen, appended to the grammar's own tags.scm.
;
; Upstream treats `impl` blocks as `@reference.implementation`, which is right for what
; upstream is for: indexing symbols so a jump-to-definition can find them. An outline
; needs the opposite relation — the impl is the *scope* a method lives in, and a bare
; `fn new` with no container tells the reader nothing about what it constructs.
;
; Adding the impl as a definition means a type with an inherent impl yields two
; definitions sharing one name, so a reference to that name splits its weight between
; them. That is the correct reading rather than a flaw: `Report` genuinely refers to both
; the type and its behaviour, and the alternative — attributing to whichever the query
; happened to emit first — is the non-reproducible choice the graph rules out.

(impl_item
  type: (type_identifier) @name) @definition.class

(impl_item
  type: (generic_type
    type: (type_identifier) @name)) @definition.class

; `impl Trait for Type` — name the trait, since that is what the block is about.
(impl_item
  trait: (type_identifier) @name
  type: (_)) @definition.class
