# Structs

User-defined composite data types. A struct can be **expanded** (instantiated as
a global label), **referenced** (field-by-field with global labels), and
**inherited** (`extends`).

The struct system is designed to mirror the C structs produced by `v6clang`, so
mixed C/asm projects can share layout definitions 1:1, and the v6vscode
debugger already renders them.

## Definition

A struct is defined with the `.struct` / `.ends` block. It is a declaration
only: a `.struct` block contributes no bytes to the output until instances of
it are declared.

```asm
.struct <struct_name>
<field_name>: <type>   ; optional inline comment
.ends
```

- `struct_name`: the name of the struct (case-insensitive). It must not collide
  with any existing or future global label, constant, or other struct name.
- `field_name`: the name of the field (case-insensitive). Fields are addressed
  as `<instance_label>.<field_name>` after an instance is declared. The field
  names `size` and `end` are reserved (they would collide with generated
  symbols).
- Fields have no per-field default values. Initialization happens at instance
  declaration (see [Initialization](#initialization)).
- The block is closed with `.ends`; `.endstruct` is an accepted alias, and both
  spellings are documented.

**Allowed directives inside the body:** comments, `.if`/`.endif`,
`.loop`/`.endloop`, `.error`, `.print` (all compile-time, preprocessor-level).

**Forbidden inside the body:** assembly instructions, `.byte`/`.db`,
`.word`/`.dw`, `.dword`/`.dd`, `.storage`, `.org`, `.pack`/`.endpack`, macro
definitions or invocations, section switches, `.align`, and labels.

```asm
.struct actor
update_ptr:   word
status:       byte
pos_x:        word
pos_y:        word
.ends
```

## Field Types

The following size types are supported. Aliases are equivalent:

| Type   | Alias | Size (bytes) | Meaning |
|--------|-------|--------------|---------|
| `byte` | `DB`  | 1            | Single byte |
| `word` | `DW`  | 2            | 16-bit word (little-endian) |
| `dword`| `DD`  | 4            | 32-bit dword (little-endian) |

## Initialization

Struct instances are declared as global labels. Initialization depends on
whether an initializer list was supplied at the declaration.

- **Uninitialized instance**: reserves `struct_name.size` bytes like
  `.storage <N>`. Contents are undefined until runtime code initializes them;
  in practice `crt0.s` (v6clang programs) or `v6.asm` (v6gel engine) zeroes
  these regions at startup. This matches how v6gel allocates its
  `hero_runtime_data` / actor pools today.
- **Initialized instance**: a named initializer list emits bytes at assembly
  time; fields not listed are zero-initialized.

```asm
; Uninitialized instance: reserves actor.size bytes, nothing is emitted.
hero: actor

; Named initializer form: the only supported initializer form. Positional
; value lists are rejected. Fields not listed are zero-initialized.
sword: actor(pos_x = 5, pos_y = 10)
```

An empty initializer list (`label: name()`) is an error; write `label: name`
for an uninitialized instance.

Because a struct instance is a global label, it is visible everywhere labels
are, including across files (via `.global` / `.weak`) and inside `.pack` blocks
(matching how v6gel defines `hero_runtime_data` today).

> Note: a runtime-zeroed instance is a **reservation only**. The assembler
> does not guarantee the bytes contain zero at runtime; it only guarantees the
> address space is reserved. Both the v6gel engine and the v6clang `crt0`
> already share this contract: BSS-like reservations are zeroed by runtime
> startup before main.

## Referencing

A field reference `<struct_label>.<field_name>` resolves to the absolute
address of that field inside the instance:

```asm
lxi h, hero.pos_x           ; hl = address of hero's pos_x field
lxi h, sword.update_ptr     ; hl = address of sword's update_ptr field
```

- A reference is effectively a **global label**; it can be used everywhere a
  normal label can (immediate operands, `.db`/`.dw`/`.dd`, `.if`, `.align`,
  `EQU`-style constants, etc.).
- The instance label alone points to the first field of the struct.

Constant-index static references into a typed pool are also supported:

```asm
lxi h, char_pool[2].health  ; pool base + 2 * char.size + char.health
```

The index must resolve to a known constant value at the point of use
(same rule as other constant contexts); a forward reference is an error, and an
out-of-range or negative resolved index is a compile-time error since the pool
count is known. Runtime-indexed element access remains pointer-based, exactly
as the engine does today with `name.size` and the offset constants.

## Inheritance (`extends`)

A struct may extend another struct. Inherited fields keep their original
offsets and names; new fields are appended:

```asm
.struct char extends actor
health:       byte
type:         byte
data_next_ptr: word
impacted_ptr:  word
.ends
```

The resulting layout is a strict superset of the parent:

```
char: [ update_ptr(2) status(1) pos_x(2) pos_y(2) health(1) type(1)
        data_next_ptr(2) impacted_ptr(2) ] = 13 bytes
```

## Size Constants

Every struct definition automatically defines reserved constants:

| Symbol         | Meaning                                   |
|----------------|-------------------------------------------|
| `<name>.<f>`   | Byte offset of field `f` inside the struct |
| `<name>.size`  | Total size of the struct in bytes          |

These are plain immutable constants; they participate in normal constant
resolution and are covered by `.global *`.

```asm
; auto-generated from .struct actor:
;   actor.update_ptr = 0
;   actor.status     = 2
;   actor.pos_x      = 3
;   actor.pos_y      = 5
;   actor.size       = 7
```

Every instance and pool also gets auto-generated `<label>.size` and
`<label>.end` symbols:

| Symbol          | Meaning                                            |
|-----------------|----------------------------------------------------|
| `<label>.size`  | Total byte count of the instance (count × `<name>.size` for pools) |
| `<label>.end`   | Address one past the instance's last byte          |

`<label>.size` is a plain absolute constant (a length, not an address), so
`pool.size` equals `pool.end - pool`.

The field names `size` and `end` are reserved: `size` would collide with the
generated struct size constant, and `end` with the generated instance end
symbol.

## Pools

A pool of struct instances is declared with the array form:

```asm
char_pool: char[12]   ; 12 uninitialized instances, 12 * char.size bytes
```

- Pools are **uninitialized only** — no per-element initializer lists.
  ROM-resident initialized arrays of structs are not supported.
- The count must be a constant expression.
- The pool label points at the first field of element 0, so `label.field` on a
  pool is equivalent to `label[0].field`.
- The pool defines the auto `label.size` (total bytes, `N * <name>.size`) and
  `label.end` (one past the last byte) symbols.
- The pool emits a DWARF array type (`DW_TAG_array_type` with
  `DW_TAG_subrange_type`), so the v6vscode debugger shows it as an indexed
  list of expandable structs.

Pools are pack-compatible like any uninitialized instance and are measured
like `.storage` for `.pack` placement.

## DWARF Debug Information

With `-g`, struct definitions and instances are emitted into `.debug_info`:

- one `DW_TAG_structure_type` per struct (name + byte size) with one
  `DW_TAG_member` child per field (name, byte offset, base type
  `byte`/`word`/`dword`);
- one `DW_TAG_variable` per instance, typed by its struct;
- pools emit a `DW_TAG_array_type` (element = the struct,
  `DW_TAG_subrange_type` with count N), so the debugger shows them as indexed,
  expandable element lists;
- generated offset constants (`name.<field>`, `name.size`) are recorded like
  other immutable absolute constants.

These are exactly the DIE shapes `v6clang` emits for C structs and that the
v6vscode debugger already renders: instances appear as expandable structures
(members listed by name, values read live at `base + member.offset`), and
pools as indexed lists in the Variables pane.

## Limitations

- A `.struct` block may not contain any byte-emitting or space-reserving
  directives (see [Definition](#definition) for the exact whitelist).
- A struct name must not collide with any label, constant, macro, or other
  struct name; conversely, a label or constant defined later must not collide
  with a struct name. The assembler rejects both directions.
- A struct definition must appear before any instance of it is expanded.
- Pool indices and pool counts must be resolvable to a known constant value at
  the point of use.
- Field initializers must use the named form (`pos_x = 5`) and must match
  declared field names; duplicate initializers for the same field are an
  error, and positional value lists are rejected.
