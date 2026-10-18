# Directives

All directive names are **case-insensitive**: `.org`, `.ORG`, and `.Org` are equivalent.

## `.org`

Sets the program counter to a specific address. Accepts decimal, `0x..`, or `$..` literals.

```asm
.org 0x100
```

## `.include`

Include another file inline.

```asm
.include "file.asm"
.include 'file.asm'
```

Paths are resolved relative to the including file, the main asm file, any directories passed with `-I` / `--include-dir`, or the current working directory. Includes support recursive expansion up to 16 levels.

## `.pragma once`

Marks the current file so that the preprocessor includes it only once, regardless of how many times it is referenced with `.include`. Subsequent `.include` directives pointing to the same file are silently ignored.

Place it as the first non-comment line of any file that defines macros, constants, or other declarations that must not be emitted twice.

```asm
.pragma once

; v6_macros.asm — safe to include from multiple files
.macro RAM_DISK_OFF()
    mvi a, 0
    out RAM_DISK_PORT
.endmacro
```

Files without `.pragma once` are included every time they appear in an `.include` directive (the traditional textual-paste behaviour).

> **Note:** Deduplication is based on the canonical file path, so two `.include` directives that spell the same path differently (e.g. `./foo.asm` vs `foo.asm`) are correctly treated as the same file.

## `.filesize`

Defines a constant equal to the byte size of a given file. The path resolves relative to the project directory.

```asm
ROM_SIZE .filesize "out/prg.rom" ; ROM_SIZE becomes the size of prg.rom
BufEnd   = BufStart + ROM_SIZE    ; use it in expressions
```

## `.incbin`

Include raw bytes from an external file at the current address.

```asm
.incbin "path"              ; include entire file
.incbin "path", offset      ; start at offset
.incbin "path", offset, length  ; start at offset, read length bytes
```

Paths resolve relative to the project directory. `offset` and `length` are optional expressions (decimal, hex, or binary); omit them to start at 0 and read the entire file.

## `.if` / `.endif`

Conditional assembly. The block assembles only when the expression evaluates to non-zero. Nesting is supported, and the parser short-circuits inactive branches so forward references inside skipped blocks do not trigger errors.

The argument may be a single numeric/boolean literal or any full expression. Expressions support decimal/hex/binary literals, character constants, symbol names (labels, constants, `@local` labels), arithmetic (`+ - * / % << >>`), comparisons (`== != < <= > >=`), bitwise logic (`& | ^ ~`), and boolean operators (`! && ||`).

```asm
Value = 3

.if (Value >= 2) && (SomeFlag == TRUE)
  mvi a, #$00
  sta $d020
.endif
```

## `.loop` / `.endloop`

Repeat a block of source lines a given number of times (maximum: 100,000 per loop). Short form: `.endl`.

Loop counts are evaluated with the same expression engine as `.if`. Loop bodies can nest other `.loop` or `.if` blocks. Constant assignments inside the block execute on each iteration because the assembler expands the body inline:

```asm
Value = 0
Step  = 4

.loop (Step / 2)
  db Value
  Value = Value + 1
.endl
```

The example above emits `Value` three times (0, 1, 2) and leaves `Value` set to 3 for subsequent code.

## `.optional` / `.endoptional`

Defines an optional code block that is automatically removed from output if none of its internal labels and constants are used externally. Short forms: `.opt` / `.endopt`.

A block must define at least one label or constant; an `.optional` block that defines none is an error, since a block with no externally visible symbols can never be referenced.

```asm
.optional
useless_byte:
  db 0       ; removed if useless_byte is never referenced
.endoptional
```

```asm
call useful_routine
.opt
useful_routine:
  mvi a, 1 ; kept because useful_routine label was used
  ret      ; kept because useful_routine label was used
.endopt
```

### Object mode (`-f obj`)

In object mode each `.optional` block is, by default, emitted into its own ELF
section so the linker prunes unused blocks at link time (`ld.lld
--gc-sections`), instead of assemble-time pruning. The section is named after
the **first label that is referenced from outside the block** (the label that
keeps the block alive):

- a block containing code goes into `.text.<label>` (alloc + execute);
- a block that only reserves space with `.storage` (no filler, no other bytes)
  goes into `.bss.<label>` (alloc + write, `SHT_NOBITS`) so it occupies no file
  bytes but is reserved at run time;
- any other data block goes into `.data.<label>` (alloc + write), so the data is
  relocatable and can be overridden.

