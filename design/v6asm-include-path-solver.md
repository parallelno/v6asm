# v6asm directive path resolution

**Status:** Implemented.
**Repository:** `C:\Work\Programming\v6asm`.

## 1. Goal

Give `.include`, `.incbin`, and `.filesize` the same path lookup and string
handling in v6asm. Support address labels on `.include` and `.incbin`;
`.filesize` defines a named constant, not an address label.

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
  Its prefix check misses mixed case and does not enforce an exact directive name.
- `C:\Work\Programming\v6asm\crates\v6_core\src\parser.rs` handles `.incbin`
  and `.filesize` case-insensitively, with label support and double-quoted strings.
- Preprocessing expands includes before collecting macro definitions or evaluating
  `.if` conditions. It also expands includes inside macro bodies and inactive branches.
- Original-source collection scans includes separately from preprocessing, while
  listing generation scans include lines again. These scans can disagree about
  label-prefixed includes, backslash-separated statements, and skipped includes.
- Missing includes report `Cannot find include file: X`; binary directives report
  `Cannot find file: X`. Include expansion attaches a source location directly;
  assembly relies on surrounding error handling to attach the statement location.

## 3. Path lookup

An absolute operand is used as-is. If it does not name an existing file,
resolution fails.

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

If none exists, all three directives report `Cannot find file: X`, where `X` is
the decoded operand. Attach the directive's
source file and line, using the macro body location for expanded binary directives,
and preserve macro expansion notes. Use this contract in preprocessing and both
assembly passes; do not rely on different outer callers to supply the location.
Read/stat errors must likewise identify the operand and directive location, retain
the underlying I/O error, and identify the resolved path if it differs from the operand.

## 4. Directive syntax (target)

All three directive names are case-insensitive. `.include` and `.incbin` accept
address labels; `.filesize` requires a constant name before the directive:

```asm
part: .include "part.asm"
data: .incbin "assets/data.bin", 0, 16
SIZE .filesize "assets/data.bin"
```

For label-prefixed `.include`, define the label at the current assembly address
immediately before assembling the included source, just as a label on `.incbin`
marks the start of the inserted binary data. Do not move the label into the
included file or attach it to the file's first label. Preserve
`NAME .filesize "path"` syntax.

Reject `.filesize "path"` because the constant name is missing. Also reject an
address-label prefix on a `.filesize` statement, including
`size_label: SIZE .filesize "path"` and `SIZE: .filesize "path"`.

All three path operands are double-quoted strings using the lexer's escape decoding
in `C:\Work\Programming\v6asm\crates\v6_core\src\lexer.rs`. Single-quoted paths
are rejected. Reuse the same path-string parsing for preprocessing and assembly.
Comments outside strings (`;`, `//`, and block comments) are ignored.

## 5. Implementation

- Capture compilation CWD, entry source, and ordered include directories once in
  `cmd_assemble`, then pass them to the core. Make the same resolution configuration
  available to preprocessing and assembly, including for core-library callers;
  `Assembler::new` currently receives no include directories.
- Use one resolver for includes and both assembly passes handling `.incbin` and
  `.filesize`.
- For include recognition, reuse `split_leading_label` to separate and preserve
  leading address labels, then compare the complete directive name to `include`
  case-insensitively. Do not use prefix matching: `.Include` is an include,
  `.includefoo` is not. A recognized include with malformed operands must report
  a syntax error rather than fall through to the assembler's ignored include case.
- Collect original source text during preprocessing as each file is visited,
  rather than resolving and scanning its includes a second time with
  `collect_original_sources`. Return the collected sources together with expanded
  lines. Record include relationships at their physical line and statement offset,
  distinguishing expanded and include-once-skipped occurrences. Listing generation
  must consume these recorded relationships, not rediscover includes with prefix
  matching or assume each include consumes the next source entry. Preserve source
  order and associate repeated expansions with their corresponding include occurrence.
- A resolved source path is the absolute path used to locate a source file,
  retained separately from any shortened filename shown in diagnostics or listings.
  Preserve it for the entry source and every included file. `SourceLine.file`
  currently mixes CWD-relative paths, project-relative paths, and bare filenames
  for absolute files outside `project_dir`; do not reconstruct source directories
  from these display strings.
- Retain each statement's resolved source path through macro-body collection and
  macro expansion. Supply its parent directory to the shared resolver for
  `.incbin` and `.filesize` in both assembly passes.
