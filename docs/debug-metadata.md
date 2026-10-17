# Debug Metadata

`v6asm` can emit DWARF v4 source metadata for relocatable ELF objects and for
direct ROM builds through a companion `ET_EXEC` ELF file.

## Generate Debug Output

Pass `-g` (or `--debug`) together with `-f obj`:

```powershell
v6asm -g -f obj game.asm -o game.o
ld.lld -m elf32v6c -T v6c.ld game.o -o game.elf
llvm-objcopy -O binary game.elf game.rom
```

For a direct ROM build, `-g` preserves the ROM bytes and writes a sibling ELF:

```powershell
v6asm -g game.asm -o game.rom
# -> game.rom and game.elf
```

Use `--debug-elf <path>` to choose the companion location. The companion has
an allocatable `.text` section at the ROM's first emitted address, a local
symbol table for module-level code labels and immutable absolute constants,
and absolute DWARF line addresses.
Its `.text` contents are the same contiguous byte range as the ROM.

Keep `game.elf` for the debugger and use `game.rom` where a raw ROM image is
required. Do not run a debug-section stripping step on the ELF that the
debugger will consume.

## Emitted ELF Data

With `-g`, the object or ROM companion contains these non-allocating DWARF v4
sections:

| Section | Contents |
|---------|----------|
| `.debug_info` | One minimal compilation-unit DIE with constant declarations, abstract `DW_TAG_subprogram` DIEs for invoked macro definitions, and nested `DW_TAG_inlined_subroutine` DIEs for each macro expansion site |
| `.debug_abbrev` | Abbreviations used by that compilation unit |
| `.debug_line` | Instruction source locations plus deterministic directory/file tables |
| `.debug_str` | Compilation-unit strings |

In object mode, `.debug_line` uses `R_V6C_16` relocations against each code
section. Its corresponding `.rela.debug_line` section is present whenever the
input has executable statement rows, and LLD resolves the section-relative
instruction addresses. In a direct-ROM companion, line addresses are already
absolute, so `.rela.debug_line` is omitted.

In object mode, `.debug_info` also carries a `.rela.debug_info` section with
one `R_V6C_32` relocation per section-relative DWARF reference: every
`DW_FORM_strp` string offset against the object's `.debug_str` section symbol,
the `DW_AT_stmt_list` line-table offset against `.debug_line`, the CU-header
`abbrev_offset` against `.debug_abbrev`, and one `R_V6C_16` relocation per
macro inline DIE's `DW_AT_low_pc` against its code section. LLD rebases these
references while concatenating each input object's debug sections, so every
compilation unit's strings, line program, and abbreviation table stay valid in
the merged output. In a direct-ROM companion there is a single compilation
unit whose raw offsets are already final, so `.rela.debug_info` is omitted.

Inspect an object before linking with:

```powershell
llvm-readelf -S -r game.o
llvm-dwarfdump --debug-info --debug-line game.o
```

Inspect the linked result or direct-ROM companion with the same commands
against `game.elf`.

## Source Attribution

The line table contains statement rows only for instructions. Labels, comments,
control directives, data directives, `.storage`, alignment padding, and `.org`
gaps are not breakpoint targets.

- An ordinary instruction maps to its own source file and line.
- Included files have their own line-table file entries. Paths are normalized
  with `/` separators relative to the source project where possible, so files
  with the same basename remain distinct.
- A loop body creates one instruction row per emitted iteration. These rows
  can share a source line while having different linked addresses. The internal
  row model retains the iteration number as expansion provenance.
- A macro-expanded instruction maps to the macro body line it was expanded
  from, at the body statement's own column, and the first instruction of an
  expansion also keeps a statement row at the invocation line and column. A
  breakpoint on the macro call still stops in its generated code, while
  stepping inside the expansion shows the macro definition.
- `.debug_info` models macro provenance the way C compilers model inlining:
  every invoked macro definition gets an abstract `DW_TAG_subprogram` DIE
  (named after the macro, declared at its `.macro` line), and every contiguous
  expansion run gets a `DW_TAG_inlined_subroutine` DIE covering the expanded
  instructions with `DW_AT_call_file`/`DW_AT_call_line`/`DW_AT_call_column`
  pointing at the invocation site. Nested macro invocations produce nested
  inline DIEs, so debuggers can step into macro bodies and show the macro
  invocation chain on the call stack.
- Multiple instructions separated by `\` on one physical source line produce
  separate rows at their respective instruction starts and physical columns.

The human-readable listing has its own presentation rules. In particular, it
may show macro definition lines while the DWARF line table maps generated
instructions to the invocation site. See [Listing File Format](listing.md) for
listing behavior.

## Symbols

For a debug object, module-level labels and immutable absolute constants are
included in `.symtab` as local symbols unless `.globl`, `.global`, or `.weak`
gives them an external binding.
Labels that begin executable instruction ranges use `STT_FUNC`; labels that
begin emitted data or storage ranges use `STT_OBJECT`. Both receive a non-zero
size when the next module label or source range establishes an extent. Existing
section symbols continue to be used for ordinary intra-object relocations.
Direct-ROM companions include the same module-level labels and constants as
local symbols.
Immutable absolute constants are included as `STT_NOTYPE` `SHN_ABS` symbols
with a `DW_TAG_variable` declaration that records their source file, line, and
numeric value. The assembler cannot distinguish address-like constants from
generic numeric constants, so both use this representation.

Scoped `@` labels and macro-generated names are intentionally omitted from the
debugger-facing named-symbol set because their source spelling is not unique.
Use the DWARF line table to locate generated instructions instead.

## Current Limits

- Metadata is DWARF v4 and uses the V6C 16-bit address model.
- The assembler does not parse DWARF. Direct-ROM companions use one contiguous
  `.text` image, so sparse `.org` gaps are represented as zero-filled bytes.
- Link object output with LLD and retain the linked ELF for debugging. For a
  direct ROM build, retain its companion `.elf`; the raw ROM alone has no
  embedded source metadata.
- Debug sections are opt-in. Invocations without `-g` retain the existing
  object and ROM output behavior.