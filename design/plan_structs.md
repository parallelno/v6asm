# Plan: `.struct` — user-defined data structures for v6asm

## Status

**Proposed.** Spec frozen after the design discussion of 2026-10-17; not yet
implemented. Every decision in this document was explicitly agreed; items
deliberately excluded are listed under *Out of scope*.

## Goal

Replace the hand-maintained struct layouts in the v6gel engine — offset chains
in `engine/actor/v6_actor_consts.asm`, the `char_* = actor_*` alias block in
`engine/char/v6_char_consts.asm`, and the duplicated `.storage` field list of
`hero_runtime_data` in `engine/v6_runtime_data.asm` — with a single
assembler-level definition that generates offset constants, instance
reservations, and debugger-visible DWARF. The design reuses existing
infrastructure wherever possible: pack-block rules, data-directive emission,
symbol/visibility rules, and the DWARF structure shapes that the v6vscode
debugger already parses for C.

## Summary example

```asm
.struct actor
update_ptr:             word    ; .word function pointer for updating logic
draw_ptr:               word    ; .word function pointer for drawing
status:                 byte    ; .byte what the actor is doing
status_timer:           byte
anim_timer:             byte
anim_ptr:               word
erase_scr_addr:         word
erase_scr_addr_old:     word
erase_wh:               word
erase_wh_old:           word
pos_x:                  word
pos_y:                  word
speed_x:                word
speed_y:                word
.endstruct
; generated:  actor.update_ptr = 0 ... actor.speed_y = 23, actor.size = 25

.struct char extends actor
data_next_ptr:          word    ; NULL if last actor in the list
impacted_ptr:           word
id:                     byte
type:                   byte
health:                 byte
.endstruct
; generated:  char.data_next_ptr = 25 ... char.health = 32, char.size = 33
; inherited:  char.pos_x == actor.pos_x == 17

hero:      char                     ; uninitialized instance (see Semantics)
sword:     char(health=5, type=CHAR_TYPE_ALLY)  ; named init, bytes emitted
char_pool: char[12]                 ; typed pool: 12 uninitialized instances

lxi h, hero.pos_x           ; absolute address of hero's pos_x field
lxi h, char.health          ; offset constant — pointer-based field access
lxi h, char_pool[2].health  ; constant-index static reference
```
## Model (locked)

### Struct definition

```
.struct <name> [extends <parent>]
<field>*
.ends                 ; '.endstruct' is also accepted; both spellings are
                      ; first-class and both are documented
```

- **Field line:** `<name>: <type>` — exactly one value per field; multi-value
  fields are forbidden (split them).
- **Field types:** `byte | DB`, `word | DW`, `dword | DD` — all six spellings
  are accepted, case-insensitively, mapping to 1/2/4 bytes. These reuse the
  existing data-directive alias set (`parser.rs`: `BYTE|DB`, `WORD|DW`,
  `DWORD|DD`).
- **No default values, no `?`.** Field definitions carry no initializer. This
  keeps uninitialized instances expandable to pure `.storage`, which is what
  makes `.pack` compatibility free.
- **Inheritance (`extends`):** flattened. The parent's fields come first with
  their original offsets; the child's fields continue after the parent's size.
  Inherited fields keep their names: `char.pos_x == actor.pos_x`. This matches
  the engine's polymorphic use (actor-list nodes treated as actor layout).
  Single inheritance only; a parent must be defined before the child.
- **Offsets are cumulative byte sums.** No padding, ever — consistent with the
  V6CLANG data layout (`e-p:16:8-i1:8-i8:8-i16:8-i32:8-i64:8-n8:16-S8`), where
  every type is 8-bit aligned.
- **Allowed inside the body:** field lines, comments, `.if/.endif`,
  `.loop/.endloop`, `.error`, `.print` (all preprocessor-level; they expand
  before the struct is parsed). Nothing else.
- **Forbidden inside the body:** assembly instructions (asm code); any
  byte-emitting or space-reserving directive — `.byte`/`.db`, `.word`/`.dw`,
  `.dword`/`.dd`, `.text`, `.incbin`, `.storage`, `.filesize` — in their
  standalone directive form; `.org`, `.pack`/`.endpack`, `.macro`
  definitions **and invocations**, `.align`, section switches (`.section` /
  `.text` / `.data` / `.bss`), `.optional`, and labels. The size keywords
  (`byte`/`db`/`word`/`dw`/`dword`/`dd`) are valid **only** in the
  `<name>: <type>` field-line position — a standalone data directive inside
  the body is an error, not a field.
- **A struct definition emits no bytes** and may therefore appear anywhere,
  including inside a `.pack` block.
- **Ordering:** a struct must be defined before first use (same discipline as
  macros). A duplicate `.struct <name>` is an error (rely on `.pragma once`
  for shared definition files).
- **Case-insensitivity:** struct, field, and instance names follow the
  existing case-insensitive identifier rule.

