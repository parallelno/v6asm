use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use v6_core::assembler::Assembler;
use v6_core::debug_symbols::{build_debug_symbols, relativize, SymbolType};
use v6_core::diagnostics::AsmError;
use v6_core::output::{generate_debug_symbols, generate_listing, generate_rom, RomConfig};
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

#[test]
fn symbol_lookup_is_case_insensitive_and_outputs_preserve_original_case() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
MixedLabel:
    jmp mixedlabel
MiXeDConst = 0x1234
    lxi h, mixedconst
")]);
    let asm = proj.assemble().unwrap();

    let rom = generate_rom(&asm, &RomConfig::default());
    assert_eq!(&rom[..6], &[0xC3, 0x00, 0x01, 0x21, 0x34, 0x12]);

    let listing = generate_listing(&asm);
    assert!(listing.contains("MixedLabel:"));
    assert!(listing.contains("jmp mixedlabel"));
    assert!(listing.contains("MiXeDConst = 0x1234"));
    assert!(listing.contains("lxi h, mixedconst"));

    let json = generate_debug_symbols(&asm).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let symbols = parsed["symbols"].as_object().unwrap();

    assert!(symbols.contains_key("MixedLabel"));
    assert!(symbols.contains_key("MiXeDConst"));
    assert!(!symbols.contains_key("MIXEDLABEL"));
    assert!(!symbols.contains_key("MIXEDCONST"));
}

#[test]
fn macro_lookup_is_case_insensitive_and_symbols_json_keeps_original_case() {
    let proj = TestProject::new(&[("main.asm", "\
.macro DrawSprite (Color=7)
    mvi a, Color
.endmacro
.org 0x100
    drawsprite()
")]);
    let asm = proj.assemble().unwrap();

    let json = generate_debug_symbols(&asm).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let symbols = parsed["symbols"].as_object().unwrap();

    assert!(symbols.contains_key("DrawSprite"));
    assert!(symbols.contains_key("DrawSprite.Color"));
    assert!(!symbols.contains_key("DRAWSPRITE"));
    assert!(!symbols.contains_key("DRAWSPRITE.COLOR"));
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

// ── func detection (.optional blocks) ───────────────────────────────────────

#[test]
fn label_inside_optional_is_func() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
    jmp MyFunc
.optional
MyFunc:
    nop
    ret
.endoptional
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let entry = ds.symbols.get("MyFunc").expect("MyFunc label missing");
    assert_eq!(entry.sym_type, SymbolType::Func);
    assert_eq!(entry.value, 0x103); // after 3-byte JMP
}

#[test]
fn label_outside_optional_stays_label() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
Start:
    jmp MyFunc
.optional
MyFunc:
    nop
    ret
.endoptional
End:
    hlt
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let start = ds.symbols.get("Start").expect("Start missing");
    assert_eq!(start.sym_type, SymbolType::Label);

    let end = ds.symbols.get("End").expect("End missing");
    assert_eq!(end.sym_type, SymbolType::Label);

    let func = ds.symbols.get("MyFunc").expect("MyFunc missing");
    assert_eq!(func.sym_type, SymbolType::Func);
}

#[test]
fn multiple_labels_in_optional_all_func() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
    jmp Func1
    jmp Func2
.optional
Func1:
    nop
Func2:
    ret
.endoptional
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let f1 = ds.symbols.get("Func1").expect("Func1 missing");
    assert_eq!(f1.sym_type, SymbolType::Func);

    let f2 = ds.symbols.get("Func2").expect("Func2 missing");
    assert_eq!(f2.sym_type, SymbolType::Func);
}

#[test]
fn nested_optional_labels_are_func() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
    jmp Outer
.optional
Outer:
    nop
    call Inner
    .optional
    Inner:
        nop
        ret
    .endoptional
    ret
.endoptional
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let outer = ds.symbols.get("Outer").expect("Outer missing");
    assert_eq!(outer.sym_type, SymbolType::Func);

    let inner = ds.symbols.get("Inner").expect("Inner missing");
    assert_eq!(inner.sym_type, SymbolType::Func);
}