Use `.setting optional, prune` to fall back to assemble-time pruning in object
mode, or `.setting optional, sections` to force per-section emission. See
[`.setting`](#setting) and [Object Output](object-output.md).


## `.function` / `.endfunction`

This is an alias for `.optional` / `.endoptional`. Short forms: `.func` / `.endfunc`

## `.setting`

Updates assembler defaults using non-case-sensitive key/value pairs. Values may be string, integer, or boolean. Multiple pairs can be specified in one directive.

| Key | Values | Default | Description |
|-----|--------|---------|-------------|
| `optional` | `true` / `false` / `prune` / `sections` | `true` | Controls handling of `.optional` blocks. `false` disables pruning (all blocks kept). `prune` forces assemble-time pruning of unreferenced blocks. `sections` (object mode only) emits each block into its own section for link-time pruning. In object mode the default is `sections`; in ROM mode it is `prune`. |
| `force_once` | `true` / `false` | `false` | Include this file and all files it includes at most once (see below). |

```asm
.setting optional, false          ; disables pruning of .optional blocks
.setting optional, sections       ; emit each .optional block as its own section (obj mode)
.setting force_once, true         ; deduplicate this file and its sub-includes
.setting force_once, true, optional, false  ; multiple pairs in one directive
```

### `force_once`

When a file contains `.setting force_once, true`, the preprocessor treats it
like `.pragma once` — subsequent `.include` directives pointing to the same
file are silently ignored.  In addition, the setting **propagates** to every
file that file includes: all direct and transitive sub-includes are also
deduplicated for the duration of that include tree.

**Priority:** `true` wins over `false` and over the default.  If the same file
is included from multiple modules and at least one of them (or the file itself)
sets `force_once = true`, the file is assembled only once — the first
occurrence is kept and all later ones are skipped.

```asm
; macros.asm — safe to include from multiple files
.setting force_once, true

.macro NOP_()
    nop
.endmacro
```

```asm
; module_a.asm
.include "macros.asm"   ; assembled — macros.asm registered as force-once
```

```asm
; module_b.asm
.include "macros.asm"   ; skipped — already registered
```

The difference from `.pragma once` is one of *declaration point*: `.pragma
once` is declared inside the file itself; `force_once` can also be set by the
**parent** file (`.setting force_once, true` in the file that does the
including), which covers the case where you cannot modify the included file.

## Labels and Constants

### Global Labels

Standard labels defined with a trailing colon. They are visible throughout the entire file and across included files.

### Local Labels and Constants (`@name`)

Locals are scoped between the nearest surrounding global labels (or the start/end of the file/macro/loop expansion). A reference resolves to the closest definition in the same scope, preferring the latest definition at or before the reference; if none, it falls back to the next definition in that scope. Locals are per-file/per-macro and do not collide with globals.

```asm
mem_erase_sp_filler:
  lxi b, $0000
  sphl
  mvi a, 0xFF
@loop:
  PUSH_B(16)
  dcx d
  cmp d
  jnz @loop    ; resolves to @loop above (same scope)

mem_fill_sp:              ; new global label -> new local scope
  shld mem_erase_sp_filler + 1
  ; @loop here would be unrelated to the one above
```

Locals can be redefined in a scope; references before a redefinition bind to the earlier definition, references after bind to the later one. Use globals for cross-scope jumps or data addresses.

Exmple:
```asm
start:

@loop:
  nop
  jmp @loop     ; jumps to the first @loop

  ; ... some code ...

@loop:
  mov a, b
  jmp @loop     ; jumps to the second @loop
```
A reference to @loop looks for the closest matching definition in the current scope:
* If there is a definition before the reference, it uses the most recent previous one.
* there is no previous definition, it may use the next definition ahead in the same scope.

### Constants (`=` / `EQU`)

Defines an immutable constant. Both plain and label-style forms are accepted (e.g., `CONST:` followed by `= expr`). The assembler defers evaluating these expressions until after the first pass, so forward references work. Reassigning a constant with a different value triggers an error; use `.var` for mutability.

```asm
OS_FILENAME_LEN_MAX = BASENAME_LEN + BYTE_LEN + EXT_LEN + WORD_LEN
BASENAME_LEN = 8
BYTE_LEN = 1
EXT_LEN = 3
WORD_LEN = 2
```

Local constants: prefix with `@` to give a constant the same scoped resolution as local labels:

```asm
CONST1: = $2000
@data_end: = CONST1 * 2   ; emits 0x4000 before end_label
...
end_label:
@data_end: = CONST1 * 4   ; emits 0x8000 after end_label
```

### Mutable Variables (`.var`)

Declares a mutable variable whose value can be reassigned later. Unlike `=` or `EQU`, `.var` establishes an initial value but can be updated with direct assignments or a subsequent `EQU`.

```asm
ImmutableConst = 1      ; Initialize constant
            ; Emits: 0x01

Counter .var 10         ; Initialize variable
db Counter              ; Emits: 0x0A

Counter = Counter - 1   ; Update with expression
db Counter              ; Emits: 0x09

Counter equ 5           ; Update with EQU
db Counter              ; Emits: 0x05
```

## `.print`

Emit compile-time diagnostics to the console during the second pass. Arguments are comma-separated and can mix string literals, numeric literals, labels, or arbitrary expressions.

```asm
.print "Copying from", SourceAddr, "to", DestAddr
.print "Loop count:", (EndAddr - StartAddr) / 16
```

Strings honor standard escapes (`\n`, `\t`, `\"`, etc.). Non-string arguments are printed in decimal.

## `.error`

Immediately halt the second pass with a fatal diagnostic. Uses the same argument rules as `.print`. Because inactive `.if` blocks are skipped, `.error` calls inside false branches never trigger.

```asm
MAX_SIZE = 100

.if (BUFFER_SIZE > MAX_SIZE)
  .error "Buffer size", BUFFER_SIZE, "exceeds", MAX_SIZE
.endif
```

## `.align`

Pad the output with zero bytes until the program counter reaches the next multiple of the given value. The argument must be positive and a power of two. If already aligned, no padding is emitted.

```asm
.org $100
Start:
  db 0, 1, 2
.align 16   ; next instructions start at $110
AlignedLabel:
  mvi a, 0
```

`AlignedLabel` is assigned the aligned address ($110) and the gap between `$103` and `$10F` is filled with zeros.

### Object mode

When emitting a relocatable object (`-f obj`), `.align` additionally raises the
enclosing section's required alignment (`sh_addralign`) so the linker preserves
it. A `.align` placed immediately before a `.section` or `.optional` directive
propagates to the section that directive creates, letting you align a generated
section (for example a 256-byte page-aligned buffer):

```asm
.align 256      ; align the section created below
.opt
_v6_gc_buffer:
  .storage 256 * 14, 0   ; -> .data._v6_gc_buffer with sh_addralign = 256
.endopt
```

## `.storage`

Reserves bytes of address space. If `Filler` is provided, the assembler emits that byte `Length` times. If omitted, the bytes are uninitialized (PC advances but nothing is written), useful for runtime buffers.

```asm
.org 0x200
buffer:   .storage 16          ; advances PC by 16, writes nothing
table:    .storage 4, 0x7E     ; emits 0x7E 0x7E 0x7E 0x7E
after:    .db 0xAA              ; assembled after the reserved space
```

## `.pack` / `.endpack`

Declares movable runtime-only storage blocks. A pack block may contain labels,
constant assignments, and `.storage <length>` reservations without a filler.
It may not contain instructions, initialized data, `.align`, `.org`, section
switches, `.optional`, or another `.pack`. Every block must define at least one
label and reserve at least one byte.

The optional keyword selects the placement rule:

| Directive | Placement |
|-----------|-----------|
| `.pack` | Filler block; may be placed anywhere and may cross a 256-byte boundary. |
| `.pack align` | Anchor block; starts at a 256-byte boundary. |
| `.pack window` | Windowed block; must fit entirely within one 256-byte page. Its length must be no greater than 256 bytes. |

```asm
.pack align
room_tiledata:
  .storage 240
.endpack

.pack window
hero_resources:
  .storage 17
.endpack

.pack
runtime_flags:
  .storage 16
.endpack
```

References must use labels rather than relying on source order. In ROM mode,
the assembler reorders blocks: anchors form the aligned skeleton, then
windowed blocks and filler blocks use available holes. In object mode, the
linker performs that reordering after garbage collection.

In ROM mode the arena is reserved at the first `.pack` position, rounded up
to a 256-byte boundary, and later inline content resumes after the arena. In
object mode each block becomes a distinct alignment-1 `SHT_NOBITS` section:
`.bss.pack`, `.bss.pack.align`, or `.bss.pack.window`. This lets
`ld.lld --gc-sections` discard unreferenced blocks independently and lets the
V6C linker pack all surviving blocks across object files using their final
absolute addresses. See [Object Output](object-output.md) for the ELF contract.

### Pack analysis constants

In ROM mode, after laying out pack blocks, the assembler defines these reserved
global constants. They are not defined in object mode because only the linker
knows the post-GC global arena and its final gaps.

| Constant | Meaning |
|----------|---------|
| `__PACK_ARENA_SIZE` | Total reserved size of the pack arena. |
| `__PACK_WASTE_BYTES` | Sum of all unfilled gaps after packing. |
| `__PACK_WASTE_COUNT` | Number of unfilled gaps. |
| `__PACK_WASTE_<n>_OFFSET` | Arena-relative offset of unfilled gap `n`. |
| `__PACK_WASTE_<n>_SIZE` | Length of unfilled gap `n`. |

Gaps are numbered by ascending arena offset. Names beginning with `__PACK_`
are reserved for assembler-generated pack analysis constants.

## `.struct` / `.ends`

Defines a user-defined composite data type (struct). Structs can be expanded
(instantiated), referenced field-by-field with global labels, and inherited
(`extends`). See [Structs](structs.md) for the full specification.

```asm
.struct actor
update_ptr:   word
status:       byte
pos_x:        word
pos_y:        word
.ends
```

A `.struct` block only declares the layout — it emits no bytes. Instances are
created by declaring a label with the struct name:

```asm
hero: actor                        ; uninitialized instance
shield: actor(pos_x = 5, pos_y = 10) ; named initializer form (the only
                                   ; supported form; positional lists are
                                   ; rejected)
char_pool: actor[12]               ; pool of 12 uninitialized instances
```

Field references resolve to the field's absolute address inside the instance
and can be used everywhere a global label can:

```asm
lxi h, hero.pos_x           ; address of hero's pos_x field
lxi h, char_pool[2].status  ; constant-indexed pool reference
```

Every struct definition generates `<name>.<field>` offset constants and a
`<name>.size` constant; every instance and pool generates a `<label>.end`
symbol.

## Data Emission

### `.byte` / `DB`

Emit one or more bytes at the current address. Accepts decimal, hex (`0x`/`$`), or binary (`b`/`%`).

```asm
.byte 255, $10, 0xFF, b1111_0000, %11_11_00_00
```

Emits `FF 10 FF F0 F0`.

### `.word` / `DW`

Emit one or more 16-bit words (little-endian). Negative values down to -0x7FFF are encoded using two's complement.

```asm
.word $1234, 42, b0000_1111, -5
```

Outputs `34 12 2A 00 0F 00 FB FF`.

### `.dword` / `DD`

Emit one or more 32-bit words (little-endian). Negative values down to -0x7FFFFFFF are encoded using two's complement.

```asm
.dword $12345678, CONST_BASE + 0x22, -1
```

Outputs `78 56 34 12 22 00 00 01 FF FF FF FF`.

## `.encoding`

Selects how upcoming `.text` literals convert characters to bytes. Supported types are `"ascii"` (default) and `"screencodecommodore"`. The optional case argument accepts `"mixed"` (default), `"lower"`, or `"upper"`: `"mixed"` leaves every character unchanged; `"lower"` converts `A` through `Z` to `a` through `z`; and `"upper"` converts `a` through `z` to `A` through `Z`. Non-alphabetic characters are unaffected.

```asm
.encoding "ascii", "upper"
.text "hello", 'w'           ; emits: 0x48, 0x45, 0x4C, 0x4C, 0x4F, 0x57

.encoding "screencodecommodore"
.text "@aA"                  ; emits: 0x00, 0x01, 0x41
```

## `.text`

Emits bytes from comma-separated string or character literals using the current `.encoding` settings. Strings honor standard escapes like `\n`, `\t`, `\"`, etc.

```asm
.encoding "ascii"
.text "   address:   1", '\n', '\0'
; emits: 20 20 20 61 64 64 72 65 73 73 3A 20 20 20 31 0A 00
```

## Object-Mode Directives

These directives only take effect when assembling in object mode (`v6asm -f obj`). In ROM mode the section directives are ignored as no-ops where harmless, and binding directives have no effect. See [Object Output](object-output.md) for the full model.

### Section selection

Switch the active output section. Subsequent code, data, and labels are placed in that section. The default section is `.text`.

| Directive | Section | Default flags |
|-----------|---------|---------------|
| `.text` (no operand) | `.text` | alloc + execute |
| `.data` | `.data` | alloc + write |
| `.rodata` | `.rodata` | alloc (read-only) |
| `.bss` | `.bss` | alloc + write, no file contents |
| `.section <name>` | `<name>` | inferred from the name |

```asm
.text
start:    call init
          jmp  start

.data
counter:  .word 0

.bss
buffer:   .storage 64       ; reserves space, occupies no file bytes
```

Note: a bare `.text` switches sections, while `.text "..."` (with operands) is the string-emitting directive described above. `.section` accepts an optional `,"flags",@type` suffix for GNU-assembler compatibility; it is parsed and ignored (flags/type are inferred from the name).

The active section is stateful: content flows into the most recently selected section until the next switch. With no section directive the whole unit stays in the default `.text`, and re-selecting an existing section resumes and appends to it rather than creating a new one.

### `.globl` / `.global`

Mark one or more symbols as global (externally visible). Global symbols are exported in the object's symbol table so the linker can resolve references from other objects.

```asm
.globl interruption, vblank_handler
```

### `.weak`

Mark one or more symbols as weak. A weak definition can be overridden by a strong (global) definition in another object, and an unresolved weak reference links as zero instead of erroring.

```asm
.weak default_handler
```

### `.local`

Mark one or more symbols as local. In object mode, defined labels are local by default — only `.globl`/`.weak` export them — so `.local` is accepted mainly for source compatibility and to make the intent explicit.

```asm
.local scratch_routine
```
