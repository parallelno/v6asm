# v6asm directive path resolution

**Status:** Draft; not implemented.
**Repository:** `C:\Work\Programming\v6asm`.

## 1. Goal

Give `.include`, `.incbin`, and `.filesize` the same path lookup, label support,
and string handling in v6asm.

- **Source directory:** directory of the source file containing the directive.
- **Compilation directory:** assembler process current working directory (CWD).
- **Entry source:** top-level assembly file passed to v6asm.
- **Include directory:** a directory supplied through `-I` / `--include-dir`.
- **Operand:** the directive's path after decoding its double-quoted string.

## 2. Current behavior

- `C:\Work\Programming\v6asm\crates\v6_core\src\preprocessor.rs`:
  `resolve_include_path` tries the containing source directory, `project_dir`,
  each `-I` directory, then process CWD.
- `C:\Work\Programming\v6asm\crates\v6asm\src\main.rs`:
  `cmd_assemble` sets `project_dir` to the entry source's parent directory.
- `C:\Work\Programming\v6asm\crates\v6_core\src\assembler.rs`:
  `resolve_file_path`, used for `.incbin` and `.filesize`, tries only `project_dir`
  and process CWD.
- The include preprocessor accepts `.include` / `.INCLUDE` without a label prefix.
  It accepts either quote style and removes quotes without decoding escapes.
- `C:\Work\Programming\v6asm\crates\v6_core\src\parser.rs` handles `.incbin`
  and `.filesize` case-insensitively, with label support and double-quoted strings.

## 3. Path lookup

An absolute operand is used as-is. If it does not name an existing file, resolution fails.

For a relative operand, try these paths in order. Use the first existing file:

1. `join(source_directory, operand)`.
2. `join(include_directory, operand)` for each `-I` directory, in command-line order.
3. `join(compilation_directory, operand)`.

Absolute `-I` directories are unchanged. Relative `-I` directories are joined to the
compilation directory.

Example: CWD `/project`, source `/project/src/main.asm`, `-I libs`, operand `common.inc`:

```text
/project/src/common.inc
/project/libs/common.inc
/project/common.inc
```

If none exists, report the unresolved operand at its directive's source location.

## 4. Directive syntax (target)

All three directives are case-insensitive and accept labels:

```asm
part: .include "part.asm"
data: .incbin "assets/data.bin", 0, 16
size_label: SIZE .filesize "assets/data.bin"
```

For label-prefixed `.include`, preserve the label before the expanded source so it
still defines an address at that position. Preserve `NAME .filesize "path"` syntax.

All three path operands are double-quoted strings using the lexer's escape decoding
in `C:\Work\Programming\v6asm\crates\v6_core\src\lexer.rs`. Single-quoted paths
are rejected. Reuse the same path-string parsing for preprocessing and assembly.
Comments outside strings (`;`, `//`, and block comments) are ignored.

## 5. Implementation

- Capture compilation CWD, entry source, and ordered include directories once in
  `cmd_assemble`, then pass them to the core.
- Use one resolver for includes, original-source collection, and both assembly passes
  handling `.incbin` and `.filesize`.
- Supply the containing source directory to binary directives. `SourceLine.file`
  currently stores paths relative to `project_dir`; retain that base when resolving them.
- For a directive written in a macro body, use the macro definition file's directory.
  `SourceLine.origin` records the body location; `SourceLine.file` records the invocation.
- Apply the same rules to CLI and core-library callers.

## 6. Extracted DWARF-writer changes

A compilation unit (CU) represents an assembly invocation in DWARF. The following
writer changes are extracted from the original design and are separate from path lookup.

Current writer code:

- `C:\Work\Programming\v6asm\crates\v6_core\src\output.rs` passes
  `asm.project_dir()` as the compilation directory for object and direct-ROM debug output.
- `C:\Work\Programming\v6asm\crates\v6_core\src\object\dwarf.rs` chooses
  the first sorted source filename as `DW_AT_name`.

Proposed fields:

- `DW_AT_comp_dir`: absolute compilation CWD.
- `DW_AT_name`: actual entry source, relative to CWD when possible, otherwise absolute.
- `.debug_line`: resolved source locations, with relative directories based on CWD.

## 7. Tests and migration

Test:

- Search precedence with duplicate names in the source directory, ordered `-I`
  directories, and CWD; relative/absolute `-I`; absolute hit/miss; missing operands.
- CLI invocation with CWD different from the entry source directory; nested includes.
- Labels on all three directives, including the address of a label before `.include`.
- Double-quoted strings and escapes; rejection of single-quoted paths; mixed case;
  comments; `.incbin` offset/length; `.filesize` constant definitions.
- Original-source collection, both assembly passes, macro definition directories,
  and include-once behavior.
- For writer changes: correct entry name even if an include sorts first; source
  references in direct-ROM output and linked objects still locate the correct files.

Run `cargo test --workspace` from `C:\Work\Programming\v6asm`.

Document the changes in `C:\Work\Programming\v6asm\docs\directives.md`,
`C:\Work\Programming\v6asm\docs\cli.md`, and, for writer changes,
`C:\Work\Programming\v6asm\docs\debug-metadata.md`.

Test migration: Rewrite single-quoted `.include` paths with double quotes and
escape literal backslashes.