- For `.incbin` or `.filesize` written in a macro body, use the macro definition
  file's directory.
  Keep the body statement's resolved source path when expanding it, rather than
  replacing it with the invocation file's path. `SourceLine.origin` records the
  body location; `SourceLine.file` records the invocation for expanded lines.
- `.include` is resolved while preprocessing the containing source, including inside
  macro definitions and inactive `.if` branches. It is not deferred to macro
  invocation or conditional evaluation. A missing include in `.if 0` remains an
  error. Included statements collected into a macro body retain their own source
  paths for subsequent binary-directive resolution.
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
- `.debug_line`: each file-table entry contains a basename and a directory index,
  not an absolute filename. The directory table contains the file's resolved parent
  directory, expressed relative to CWD when possible, otherwise as an absolute
  directory. In the emitted DWARF v4 format, directory index zero denotes the
  compilation directory; nonzero indices select directory-table entries.

For example, with CWD `C:/project`, source `C:/project/src/main.asm` uses
`DW_AT_name = src/main.asm`, a directory-table entry `src`, and a file-table entry
`main.asm` referencing that directory. An external source on `D:/shared` uses
directory `D:/shared` and its basename. Two files with the same basename remain
distinct through their directory indices.

The writer already uses `file_name()` and `file_directory()` to create this table
format. Build their inputs from retained resolved source paths, not from the mixed
display strings in `SourceLine.file`. Supply the actual entry source explicitly
to the writer; do not infer it from sorted line records, even if the entry source
emits no instructions itself.

Use the same CWD-based path conversion for all source-bearing debug metadata:
line-table files used by instruction rows, constants, types, instances, and macro
definition/invocation references, and the separate source-path strings in
optional-region entry metadata. These changes must land together with resolved
source-path preservation in section 5 and the `DW_AT_comp_dir` change. Do not
reinterpret project-relative strings as CWD-relative. Listing display names may
remain separate; keep their source associations consistent without using them
as the input to debug-path conversion.

## 7. Tests and migration

Test:

- Search precedence with duplicate names in the source directory, ordered `-I`
  directories, and CWD; relative/absolute `-I`; absolute hit/miss; missing operands.
- CLI invocation with CWD different from the entry source directory; nested includes.
- Relative and absolute spellings of the same source; absolute includes outside
  `project_dir`, including files with the same basename in different directories;
  binary operands in those included files and their macro bodies.
- Address labels on `.include` and `.incbin`, including the address before `.include`;
  named `.filesize` constants; rejection of unnamed and address-label-prefixed
  `.filesize` forms.
- Double-quoted strings and escapes; rejection of single-quoted paths; mixed case;
  comments; `.incbin` offset/length; `.filesize` constant definitions.
- Original-source collection, both assembly passes, macro definition directories,
  and include-once behavior.
- Exact include-name matching (`.Include`, `.includefoo`), syntax errors for malformed
  includes, and listing collection for labels and backslash-separated statements.
- Include preprocessing inside macro definitions and `.if 0`; missing includes in
  inactive branches; source paths for binary directives originating in included macro text.
- Matching missing-file diagnostics and directive locations across all three
  directives, including expanded macro bodies and both assembly passes.
- For writer changes: correct entry name even if an include sorts first; source
  references in direct-ROM output and linked objects still locate the correct files.
  Check basename/directory-index pairs, same-basename files in different directories,
  an entry with no instruction rows, and all source-bearing debug metadata.

Run `cargo test --workspace` from `C:\Work\Programming\v6asm`.

Document the changes in `C:\Work\Programming\v6asm\docs\directives.md`,
`C:\Work\Programming\v6asm\docs\cli.md`, and, for writer changes,
`C:\Work\Programming\v6asm\docs\debug-metadata.md`.

Migration:

- Rewrite single-quoted `.include` paths with double quotes and escape literal
  backslashes. Replace the positive single-quote assertion in
  `C:\Work\Programming\v6asm\crates\v6_core\tests\preprocessor_tests.rs`
  with a rejection test, and remove the single-quote example from `docs/directives.md`.
- Replace the source-project-relative path wording in `docs/debug-metadata.md`
  with the CWD-based directory-table rules above.
- Document removal of the entry-source-directory fallback: nested sources that
  relied on it must adjust operands or supply that directory with `-I`.
- Document that unnamed and address-label-prefixed `.filesize` forms are errors.