// ── multi-file project with .include ────────────────────────────────────────

#[test]
fn multi_file_paths_are_relative() {
    let proj = TestProject::new(&[
        ("main.asm", "\
.org 0x100
Start:
    nop
    .include \"sub/helper.asm\"
End:
    hlt
"),
        ("sub/helper.asm", "\
Helper:
    nop
    ret
"),
    ]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    // Labels from main file
    let start = ds.symbols.get("Start").expect("Start missing");
    assert_eq!(start.path, "main.asm");

    // Labels from included file should have relative path with forward slashes
    let helper = ds.symbols.get("Helper").expect("Helper missing");
    assert_eq!(helper.path, "sub/helper.asm");

    // line_addresses should contain both files with relative paths
    assert!(ds.line_addresses.contains_key("main.asm"), "main.asm missing from line_addresses");
    assert!(ds.line_addresses.contains_key("sub/helper.asm"), "sub/helper.asm missing from line_addresses");
}

// ── local label disambiguation ──────────────────────────────────────────────

#[test]
fn local_label_disambiguation() {
    let proj = TestProject::new(&[("main.asm", "\
.org 0x100
Func1:
    nop
@loop:
    nop
    jmp @loop
Func2:
    nop
@loop:
    nop
    jmp @loop
")]);
    let asm = proj.assemble().unwrap();
    let ds = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());

    let loop0 = ds.symbols.get("@loop_0").expect("@loop_0 missing");
    assert_eq!(loop0.sym_type, SymbolType::Label);
    assert_eq!(loop0.path, "main.asm");

    let loop1 = ds.symbols.get("@loop_1").expect("@loop_1 missing");
    assert_eq!(loop1.sym_type, SymbolType::Label);
    assert_eq!(loop1.path, "main.asm");

    // They should have different addresses
    assert_ne!(loop0.value, loop1.value);
}

// ── integration test ────────────────────────────────────────────────────────

#[test]
fn integration_full_project() {
    let proj = TestProject::new(&[
        ("main.asm", "\
MAX_SIZE = 64
.org 0x100
Start:
    nop
    call MyFunc
@loop:
    jmp @loop
.byte 0xAA, 0xBB
.word 0x1234
    .include \"lib/util.asm\"
.optional
MyFunc:
    ret
.endoptional
"),
        ("lib/util.asm", "\
Util:
    nop
    ret
"),
    ]);
    let asm = proj.assemble().unwrap();
    let json = generate_debug_symbols(&asm).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // All three top-level sections exist
    let symbols = parsed.get("symbols").expect("symbols section missing").as_object().unwrap();
    let line_addrs = parsed.get("lineAddresses").expect("lineAddresses section missing").as_object().unwrap();
    let data_lines = parsed.get("dataLines").expect("dataLines section missing").as_object().unwrap();

    // Symbols from both files
    assert!(symbols.contains_key("Start"));
    assert!(symbols.contains_key("Util"));
    assert!(symbols.contains_key("MAX_SIZE"));
    assert!(symbols.contains_key("MyFunc"));

    // Types
    assert_eq!(symbols["Start"]["type"], "label");
    assert_eq!(symbols["MAX_SIZE"]["type"], "const");
    assert_eq!(symbols["MyFunc"]["type"], "func");
    assert_eq!(symbols["Util"]["type"], "label");

    // Paths are relative
    assert_eq!(symbols["Start"]["path"], "main.asm");
    assert_eq!(symbols["Util"]["path"], "lib/util.asm");

    // Local label disambiguation
    assert!(symbols.contains_key("@loop_0"));

    // lineAddresses has entries for both files
    assert!(line_addrs.contains_key("main.asm"));
    assert!(line_addrs.contains_key("lib/util.asm"));

    // dataLines has entries
    assert!(data_lines.contains_key("main.asm"));
    let main_data = data_lines["main.asm"].as_object().unwrap();
    // .byte on line 8, .word on line 9
    assert!(main_data.contains_key("8") || main_data.contains_key("9"),
        "expected data line entries for .byte/.word directives, got keys: {:?}", main_data.keys().collect::<Vec<_>>());
}
