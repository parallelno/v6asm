use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use v6_core::assembler::Assembler;
use v6_core::debug_symbols::{build_debug_symbols, relativize, SymbolType};
use v6_core::diagnostics::AsmError;
use v6_core::output::generate_debug_symbols;
use v6_core::preprocessor::preprocess;
use v6_core::project::CpuMode;
use v6_core::symbols::SymbolTable;

// ── helpers ─────────────────────────────────────────────────────────────────

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TestProject {
    root: PathBuf,
}

impl TestProject {
    fn new(files: &[(&str, &str)]) -> Self {
        let unique = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("v6asm-dbgsym-{}-{}", nanos, unique));
        fs::create_dir_all(&root).unwrap();
        for (path, content) in files {
            let full_path = root.join(path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full_path, content).unwrap();
        }
        Self { root }
    }

    fn assemble(&self) -> Result<Assembler, AsmError> {
        let main_path = self.root.join("main.asm");
        let mut symbols = SymbolTable::new();
        let lines = preprocess(&main_path, &self.root, &mut symbols, &|path| {
            fs::read_to_string(path).map_err(|err| AsmError::new(err.to_string()))
        })?;

        let mut asm = Assembler::new(CpuMode::I8080, self.root.clone());
        asm.quiet = true;
        asm.symbols = symbols;
        asm.assemble(&lines)?;
        asm.collect_macro_debug_info();
        Ok(asm)
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

// ── label and const symbols ─────────────────────────────────────────────────

#[test]
fn label_symbol_entry() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
Start:
    nop
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let entry = ds.symbols.get("Start").expect("Start label missing");
    assert_eq!(entry.value, 0x100);
    assert_eq!(entry.sym_type, SymbolType::Label);
    assert_eq!(entry.path, "main.asm");
    assert_eq!(entry.line, 2);
}

