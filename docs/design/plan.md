# Plan: Intel 8080/Z80 CLI Assembler & FDD Utility in Rust

**TL;DR**: Build a Rust workspace containing two CLI binaries — `v6asm` (two-pass Intel 8080/Z80 assembler for Vector-06c) and `v6fdd` (FDD image builder) — plus a shared library crate. The assembler compiles `.asm` source files into `.rom` binaries with optional listing output. The FDD tool is also usable standalone. The project follows idiomatic Rust with `clap` for CLI and a clean separation between parsing, assembly, and output.

---

## Phase 1: Project Scaffolding

1. **Create Rust workspace** with three crates:
   - `v6_core` (library) — shared types, expression evaluator, instruction tables, FDD image logic
   - `v6asm` (binary) — the assembler CLI
   - `v6fdd` (binary) — the standalone FDD utility CLI
   - Root `Cargo.toml` with workspace members

2. **Set up dependencies** in root `Cargo.toml`:
   - `clap` (derive) — CLI argument parsing
   - `thiserror` — error types
   - `log` + `env_logger` — diagnostics
   - Embed `rds308.fdd` as `include_bytes!` in `v6_core` so the built-in template ships with the binary

3. **Add reference assets**:
   - Copy `references/fdd/rds308.fdd` → embedded in `v6_core/src/fdd/rds308.fdd`
   - Copy `references/templates/main.asm` → embedded in `v6asm/src/templates/main.asm`

**Directory layout**:
```
Cargo.toml              (workspace)
crates/
  v6_core/
    Cargo.toml
    src/
      lib.rs
      project.rs        (project config types)
      lexer.rs          (tokenizer)
      parser.rs         (directive/instruction parser)
      expr.rs           (expression evaluator)
      assembler.rs      (two-pass assembler orchestrator)
      instructions/
        mod.rs
        i8080.rs        (i8080 opcode table & encoder)
        z80_compat.rs   (Z80 compatibility mnemonic mapping)
      preprocessor.rs   (comment stripping, macro expansion, .include, .loop, .if)
      symbols.rs        (symbol table: labels, consts, vars, scoping)
      encoding.rs       (character encoding: ascii, screencodecommodore)
      output.rs         (ROM binary + debug JSON emitter)
      diagnostics.rs    (errors, warnings, source locations)
      fdd/
        mod.rs
        image.rs        (FDD image read/write — port of fddimage.ts)
        filesystem.rs   (directory, clusters, saveFile — port of fddimage.ts Filesystem)
        rds308.fdd      (embedded binary)
  v6asm/
    Cargo.toml
    src/
      main.rs           (CLI entry: clap, project loading, assemble, output)
      templates/
        main.asm        (embedded template for --init)
  v6fdd/
    Cargo.toml
    src/
      main.rs           (CLI entry: clap, FDD build)
references/             (existing — unchanged)
docs/                   (existing — unchanged)
```

---

## Phase 2: Core Library — Lexer & Expression Evaluator

4. **Lexer** (`v6_core/src/lexer.rs`): Tokenize a single line of assembly into a stream of tokens.
   - Token types: `Identifier`, `Number(i64)`, `StringLiteral`, `CharLiteral`, `Operator`, `Comma`, `Colon`, `OpenParen`, `CloseParen`, `Dot` (for directives), `At` (for local labels), `Newline`, `Comment`
   - Number parsing: decimal, `$hex`, `0xHex`, `%bin`, `0bBin`, `bBin` (with `_` separators), character `'A'`/`'\n'`
   - String parsing: double-quoted with escape sequences (`\n`, `\t`, `\\`, `\"`, `\0`, `\r`)
   - Comment stripping: `;`, `//` (single-line), `/* */` (multi-line, handled during pre-processing)
   - Each token carries source location `(file_id, line, col)` for diagnostics

5. **Expression evaluator** (`v6_core/src/expr.rs`): Pratt parser / recursive descent for the full operator precedence table.
   - Operator precedence (12 levels as documented)
   - Unary prefix: `+`, `-`, `!`, `~`, `<` (low byte), `>` (high byte)
   - Binary: `*`, `/`, `%`, `+`, `-`, `<<`, `>>`, `<`, `<=`, `>`, `>=`, `==`, `!=`, `&`, `^`, `|`, `&&`, `||`
   - Operands: numeric literals, symbol references (resolved via symbol table callback), `TRUE`/`FALSE`, `*` (current PC)
   - Returns `i64` (all arithmetic is signed 64-bit internally, truncated to 8/16/32-bit at emit)
   - Must handle deferred evaluation (first pass: some symbols unknown → mark as unresolved; second pass: all resolved)

