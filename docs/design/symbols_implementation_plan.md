# Debug Symbols — Implementation Plan

Based on [symbols_design.md](symbols_design.md) and the current codebase state.

## Current State

Already implemented:
- `DebugInfo` struct (`assembler.rs`) with `labels`, `consts`, `macros`, `line_addresses`, `data_lines`.
- `LabelInfo`, `ConstInfo`, `MacroDebugInfo`, `DataLineInfo` data structs.
- Collection during pass 2: labels, consts, local labels (disambiguated `@name_N`), line addresses, data lines.
- `collect_macro_debug_info()` populates macro names, source locations, and param names.
- `record_line_address()` / `record_data_line()` helpers store per-file, per-line data.

Not yet implemented:
- JSON serialization of `DebugInfo` to `*.symbols.json`.
- Symbol type field (`label`/`const`/`func`/`macro`/`macroparam`).
- `func` detection via `.optional`/`.endoptional` blocks.
- Macro param entries with default value evaluation (`value: -1` when no default).
- CLI flag / automatic output trigger.
- Path relativization (currently stored as-is from preprocessor).
- Unit tests for serialization.

---

## Tasks

### 1. Add `serde` / `serde_json` to `v6_core` runtime dependencies

Move from `[dev-dependencies]` to `[dependencies]` in `crates/v6_core/Cargo.toml`:
```toml
[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
```

### 2. Define the serializable output structures

Create a new module `crates/v6_core/src/debug_symbols.rs` with types that mirror the JSON schema:

```
DebugSymbols          (top-level, maps to the JSON root)
├── symbols:          HashMap<String, SymbolEntry>
├── line_addresses:   HashMap<String, HashMap<usize, Vec<u16>>>
└── data_lines:       HashMap<String, HashMap<usize, DataLineEntry>>

SymbolEntry { value: i64, path: String, line: usize, type: SymbolType }
SymbolType  enum { Label, Const, Func, Macro, MacroParam }
DataLineEntry { addr: u16, byte_length: usize, unit_bytes: usize }
```

All derive `Serialize`. Use `#[serde(rename_all = "camelCase")]` to match JSON naming (`byteLength`, `unitBytes`, `lineAddresses`, `dataLines`).

### 3. Build the `DebugSymbols` from `DebugInfo`

Add `pub fn build_debug_symbols(info: &DebugInfo, symbols: &SymbolTable) -> DebugSymbols` in the new module.

Logic per symbol category:

| Source | Output type | Value |
|---|---|---|
| `info.labels` (global) | `label` | `addr` |
| Labels inside `.optional` blocks | `func` | `addr` |
| `info.consts` | `const` | `value` |
| `info.macros` | `macro` | `-1` |
| Macro params (from `MacroDef.params`) | `macroparam` | evaluated default or `-1` |

Steps:
1. Iterate `info.labels` → emit as `label` (or `func`, see task 4).
2. Iterate `info.consts` → emit as `const`.
3. Iterate `info.macros` → emit macro entry + one `macroparam` per param.
4. Copy `info.line_addresses` and `info.data_lines` directly (types are already compatible).

### 4. Track `.optional` blocks for `func` type detection

During pass 2, when entering an `.optional` block, push a marker onto a stack. When a global label is defined inside an `.optional` block, tag it as a function candidate.

Options (pick one):
- **Option A (lightweight):** Add a `HashSet<String>` named `optional_labels` to `DebugInfo`. Populate when a label is defined while `_optional_stack` is non-empty. In `build_debug_symbols`, check membership to decide `label` vs `func`.
- **Option B (field on LabelInfo):** Add `is_func: bool` to `LabelInfo`. Set during pass 2.

Option A is simpler and avoids changing the existing struct layout.

### 5. Relativize source paths

`SourceLine.file` comes from the preprocessor and may be absolute. Before writing the JSON, strip the project directory prefix so paths are project-relative (forward slashes).

Add a helper: `fn relativize(path: &str, project_dir: &Path) -> String`.

### 6. Serialization and file output

In `crates/v6_core/src/output.rs` (alongside `write_rom` / `write_listing`):

```rust
pub fn generate_debug_symbols(asm: &Assembler) -> AsmResult<String>
pub fn write_debug_symbols(json: &str, path: &Path) -> AsmResult<()>
```

`generate_debug_symbols` calls `build_debug_symbols`, relativizes paths, then `serde_json::to_string_pretty`.

### 7. Wire into CLI

In `crates/v6asm/src/main.rs`:

- Always generate `*.symbols.json` next to the ROM (no flag needed — it's part of the compilation output per the design doc).
- Path: `rom_path.with_extension("symbols.json")`.
- Call `generate_debug_symbols` + `write_debug_symbols` after `write_rom`.

### 8. Register module

Add `pub mod debug_symbols;` to `crates/v6_core/src/lib.rs`.

### 9. Unit tests

Add `crates/v6_core/tests/debug_symbols_tests.rs` with test cases from the design doc's Testing Strategy:

1. **Each symbol type** — assemble a small snippet, build `DebugSymbols`, assert JSON fields.
2. **Local label disambiguation** — two `@loop` in same scope → `@loop_0`, `@loop_1`.
3. **Macro param defaults** — param with default → numeric value; no default → `-1`.
4. **`dataLines` structure** — `.byte` and `.word` directives → correct `addr`, `byteLength`, `unitBytes` grouped by path.
5. **`lineAddresses` multi-address** — verify array has multiple entries when a line maps to more than one address.
6. **Multi-file** — use `.include` to pull in a second file; verify paths in output.
7. **`func` detection** — label inside `.optional` → type `func`, label outside → type `label`.

---

## Execution Order

```
1  Add serde deps              (Cargo.toml change)
2  Create debug_symbols.rs     (types + build function + relativize)
3  Register module             (lib.rs one-liner)
4  Track optional labels       (assembler.rs — small change in pass 2)
5  Serialize + write           (output.rs — two functions)
6  Wire into CLI               (main.rs — ~5 lines)
7  Unit tests                  (new test file)
```

Steps 1–3 can be done together. Step 4 is independent. Steps 5–6 depend on 2. Step 7 depends on all prior steps.

## Out of Scope
- Runtime debug state (`*.debug.json`) — separate feature.
- Breakpoints, watchpoints — belongs to `DebugState`.
- LSP / editor integration — consumer side, not assembler.