#[test]
fn const_symbol_entry() {
    let proj = TestProject::new(&[("main.asm", "\
MAX_SIZE = 64
.org 0x100
    nop
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let entry = ds.symbols.get("MAX_SIZE").expect("MAX_SIZE const missing");
    assert_eq!(entry.value, 64);
    assert_eq!(entry.sym_type, SymbolType::Const);
    assert_eq!(entry.path, "main.asm");
    assert_eq!(entry.line, 1);
}

#[test]
fn var_recorded_as_const() {
    let proj = TestProject::new(&[("main.asm", "\
counter .var 10
.org 0x100
    nop
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let entry = ds.symbols.get("counter").expect("counter var missing");
    assert_eq!(entry.value, 10);
    assert_eq!(entry.sym_type, SymbolType::Const);
    assert_eq!(entry.path, "main.asm");
}

// ── data_lines structure ────────────────────────────────────────────────────

#[test]
fn data_lines_byte_directive() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
.byte 1, 2, 3
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let file_data = ds.data_lines.get("main.asm").expect("main.asm data_lines missing");
    let entry = file_data.get(&2).expect("line 2 data missing");
    assert_eq!(entry.addr, 0x100);
    assert_eq!(entry.byte_length, 3);
    assert_eq!(entry.unit_bytes, 1);
}

#[test]
fn data_lines_word_directive() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x200
.word 0x1234, 0x5678
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let file_data = ds.data_lines.get("main.asm").expect("main.asm data_lines missing");
    let entry = file_data.get(&2).expect("line 2 data missing");
    assert_eq!(entry.addr, 0x200);
    assert_eq!(entry.byte_length, 4);
    assert_eq!(entry.unit_bytes, 2);
}

// ── line_addresses ──────────────────────────────────────────────────────────

#[test]
fn line_addresses_single_instruction() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
    nop
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let file_addrs = ds.line_addresses.get("main.asm").expect("main.asm line_addresses missing");
    let addrs = file_addrs.get(&2).expect("line 2 address missing");
    assert!(addrs.contains(&0x100));
}

#[test]
fn line_addresses_multiple_entries() {
    // A loop unrolling should generate multiple address entries for the same source line
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
.loop 3
    nop
.endloop
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let file_addrs = ds.line_addresses.get("main.asm").expect("main.asm line_addresses missing");
    let addrs = file_addrs.get(&3).expect("line 3 addresses missing");
    assert!(addrs.len() >= 3, "expected at least 3 address entries from 3 loop iterations, got {}", addrs.len());
    assert!(addrs.contains(&0x100));
    assert!(addrs.contains(&0x101));
    assert!(addrs.contains(&0x102));
}

// ── relativize helper ───────────────────────────────────────────────────────

#[test]
fn relativize_strips_project_prefix() {
    use std::path::Path;
    assert_eq!(relativize("/home/proj/main.asm", Path::new("/home/proj")), "main.asm");
    assert_eq!(relativize("/home/proj/sub/f.asm", Path::new("/home/proj")), "sub/f.asm");
}

#[test]
fn relativize_returns_input_when_no_prefix() {
    use std::path::Path;
    assert_eq!(relativize("main.asm", Path::new("/other")), "main.asm");
}

// ── JSON serialization round-trip ───────────────────────────────────────────

#[test]
fn generate_debug_symbols_produces_valid_json() {
    let proj = TestProject::new(&[("main.asm", "\
MAX = 42
.org 0x100
Start:
    nop
.byte 0xFF
")]);
    let asm = proj.assemble().unwrap();
    let json = generate_debug_symbols(&asm).unwrap();

    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed.get("symbols").is_some());
    assert!(parsed.get("lineAddresses").is_some());
    assert!(parsed.get("dataLines").is_some());

    // Verify camelCase key naming
    let symbols = parsed["symbols"].as_object().unwrap();
    let start = symbols.get("Start").unwrap();
    assert_eq!(start["type"], "label");
    assert_eq!(start["value"], 0x100);

    let max = symbols.get("MAX").unwrap();
    assert_eq!(max["type"], "const");
    assert_eq!(max["value"], 42);
}

// ── macro and macroparam symbols ────────────────────────────────────────────

#[test]
fn macro_symbol_entry() {
    let proj = TestProject::new(&[("main.asm", "\
.macro PrintChar (ch)
    mvi a, ch
.endmacro
.org 0x100
    PrintChar(65)
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let entry = ds.symbols.get("PrintChar").expect("PrintChar macro missing");
    assert_eq!(entry.value, -1);
    assert_eq!(entry.sym_type, SymbolType::Macro);
    assert_eq!(entry.path, "main.asm");
    assert_eq!(entry.line, 1);
}

#[test]
fn macroparam_no_default() {
    let proj = TestProject::new(&[("main.asm", "\
.macro PrintChar (ch)
    mvi a, ch
.endmacro
.org 0x100
    PrintChar(65)
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let entry = ds.symbols.get("PrintChar.ch").expect("PrintChar.ch macroparam missing");
    assert_eq!(entry.value, -1);
    assert_eq!(entry.sym_type, SymbolType::MacroParam);
    assert_eq!(entry.path, "main.asm");
    assert_eq!(entry.line, 1);
}

#[test]
fn macroparam_with_default() {
    let proj = TestProject::new(&[("main.asm", "\
.macro SetColor (col=7)
    mvi a, col
.endmacro
.org 0x100
    SetColor()
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let entry = ds.symbols.get("SetColor.col").expect("SetColor.col macroparam missing");
    assert_eq!(entry.value, 7);
    assert_eq!(entry.sym_type, SymbolType::MacroParam);
}

#[test]
fn macro_multiple_params_mixed_defaults() {
    let proj = TestProject::new(&[("main.asm", "\
.macro Draw (x, y, color=3)
    mvi a, x
    mvi b, y
    mvi c, color
.endmacro
.org 0x100
    Draw(10, 20)
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    // macro entry
    let m = ds.symbols.get("Draw").expect("Draw macro missing");
    assert_eq!(m.sym_type, SymbolType::Macro);
    assert_eq!(m.value, -1);

    // params
    let x = ds.symbols.get("Draw.x").expect("Draw.x missing");
    assert_eq!(x.value, -1);
    assert_eq!(x.sym_type, SymbolType::MacroParam);

    let y = ds.symbols.get("Draw.y").expect("Draw.y missing");
    assert_eq!(y.value, -1);

    let color = ds.symbols.get("Draw.color").expect("Draw.color missing");
    assert_eq!(color.value, 3);
    assert_eq!(color.sym_type, SymbolType::MacroParam);
}

#[test]
fn macro_no_params() {
    let proj = TestProject::new(&[("main.asm", "\
.macro DoNothing
    nop
.endmacro
.org 0x100
    DoNothing
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let entry = ds.symbols.get("DoNothing").expect("DoNothing macro missing");
    assert_eq!(entry.value, -1);
    assert_eq!(entry.sym_type, SymbolType::Macro);

    // No macroparam entries should exist for this macro
    let param_keys: Vec<_> = ds.symbols.keys().filter(|k| k.starts_with("DoNothing.")).collect();
    assert!(param_keys.is_empty(), "expected no macroparam entries, got {:?}", param_keys);
}