---

## Phase 3: Core Library — Instruction Encoding

6. **i8080 instruction table** (`v6_core/src/instructions/i8080.rs`):
   - Full Intel 8080 instruction set (244 valid opcodes)
   - Organized by mnemonic → operand pattern → opcode byte(s) + size
   - Instruction formats: implied (1 byte), register (1 byte), immediate8 (2 bytes), immediate16 (3 bytes), direct address (3 bytes)
   - Mnemonics: `MOV`, `MVI`, `LXI`, `LDA`, `STA`, `LDAX`, `STAX`, `LHLD`, `SHLD`, `XCHG`, `ADD`, `ADC`, `SUB`, `SBB`, `ANA`, `XRA`, `ORA`, `CMP`, `ADI`, `ACI`, `SUI`, `SBI`, `ANI`, `XRI`, `ORI`, `CPI`, `RLC`, `RRC`, `RAL`, `RAR`, `JMP`, `JNZ`, `JZ`, `JNC`, `JC`, `JPO`, `JPE`, `JP`, `JM`, `CALL`, `CNZ`, `CZ`, `CNC`, `CC`, `CPO`, `CPE`, `CP`, `CM`, `RET`, `RNZ`, `RZ`, `RNC`, `RC`, `RPO`, `RPE`, `RP`, `RM`, `PUSH`, `POP`, `DAD`, `INX`, `DCX`, `INR`, `DCR`, `DAA`, `CMA`, `STC`, `CMC`, `HLT`, `NOP`, `DI`, `EI`, `IN`, `OUT`, `RST`, `PCHL`, `SPHL`, `XTHL`

7. **Z80 compatibility mapping** (`v6_core/src/instructions/z80_compat.rs`):
   - Maps Z80 mnemonics to their i8080 opcode equivalents (1:1 encoding)
   - Supported subset: `LD`, `ADD`, `ADC`, `SUB`, `SBC`, `AND`, `XOR`, `OR`, `CP`, `INC`, `DEC`, `JP`, `CALL`, `RET`, `PUSH`, `POP`, `IN`, `OUT`, `EX`, `HALT`, `NOP`, `DI`, `EI`, `RLCA`, `RRCA`, `RLA`, `RRA`, `DAA`, `CPL`, `SCF`, `CCF`, `RST`
   - Z80 register syntax: `A`, `B`, `C`, `D`, `E`, `H`, `L`, `(HL)`, `BC`, `DE`, `HL`, `SP`, `AF`
   - Port I/O forms: `IN A,(N)`, `OUT (N),A`
   - The parser detects CPU mode and dispatches to the correct table

8. **Instruction parser** (`v6_core/src/parser.rs`): Parse a tokenized line into an instruction or directive.
   - Match mnemonic (case-insensitive) against instruction table for current CPU mode
   - Extract operands: register, register pair, immediate, memory reference `M`/`(HL)`, port number
   - Validate operand combinations and sizes
   - Return parsed instruction with opcode, operand expressions, and byte size

---

## Phase 4: Core Library — Preprocessor & Symbol Table

9. **Symbol table** (`v6_core/src/symbols.rs`):
   - Stores labels, constants (`=`/`EQU`), mutable variables (`.var`), and macros
   - Scoping: global labels, local labels/constants (`@name`) scoped between global labels
   - Macro-local scoping: per-invocation namespace `MacroName_<call-index>.Label`
   - Forward reference tracking: symbols can be referenced before definition (resolved in pass 2)
   - Reserved identifier validation (register names, condition codes — see list in spec)
   - Immutability enforcement for `=`/`EQU` constants; mutability for `.var`

10. **Preprocessor** (`v6_core/src/preprocessor.rs`): Expand source before the two-pass assembly.
    - **Multi-line comment stripping**: Remove `/* ... */` across all sources
    - **`.include` expansion**: Recursively inline included files (up to 16 levels), resolve paths relative to: including file → main asm file → project dir → workspace dir → CWD
    - **`.macro` collection**: Parse macro definitions, store in symbol table, remove from source
    - **Macro expansion**: Replace macro invocations with expanded bodies, substitute parameters, create per-invocation label/constant namespaces. Nested expansion up to 32 levels.
    - **`.loop` expansion**: Unroll loop bodies inline (max 100,000 iterations per loop)
    - **`.if`/`.endif` evaluation**: Conditionally include/exclude source blocks. Nested `.if` supported. Inactive branches are skipped.
    - **`.optional`/`.endoptional` marking**: Tag blocks, defer pruning decision until after pass 1 (check if any internal symbol is referenced externally)
    - Output: a flat list of `SourceLine { file_id, original_line, text, macro_context }` ready for the two-pass assembler

