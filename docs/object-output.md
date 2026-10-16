# Object Output (Relocatable ELF)

By default v6asm produces a fully-located flat `.rom` image. With `-f obj`
(also `--format obj`) it instead emits a **relocatable ELF32 object** that can be
linked with `ld.lld` and the V6C linker script, alongside objects produced by
the C toolchain. ROM mode is unchanged; object mode is fully opt-in.

```bash
v6asm main.asm -f obj                 # -> main.o
v6asm main.asm -f obj -o build/x.o    # custom path
```

## When to use it

Use object mode when an assembly module must be **linked** with other code
(C objects, other assembly objects) and the final addresses are chosen by the
linker rather than hard-coded with `.org`. Use the default ROM mode for
standalone Vector 06c programs.

## ELF contract

The emitted object follows the V6C ELF contract:

| Field | Value |
|-------|-------|
| Class | `ELFCLASS32` |
| Data | `ELFDATA2LSB` (little-endian) |
| OS/ABI | `ELFOSABI_NONE` |
| Type | `ET_REL` (relocatable) |
| Machine | `EM_V6C` = `0x8080` |
| `e_flags` | `0` |
| Relocations | `RELA` (explicit addend) |

Inspect an object with the project's LLVM tools:

```bash
llvm-readelf -h -S -s -r main.o
```

## Debug Metadata

Pass `-g` with `-f obj` to emit a minimal DWARF v4 compilation unit and line
table. Debug sections are non-allocating and are omitted unless requested. See
[Debug Metadata](debug-metadata.md) for the full linker workflow, source-line
policy, symbols, inspection commands, and direct-ROM companion workflow.

## Section model

Code, data, and labels are placed into the **active section**. The active
section starts as `.text` and is changed with the section directives:

| Directive | Section | Contents | Flags |
|-----------|---------|----------|-------|
| `.text` (no operand) | `.text` | code | alloc + execute |
| `.data` | `.data` | initialized data | alloc + write |
| `.rodata` | `.rodata` | read-only data | alloc |
| `.bss` | `.bss` | reserved space (no file bytes) | alloc + write, `SHT_NOBITS` |
| `.section <name>` | `<name>` | — | inferred from name |

Each section has its own section-relative location counter starting at 0; the
linker assigns the final base addresses. `.bss` (and any `SHT_NOBITS` section)
reserves space via `.storage` but stores no bytes in the file.

The active section is **stateful**: everything (code, `.storage`, `.byte`,
`.align`, labels, …) flows into whichever section was last selected, and stays
there until the next section directive. Consequences:

- If the source never uses a section directive, the **entire unit is a single
  `.text` section** — the default one created at startup.
- Selecting a section that already exists **resumes and appends** to it; it does
  not create a second section of the same name. So switching to `.data` and
  later back to `.text` continues the original `.text` section.
- `.align` and `.storage` never switch sections; they act on the current active
  section only.

`.org` is **rejected** in object mode — absolute placement is the linker's job.

### Section alignment

Each section records a required alignment (`sh_addralign`, default 1). The
`.align` directive raises the active section's alignment, and an `.align`
placed immediately before a `.section` or `.optional` directive propagates to
the section that directive creates. This lets a generated section (such as a
page-aligned buffer) carry the alignment the linker must honor.

### `.pack` blocks

`.pack` / `.endpack` blocks are runtime-only storage blocks. In object mode
each logical block is emitted as a distinct input section:

| Directive | Input section | Linker constraint |
|-----------|---------------|-------------------|
| `.pack` | `.bss.pack` | May start anywhere and cross a 256-byte boundary. |
| `.pack align` | `.bss.pack.align` | Must start on a 256-byte boundary. |
| `.pack window` | `.bss.pack.window` | Must fit within one 256-byte page. |

Every block section is `SHT_NOBITS`, has `SHF_ALLOC | SHF_WRITE`, alignment 1,
and reserves runtime space without contributing file bytes. Repeated blocks of
the same kind remain distinct section headers even though they have the same
name. Symbols are block-relative, and normal relocations give
`ld.lld --gc-sections` an independent reachability edge for each block.

The V6C linker collects surviving blocks into its dedicated `.bss.pack` output
section and packs them globally across all input objects. It applies anchor and
window constraints using final absolute addresses, after garbage collection.
Custom linker scripts must collect the three exact input names in a dedicated
output section before a broad `.bss.*` pattern. The standard V6C linker script
already does this and keeps the packed arena within `__bss_start` and
`__bss_end`.

The assembler does not define `__PACK_*` analysis constants in object mode,
because only the linker knows the surviving blocks and final arena layout.

For example:

```asm
.pack align
page_table:
  .storage 240
.endpack

.pack window
small_runtime_data:
  .storage 32
.endpack
```

Use `llvm-readelf -S -s -r module.o` to inspect the block sections, symbols,
and relocation reachability edges.


