use v6_core::symbols::{MacroDef, MacroParam, SymbolTable};

#[test]
fn test_global_label() {
    let mut st = SymbolTable::new();
    st.define_label("start", 0x100, "test.asm", 1).unwrap();
    assert_eq!(st.resolve("start"), Some(0x100));
}

#[test]
fn test_constant() {
    let mut st = SymbolTable::new();
    st.define_constant("MAX", 255, "test.asm", 1).unwrap();
    assert_eq!(st.resolve("MAX"), Some(255));
}

#[test]
fn test_global_symbol_lookup_is_case_insensitive() {
    let mut st = SymbolTable::new();
    st.define_label("StartLabel", 0x100, "test.asm", 1).unwrap();
    st.define_constant("MiXeDConst", 255, "test.asm", 2).unwrap();

    assert_eq!(st.resolve("startlabel"), Some(0x100));
    assert_eq!(st.resolve("STARTLABEL"), Some(0x100));
    assert_eq!(st.resolve("mixedconst"), Some(255));
    assert_eq!(st.resolve("MIXEDCONST"), Some(255));
}

#[test]
fn test_variable() {
    let mut st = SymbolTable::new();
    st.define_variable("counter", 10, "test.asm", 1).unwrap();
    assert_eq!(st.resolve("counter"), Some(10));
    st.update_variable("counter", 9).unwrap();
    assert_eq!(st.resolve("counter"), Some(9));
}

#[test]
fn test_local_label() {
    let mut st = SymbolTable::new();
    st.define_label("start", 0x100, "test.asm", 1).unwrap();
    st.define_local_label("loop", 0x110, "test.asm", 5).unwrap();
    assert_eq!(st.resolve_local("loop"), Some(0x110));
}

#[test]
fn test_local_symbol_lookup_is_case_insensitive() {
    let mut st = SymbolTable::new();
    st.define_label("Start", 0x100, "test.asm", 1).unwrap();
    st.define_local_label("InnerLoop", 0x110, "test.asm", 5).unwrap();

    assert_eq!(st.resolve_local("innerloop"), Some(0x110));
    assert_eq!(st.resolve_local("INNERLOOP"), Some(0x110));
}

#[test]
fn test_local_scope_isolation() {
    let mut st = SymbolTable::new();
    st.define_label("func1", 0x100, "test.asm", 1).unwrap();
    st.define_local_label("loop", 0x110, "test.asm", 5).unwrap();
    st.define_label("func2", 0x200, "test.asm", 10).unwrap();
    assert_eq!(st.resolve_local("loop"), None);
}

#[test]
fn test_macro_lookup_is_case_insensitive() {
    let mut st = SymbolTable::new();
    st.define_macro(MacroDef {
        name: "DrawSprite".to_string(),
        params: vec![MacroParam {
            name: "Color".to_string(),
            default: Some("7".to_string()),
        }],
        body: vec!["nop".to_string()],
        file: "test.asm".to_string(),
        line: 1,
    }).unwrap();

    let lower = st.get_macro("drawsprite").expect("macro lookup should ignore case");
    assert_eq!(lower.name, "DrawSprite");

    let upper = st.get_macro("DRAWSPRITE").expect("macro lookup should ignore case");
    assert_eq!(upper.name, "DrawSprite");
}