---

## Phase 5: Core Library — Two-Pass Assembler

11. **Pass 1 — Symbol resolution** (`v6_core/src/assembler.rs`):
    - Walk all preprocessed lines sequentially
    - Track the program counter (PC), advancing for each instruction/directive
    - Record label addresses (global and local)
    - Record constant definitions (`=`, `EQU`, `.var`) with their expressions
    - Parse instructions to determine their byte size (without emitting bytes)
    - Process `.org` to set PC, `.align` to advance PC, `.storage` to advance PC
    - Evaluate `.filesize` to define constants
    - After pass 1: resolve all deferred constant expressions (forward references now known)

12. **Pass 2 — Code generation** (`v6_core/src/assembler.rs`):
    - Walk all preprocessed lines again
    - Evaluate all expressions (now all symbols are resolved)
    - Encode instructions into bytes using the instruction table
    - Emit data directives: `.byte`/`DB`, `.word`/`DW`, `.dword`/`DD`, `.text`
    - Handle `.align` (emit zero-fill bytes), `.storage` (emit filler or advance PC)
    - Handle `.incbin` (read file, emit bytes at current address)
    - Process `.print` (output diagnostics to stderr)
    - Process `.error` (halt with fatal error)
    - Handle `.encoding` state changes for `.text` directives
    - Prune `.optional` blocks whose symbols were never referenced externally
    - Apply `romAlign` padding if configured
    - Collect debug metadata: line→address mappings, data line info

13. **Output buffer**: A sparse byte buffer (HashMap<u16, u8> or Vec with tracking) representing the 64KB address space. `.storage` without filler advances PC but doesn't write. The final ROM is extracted as a contiguous range from the first emitted byte to the last.

---

## Phase 6: Core Library — Output Generation

14. **ROM output** (`v6_core/src/output.rs`):
    - Extract contiguous byte range from output buffer
    - Apply `romAlign` padding (pad to next multiple with zero bytes)
    - Write to output path

15. **Listing output** (`v6_core/src/output.rs`):
    - Generate `.lst` file showing addresses, emitted bytes, and source lines
    - Enabled via `--lst` CLI flag

---

## Phase 7: FDD Image Library & CLI Tool

16. **FDD image module** (`v6_core/src/fdd/image.rs` + `filesystem.rs`): Port from `fddimage.ts`.
    - Constants: `FDD_SIDES=2`, `FDD_TRACKS_PER_SIDE=82`, `FDD_SECTORS_PER_TRACK=5`, `FDD_SECTOR_LEN=1024`, total `FDD_SIZE=839,680` bytes (820 KB)
    - `MDHeader` struct (32 bytes): status, filename (8), filetype (3), extent, records, FAT[8] (16-bit LE)
    - Directory region: offset `0xA000`–`0xB000`, 128 entries × 32 bytes
    - Cluster size: 2048 bytes, max 390 clusters
    - `Filesystem` struct with methods: `from_bytes()`, `read_dir()`, `build_available_chain()`, `save_file()`, `cluster_to_ths()`, `map_sector()`, `to_bytes()`
    - Embedded `rds308.fdd` as fallback template via `include_bytes!`

17. **v6fdd CLI** (`v6fdd/src/main.rs`):
    - Usage: `v6fdd -t template.fdd -i file1.com -i file2.dat -o output.fdd`
    - Options: `-h` help, `-t <file>` template (optional), `-i <file>` input files (repeatable), `-o <file>` output file (required)
    - Use `clap` derive for argument parsing
    - Calls `v6_core::fdd::build_fdd_image()`

---

## Phase 8: Assembler CLI

