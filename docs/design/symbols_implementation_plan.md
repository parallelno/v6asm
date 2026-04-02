# Debug Symbols — Implementation Plan

Based on [symbols_design.md](symbols_design.md) and the current codebase state.

## Current State

Already implemented:
- `DebugInfo` struct (`assembler.rs`) with `labels`, `consts`, `macros`, `line_addresses`, `data_lines`.
- `LabelInfo`, `ConstInfo`, `MacroDebugInfo`, `DataLineInfo` data structs.
- Collection during pass 2: labels, consts, local labels (disambiguated `@name_N`), line addresses, data lines.
- `collect_macro_debug_info()` populates macro names, source locations, and param names.
- `record_line_address()` / `record_data_line()` helpers store per-file, per-line data.


---

## Implementation Details

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

**Note:** `HashMap<usize, _>` keys are serialized by `serde_json` as string representations of integers (e.g. `"3"`, `"30"`), which matches the design doc's `"<line>"` string keys. No custom serializer needed.

### 3. Build the `DebugSymbols` from `DebugInfo`

Add `pub fn build_debug_symbols(info: &DebugInfo, symbols: &SymbolTable, project_dir: &Path) -> DebugSymbols` in the new module.

`project_dir` is needed for path relativization (§5). Make `Assembler.project_dir` public (it's currently private but other fields like `symbols`, `debug_info`, `pc` are already `pub`).

Logic per symbol category:

| Source | Output type | Value |
|---|---|---|
| `info.labels` (global) | `label` | `addr` |
| Labels inside `.optional` blocks | `func` | `addr` |
| `info.consts` | `const` | `value` |
| `info.macros` | `macro` | `-1` |
| Macro params (from `symbols.all_macros()`) | `macroparam` | evaluated default or `-1` |

**Note:** `info.macros` (`MacroDebugInfo`) stores only param **names**. To get default values, read from `symbols.all_macros()` → `MacroDef.params` → `MacroParam.default: Option<String>`. Parse the default string as a numeric expression; if absent, emit `-1`.

Steps:
1. Iterate `info.labels` → emit as `label` (or `func`, see task 4).
2. Iterate `info.consts` → emit as `const`.
3. Iterate `info.macros` → for each, emit a `macro` entry; then read `MacroDef.params` from `symbols.all_macros()` and emit one `macroparam` per param.
4. Copy `info.line_addresses` and `info.data_lines` directly (types are already compatible).

### 4. Track `.optional` blocks for `func` type detection

**Problem:** The design doc requires labels inside `.optional`/`.endoptional` blocks to be emitted as `func` type. The assembler processes `.optional` blocks via recursive calls to `process_lines_pass2(&lines[i+1..end])` — by the time a label is recorded into `debug_info.labels`, there is no captured state indicating we're inside an `.optional` block.

Only the outermost .optional block is considered; nested .optional blocks are ignored. A function starts at any label inside the outermost block that is referenced from outside and ends at the matching .endoptional. Multiple labels within the same block are treated as separate functions sharing the same end boundary:

```asm
.optional
Func1:               ; ← func (called from outside)
    nop
    .optional        ; nested — ignored for func detection
    Helper:
        nop
    .endoptional
Func2:               ; ← func (also called from outside)
    ret
.endoptional         ; ← shared end boundary for Func1 and Func2
RegularLabel:        ; ← label (outside .optional)
```

**Solution:**
- Add `optional_depth: usize` to `Assembler` (initialized to `0`). Increment before the recursive `process_lines_pass2` call in the `ControlDirective::Optional` arm, decrement after it returns. A depth counter (rather than a boolean) is needed because the recursive processing of nested `.optional` blocks would otherwise clear the flag prematurely on return.
- Add `optional_labels: HashSet<String>` to `DebugInfo`. In `process_parsed_line_pass2`, when a global label is defined and `self.optional_depth > 0`, insert the label name into this set.
- In `build_debug_symbols`, check `optional_labels` membership: present → `func`, absent → `label`.

### 5. Relativize source paths

`SourceLine.file` comes from the preprocessor and may be absolute. Before writing the JSON, strip the project directory prefix so paths are project-relative (forward slashes).

Add a helper: `fn relativize(path: &str, project_dir: &Path) -> String`.

### 6. Serialization and file output

In `crates/v6_core/src/output.rs` (alongside `write_rom` / `write_listing`):

```rust
pub fn generate_debug_symbols(asm: &Assembler) -> AsmResult<String>
pub fn write_debug_symbols(json: &str, path: &Path) -> AsmResult<()>
```

`generate_debug_symbols` calls `build_debug_symbols(info, symbols, project_dir)` which handles path relativization internally, then serializes via `serde_json::to_string_pretty`.

### 7. Wire into CLI

In `crates/v6asm/src/main.rs`:

- Add CLI flag `--symbols` (bool, default false), following the `--lst` pattern:
  ```rust
  /// Generate debug symbols file (.symbols.json) alongside the ROM
  #[arg(long = "symbols")]
  symbols: bool,
  ```
- When `--symbols` is set, generate `rom_path.with_extension("symbols.json")` (e.g. `main.rom` → `main.symbols.json`).
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

## Implementation Phases

### Phase 1 — Minimum Viable Symbols File
**Goal:** Produce a `*.symbols.json` next to the ROM with labels, consts, lineAddresses, and dataLines.

- [x] **1.1** Add `serde` / `serde_json` to `v6_core` runtime dependencies (detail §1)
- [x] **1.2** Create `debug_symbols.rs` module with serializable types: `DebugSymbols`, `SymbolEntry`, `SymbolType`, `DataLineEntry` (detail §2)
- [x] **1.3** Register module in `lib.rs` (detail §8)
- [x] **1.4** Implement `build_debug_symbols` — convert `DebugInfo` labels and consts into `SymbolEntry` items, copy `line_addresses` and `data_lines` (detail §3)
- [x] **1.5** Implement `relativize` path helper (detail §5)
- [x] **1.6** Add `generate_debug_symbols` + `write_debug_symbols` in `output.rs` (detail §6)
- [x] **1.7** Wire into CLI — add `--symbols` flag, emit `*.symbols.json` when set (detail §7)
- [x] **1.8** Record variables (`.var`) into `debug_info.consts` during pass 2 — `VarDef` currently does not populate `debug_info.consts`, but the design doc requires variables to appear as `const` type
- [x] **1.9** Unit tests: label, const, variable-as-const, dataLines structure, lineAddresses multi-address, `relativize` path helper (detail §9, cases 1, 4, 5)

### Phase 2 — Macros & Macro Params
**Goal:** Add macro and macroparam symbol entries with default value handling.

- [x] **2.1** Extend `build_debug_symbols` to emit `macro` entries from `info.macros` (detail §3)
- [x] **2.2** Emit `macroparam` entries per param; evaluate default or use `-1` (detail §3)
- [x] **2.3** Unit tests: macro param defaults, macro with no params (detail §9, case 3)

### Phase 3 — Function Detection
**Goal:** Tag labels inside `.optional`/`.endoptional` blocks as `func` type.

- [ ] **3.1** Add `optional_depth: usize` to `Assembler`, increment/decrement around recursive `.optional` block processing in pass 2 (detail §4)
- [ ] **3.2** Add `optional_labels: HashSet<String>` to `DebugInfo`; populate when a label is defined while `optional_depth > 0` (detail §4)
- [ ] **3.3** Update `build_debug_symbols` to check membership and emit `func` vs `label` (detail §4)
- [ ] **3.4** Unit tests: func detection, label outside optional stays `label` (detail §9, case 7)

### Phase 4 — Testing suite
**Goal:** Validate correctness across included files and local label disambiguation.

- [ ] **4.1** Unit tests: multi-file project with `.include` — paths in output are relative (detail §9, case 6)
- [ ] **4.2** Unit tests: local label disambiguation `@loop_0`, `@loop_1` (detail §9, case 2)
- [ ] **4.3** Integration test: compile sample project, assert all three top-level sections exist with correct structure

---