### `.optional` blocks become sections

In object mode, each `.optional` / `.function` block is emitted into its own
section by default, so unused blocks are pruned at link time by `ld.lld
--gc-sections` rather than at assemble time. The section is named after the
first label that is referenced from outside the block (the label that keeps the
block alive):

- a block containing code → `.text.<label>` (alloc + execute);
- a block that only reserves space with `.storage` (no filler) → `.bss.<label>`
  (alloc + write, `SHT_NOBITS`): no file bytes, reserved at run time;
- any other data block → `.data.<label>` (alloc + write), so the data is
  relocatable and overridable.

Control this with `.setting optional`: `sections` (default in object mode) emits
per-block sections, while `prune` restores assemble-time pruning. A block that
defines no label or constant is an error.


## Symbol model

- Defined labels are **local by default** and are not individually exported;
  references to them from the same object are resolved through the containing
  **section symbol** with the target offset folded into the relocation addend.
- `.globl` / `.global` exports a symbol (binding `GLOBAL`) so other objects can
  reference it.
- `.weak` exports a symbol with binding `WEAK` (overridable by a strong
  definition; unresolved weak references link as zero).
- A symbol that is **referenced but never defined** becomes an undefined
  external (`SHN_UNDEF`, binding `GLOBAL`) and must be provided by another
  object at link time.

Exported symbols defined in an executable section get type `FUNC`; in a data
section they get type `OBJECT`. Absolute constants exported with `.globl` are
emitted as absolute (`SHN_ABS`) values.

## Relocatable expressions

In object mode, operands of instructions and `.byte` / `.word` data are
evaluated as **relocatable expressions**. A relocatable expression is at most:

```
[ <byte-op> ] ( <one section/symbol term> + <constant addend> )
```

- A bare constant (e.g. a `=`-defined value, literal, or a difference of two
  labels in the **same** section) is **baked** into the bytes with no
  relocation.
- A reference to a label or external symbol produces a **relocation**; any
  added/subtracted constant becomes the relocation addend (`A` in `S + A`).
- `<(expr)` takes the low byte, `>(expr)` the high byte of the resolved address.
- Differences of symbols in **different** sections, scaling a symbol, or other
  non-linear use of a symbol are errors.

### Relocation types

All relocations are absolute (the linker computes `S + A`); there are no
PC-relative relocations.

| Name | Value | Width | Produced by |
|------|-------|-------|-------------|
| `R_V6C_NONE` | 0 | — | (none) |
| `R_V6C_8` | 1 | 1 byte | 8-bit immediate referencing a symbol |
| `R_V6C_16` | 2 | 2 bytes | 16-bit address operands (`JMP`, `CALL`, `LXI`, `LHLD`, `SHLD`, `LDA`, `STA`, conditional jumps/calls) and `.word` |
| `R_V6C_LO8` | 3 | 1 byte | `<(sym)` low-byte immediate |
| `R_V6C_HI8` | 4 | 1 byte | `>(sym)` high-byte immediate |
| `R_V6C_32` | 5 | 4 bytes | DWARF section-relative references in `.rela.debug_info` (`DW_FORM_strp` → `.debug_str`, `DW_AT_stmt_list` → `.debug_line`, CU `abbrev_offset` → `.debug_abbrev`); not producible from source operands |

`R_V6C_32` mirrors the relocation type the C toolchain already emits in its
own `.rela.debug_info` sections, and `ld.lld` resolves it identically: a
section-symbol relocation becomes the input section's offset inside the merged
(non-allocating) output section plus the addend.

`.dword` remains constant-only; a relocatable `.dword` is an error.

## Example

```asm
.globl interruption

interruption:
        xthl
        shld interruption_return + 1   ; R_V6C_16, addend = offset+1
        call controls_check            ; R_V6C_16 to external (SHN_UNDEF)
        lxi  h, counter                ; R_V6C_16 to .data section symbol
        mvi  a, <(interruption)        ; R_V6C_LO8
        mvi  a, >(interruption)        ; R_V6C_HI8
interruption_return:
        jmp  interruption              ; R_V6C_16

.data
counter:
        .word 0
```

Link it together with other objects and the linker script:

```bash
v6asm interruption.asm -f obj -o interruption.o
ld.lld interruption.o other.o -T v6c.ld -o program.elf
```

## Scope / non-goals (v1)

- No PC-relative, GOT, PLT, or dynamic linking.
- No 32-bit relocations from source operands (`.dword` stays constant-only);
  the only `R_V6C_32` users are the DWARF metadata relocations described above.
- No `sym - sym` across sections — only same-section differences fold.
- Direct-ROM `ET_EXEC` debug companions are available with `-g`; see
  [Debug Metadata](debug-metadata.md) for both direct-ROM and object workflows.
- ROM mode and all existing flags behave identically.