18. **v6asm CLI** (`v6asm/src/main.rs`):
    - **Primary mode**: `v6asm <source.asm> [options]` — assemble the source file
      - Run preprocessor → pass 1 → pass 2 → emit ROM
      - Output path defaults to `<source>.rom`, overridable with `-o`
      - CPU mode via `--cpu i8080` (default) or `--cpu z80`
      - ROM alignment via `--rom-align <n>`
      - Optional listing file via `--lst`
    - **Init mode**: `v6asm --init <name>` — create a new `.asm` file
      - Generate `<name>.asm` from the embedded template (`templates/main.asm`)
    - **Options**:
      - `--quiet` / `-q` — suppress `.print` output
      - `--verbose` / `-v` — extra diagnostics

19. **Error reporting**:
    - Format: `file.asm:line:col: error: message`
    - Include the source line and a caret pointing to the error position
    - For macro expansions: show both the macro definition site and the expansion site
    - Non-zero exit code on errors

---

## Phase 9: Testing & Validation

20. **Unit tests** (per module):
    - Lexer: tokenization of all literal formats, comments, edge cases
    - Expression evaluator: operator precedence, unary prefix `<`/`>`, forward references
    - Instruction encoder: all i8080 opcodes, Z80 compat mnemonics
    - Symbol table: scoping, local labels, macro namespaces
    - Preprocessor: `.include` resolution, macro expansion, `.loop` unrolling, `.if` nesting, `.optional` pruning
    - FDD filesystem: save/read file, cluster allocation, directory management

21. **Integration test — reference project**:
    - Assemble `references/test_project/main.asm` using the built assembler
    - Compare output ROM byte-for-byte against `references/test_project/main.rom`
    - This is the primary validation gate — if this matches, the assembler is correct

22. **Additional integration tests**:
    - FDD image generation: build an FDD from known files, verify directory entries and file data
    - `--init` command: verify generated project and asm file are valid and assemble successfully
    - Error cases: undefined labels, duplicate constants, syntax errors, include depth exceeded, macro recursion limit

---

## Relevant Files

- `references/doc/assembler.md` — full assembler specification (the authoritative source)
- `references/fddutil/fddutil.ts` — FDD CLI tool reference (port to Rust)
- `references/fddutil/fddimage.ts` — FDD image/filesystem logic reference (port to Rust)
- `references/fdd/rds308.fdd` — built-in FDD template (embed as `include_bytes!`)
- `references/templates/main.asm` — project template for `--init`
- `references/test_project/main.rom` — expected ROM output (golden file for integration test)
- `references/test_project/main.asm` — reference main assembly with `.include`, `.storage`, macros, local labels
- `references/test_project/palette.asm` — included file with constants, local labels, `DB`/`DW` data
- `references/test_project/test/palette2.asm` — nested include with local labels
- `references/test_project/test/test_include.asm` — deeply nested include defining a constant

## Verification

1. `cargo build` — all three crates compile without errors
2. `cargo test` — all unit tests pass (lexer, expression, instructions, symbols, preprocessor, FDD)
3. **Golden file test**: `v6asm references/test_project/main.asm` → diff output ROM against `main.rom` (byte-for-byte match)
4. **FDD test**: `v6fdd -t references/fdd/rds308.fdd -i <test_file> -o test.fdd` → verify file is readable and directory entries are correct
5. **Init test**: `v6asm --init myproject` → verify `myproject.asm` created, then `v6asm myproject.asm` succeeds
7. **Error test**: assemble intentionally broken `.asm` files → verify proper error messages and non-zero exit codes
8. `cargo clippy` — no warnings
9. `cargo fmt --check` — formatting conformance

## Decisions

- **Rust workspace with 3 crates**: `v6_core` (lib), `v6asm` (bin), `v6fdd` (bin) — clean separation, shared FDD logic
- **Two-pass architecture**: pass 1 collects symbols & sizes, pass 2 emits bytes — standard for assemblers with forward references
- **Preprocessor runs before passes**: macro expansion, `.loop` unrolling, `.if` evaluation, and `.include` inlining happen first, producing a flat line list. This simplifies the two-pass assembler
- **64-bit signed arithmetic internally**: all expression evaluation uses `i64`, truncated to target width at emit time — avoids overflow issues
- **Sparse output buffer**: `HashMap<u16, u8>` or a `Vec<Option<u8>>` of 65536 entries — handles `.storage` without filler (no bytes written but PC advances)
- **Embedded assets**: `rds308.fdd` and `main.asm` template compiled into the binary via `include_bytes!`/`include_str!` — single-binary distribution, no external files needed
- **CPU mode**: defaults to i8080, switchable via `--cpu` CLI flag
- **No incremental compilation**: every invocation compiles from scratch — appropriate for the project size (Vector-06c ROMs are small)