### Generated symbols

Every struct definition automatically generates:

- `<name>.<field>` — the byte offset of each field (including inherited
  fields, reachable via the child's name). These are ordinary immutable
  absolute constants.
- `<name>.size` — total byte size (with `extends`: parent size + own fields).

Reserved **field names**: `size` and `end` (both would collide with generated
symbols). No other name is reserved: because a field name only ever appears
after a dot (`char.a`, `char.hl` are unambiguous), register names and
mnemonics are legal field names.

### Instances

```asm
label: name                      ; uninitialized instance
label: name(f1=v1, f2=v2)        ; named-form initialization
label: name[N]                   ; pool of N instances (uninitialized only)
```

- **Uninitialized instance** (`label: name`) expands to the label plus a
  `.storage name.size` reservation — pack-compatible, emits nothing.
- **Named-form init** (`label: name(health=5, ...)`) — the only initializer
  form (positional is rejected). Unnamed fields initialize to **0**, so output
  is deterministic. Bytes are emitted at assembly time and are part of the
  output image: `.data.<label>` sections in object mode (participating in
  `--gc-sections` like any other data), image bytes in ROM mode.
  **Forbidden inside `.pack`** (pack blocks never emit bytes).
- **Empty initializer list** (`label: name()`) is an **error**:
  *"empty initializer list; write `name` for an uninitialized instance
  (contents undefined; zeroed by runtime startup)"*. Rationale: in C/C++,
  `T()` value-initializes (means "zeroed"); accepting the spelling would
  invite exactly that misreading, while the spec guarantees the opposite.
- **Pools** (`label: name[N]`) reserve `N × name.size` bytes, uninitialized
  only. The count must be a constant expression. No per-element initializer
  lists (ROM-resident initialized arrays of structs are not needed).
- **Auto-generated `label.end`** for every instance and pool: the address one
  past the last byte. Replaces hand-written `hero_runtime_data_end:` /
  `@data_end:` boilerplate. (For a raw `.storage name.size * N` reservation
  there is no auto end label — the assembler cannot know the `.storage`
  argument means "N structs"; such pools keep hand-written end labels or
  `label + name.size * N` arithmetic.)

### References

- **`label.field`** — the address of an instance's field. It is effectively a
  global label and can be used everywhere a normal label can. `label` alone
  points to the first field of the instance.
- **`pool[k].field`** — constant-index static reference:
  `pool + k × name.size + name.field`. The index `k` may be any expression
  the assembler's expression engine accepts — numeric literals, constants
  defined with `=` / `EQU`, previously defined labels, and any combination
  using the full operator set — provided it resolves to a known constant
  value at the point of use (the same rule as other constant contexts such as
  `.storage` arguments and `.if`; a forward reference to a value that is not
  yet known at that time is an error). An out-of-range (or negative) resolved
  index is a compile-time error since the count is known. Runtime-indexed
  element access stays pointer-based, exactly as the engine does today with
  `name.size` and the offset constants.

- **Pool base references:** a pool's label points at the first field of
  element 0, so `label.field` on a pool ≡ `label[0].field`.
- **Dot-only.** No underscore-separated aliases are generated.

### Naming collisions (locked)

- A `.struct` name must not collide with any existing label or constant, and a
  label/constant defined later must not collide with any struct name — error
  in both directions. Struct names effectively occupy their own reserved
  prefix in the global namespace.
- This rule is what makes references unambiguous by construction: since a name
  is either a struct name or an instance label, never both, `name.field`
  (offset constant) and `label.field` (address) can never be confused — even
  though both are grammatically identical.

### Visibility and scoping

- Instances obey ordinary label rules: global by default, subject to
  `.global` / `.weak`, visible across files — including instances inside
  `.pack` blocks (like `hero_runtime_data` today).
- Generated symbols are ordinary constants and follow the same globbing rules
  as constants defined at that point (v6gel's `.global *` covers them
  automatically).
- Struct definitions are usable everywhere after their point of definition,
  regardless of the file they were defined in.

## Semantics: reservations are not initialization

An uninitialized instance or pool is a **reservation only**; its contents are
undefined until runtime code initializes them. In practice, runtime startup
zeroes these regions:

- in C programs, `crt0.s` (v6clang, `compiler-rt/lib/builtins/v6clang/`)
  zeroes `[__bss_start, __bss_end)`;
- in the v6gel engine, `engine/v6.asm` `_start` zeroes the engine's
  `__bss` range (lines 80–107).

In object mode, uninitialized instances land in `SHT_NOBITS` `.bss.*` sections
collected by LLD into the range those symbols delimit — so both toolchains
share one zeroing contract, and `.struct` creates no new runtime obligation.

## DWARF

Emission reuses the existing `.debug_info` / `.rela.debug_info` plumbing
(`object/dwarf.rs`); no new relocation kinds are needed:

- Per struct: one `DW_TAG_structure_type` DIE (`DW_AT_name`,
  `DW_AT_byte_size`) with `DW_TAG_member` children (`DW_AT_name`,
  `DW_AT_data_member_location` in constant form, `DW_AT_type` reference).
- Per instance: one `DW_TAG_variable` with the struct type and its address.
- Per pool: `DW_TAG_variable` with a `DW_TAG_array_type` (element = the
  struct, `DW_TAG_subrange_type` count = N).

These are exactly the DIE shapes v6clang emits for C and that
`v6vscode/src/debug/metadata/dwarf-types.ts` already parses, so:

- the **Variables pane** shows instances as expandable structures (members by
  name, values read live at `base + member.offset`) and pools as indexed
  element lists (`indexedVariables`), with zero extension changes;
- **Watch/hover expressions** such as `hero.pos_x` or `char_pool[2].health`
  evaluate through the existing `c-expression-service` member/index nodes.

Static `pool[k].field` references need no additional DWARF — they resolve to
ordinary symbols.

## Implementation notes (v6asm)

- **Lexer / preprocessor:** identifiers may contain `.` after the first
  character. Update `is_ident_char` in `lexer.rs` **and** the preprocessor's
  copy in `preprocessor.rs` (used for macro-parameter word-boundary checks) so
  substitution behaves consistently inside dotted names. Directives start
  with `.`, so a non-leading dot cannot collide with directive dispatch;
  hex-suffix and binary-prefix disambiguation are unaffected.
- **Symbol keys:** dotted names are single symbols end-to-end (expression
  parser, symbol table, ELF `.symtab` — dotted symbol names are legal ELF —
  DWARF names, listing).
- **Instance expansion reuses existing primitives:** uninitialized →
  `label:` + `.storage name.size`; initialized → `label:` + `db`/`dw`/`dd`
  values in field order; pool → `label:` + reservation of `N × name.size`.
  No new emission machinery.
- **Generated constants** are recorded like other immutable absolute
  constants (`STT_NOTYPE` `SHN_ABS` + `DW_TAG_variable` per the existing
  constant rule); instance labels get `STT_OBJECT` with size where the extent
  is known.
- **Diagnostics** (all at the definition/use site, with the usual location
  reporting): duplicate struct name; struct name vs label/constant collision
  (both directions); unknown struct or field; `name()` empty initializer;
  initialized instance inside `.pack`; pool count not constant;
  `pool[k]` index not resolvable to a known constant at the point of use or
  out of range; reserved field name (`size`, `end`); extends of an
  undefined struct; positional initializer (rejected form); instruction or
  standalone data/reserving directive inside the struct body.
- **Listing** shows the original source lines; expansion is internal.

## v6gel migration sketch

1. Define `actor` and `char extends actor` in the consts files (replacing the
   hand-written offset chains and the 14 alias lines).
2. Mechanical rename across engine sources: `actor_update_ptr` →
   `actor.update_ptr`, etc. Generated constants keep the same values, so
   pointer-based list walking, sorting, and collision code are unchanged.
3. Replace the `hero_runtime_data` `.storage` field list with
   `hero: char`; replace the `CHAR_RUNTIME_DATA_LEN > $100` guard with
   `.if char.size > $100`.
4. Reserve the actor pool as `char_pool: char[N]` (optional; raw
   `.storage char.size * N` remains valid).

## Out of scope (v2 candidates)

- Nested struct-typed fields (a field whose type is another struct).
- Initialized pools / per-element initializer lists.
- A `sizeof()` expression operator (`name.size` covers the need).
- Runtime-indexed `label[reg].field` references (impossible to resolve to a
  static symbol; pointer arithmetic + `name.size` covers it).
- A C-header → `.struct` generator for v6clang interop (v6clang already emits
  correct `DW_TAG_structure_type` metadata with cumulative byte offsets; a
  generator would read that DWARF and emit `.struct` blocks. Not needed now;
  the `.struct` syntax doubles as the interchange format when it is).

## Test plan (sketch)

- Golden assembler cases: definition + generated constants; `extends` offset
  inheritance; uninitialized / named-init / pool instances in ROM and object
  modes; pack acceptance and rejection rules; `label.field` and
  `pool[k].field` resolution (index as literal, constant, and compound
  expression; forward-reference and out-of-range index errors); body
  acceptance/rejection (field lines vs. instructions and standalone
  `.storage`/`.db`-style directives); every diagnostic listed above
  (including `char()` and both-direction name collisions).
- DWARF tests: `llvm-dwarfdump` shows structure/member/variable/array DIEs
  with correct byte sizes, offsets, and counts; v6vscode debugger parses them
  unchanged (existing `dwarf-types.ts` fixtures).
- Regression: macro substitution inside dotted identifiers; `.pragma once`
  guarded definition files; `.global *` interaction.



