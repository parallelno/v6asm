use v6_core::preprocessor::{
    parse_include_directive, parse_macro_args, parse_macro_params,
    replace_param, split_leading_label, strip_multiline_comments,
};

#[test]
fn test_strip_multiline_comments() {
    let input = "before /* comment */ after";
    let result = strip_multiline_comments(input);
    assert_eq!(result, "before  after");
}

#[test]
fn test_strip_multiline_preserves_newlines() {
    let input = "line1\n/* comment\nspanning\nlines */\nline2";
    let result = strip_multiline_comments(input);
    assert_eq!(result, "line1\n\n\n\nline2");
}

#[test]
fn test_parse_include() {
    assert_eq!(parse_include_directive(".include \"test.asm\""), Some("test.asm".to_string()));
    assert_eq!(parse_include_directive(".include 'test.asm'"), Some("test.asm".to_string()));
    assert_eq!(parse_include_directive("  .include \"test.asm\"  ; comment"), Some("test.asm".to_string()));
    assert_eq!(parse_include_directive("mvi a, 0"), None);
}

#[test]
fn test_parse_macro_params() {
    let params = parse_macro_params("(n)");
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name, "n");
    assert!(params[0].default.is_none());

    let params = parse_macro_params("(Background=$06, Border=$0e, Addr)");
    assert_eq!(params.len(), 3);
    assert_eq!(params[0].name, "Background");
    assert_eq!(params[0].default, Some("$06".to_string()));
    assert_eq!(params[2].name, "Addr");
    assert!(params[2].default.is_none());
}

#[test]
fn test_replace_param() {
    assert_eq!(replace_param("hlt n times", "n", "5"), "hlt 5 times");
    assert_eq!(replace_param("mov a, n", "n", "0x10"), "mov a, 0x10");
    assert_eq!(replace_param("innerval", "n", "5"), "innerval");
}

#[test]
fn test_replace_param_string_with_space() {
    // A quoted string argument containing a space must NOT be wrapped in extra
    // parentheses — `"A B"` should stay `"A B"`, not become `("A B")`.
    assert_eq!(replace_param(".text s", "s", "\"A B\""), ".text \"A B\"");
    // But a numeric expression with a space SHOULD be wrapped.
    assert_eq!(replace_param(".byte n", "n", "1 + 2"), ".byte (1 + 2)");
    // A plain string without spaces is unchanged.
    assert_eq!(replace_param(".text s", "s", "\"AB\""), ".text \"AB\"");
}

#[test]
fn test_parse_macro_args_string_with_space() {
    // Quoted string arguments with internal spaces must parse as a single argument.
    let args = parse_macro_args("\"A B\", 9");
    assert_eq!(args, vec!["\"A B\"", "9"]);
    let args = parse_macro_args("\"hello world\"");
    assert_eq!(args, vec!["\"hello world\""]);
}

#[test]
fn test_parse_macro_args() {
    let args = parse_macro_args("$0b, $0f, PalettePtr");
    assert_eq!(args, vec!["$0b", "$0f", "PalettePtr"]);
}

#[test]
fn test_split_leading_label() {
    // Local label sharing the line with a macro invocation.
    let label = split_leading_label("@h05:\t\tCOPY_SPRITE_TO_SCR(5)").unwrap();
    assert!(label.is_local);
    assert_eq!(label.name, "h05");
    assert_eq!(label.rest, "\t\tCOPY_SPRITE_TO_SCR(5)");

    // Global label, nothing after it.
    let label = split_leading_label("table:").unwrap();
    assert!(!label.is_local);
    assert_eq!(label.name, "table");
    assert_eq!(label.rest, "");

    // No label prefix at all.
    assert!(split_leading_label("mvi a, 5").is_none());
    assert!(split_leading_label("lda score + 1 ; x: y").is_none());
    // `@name` without a colon is a local *reference*, not a definition.
    assert!(split_leading_label("@h05").is_none());
    // A colon later on the line does not turn the line into a label.
    assert!(split_leading_label("EMIT(1) ; note:").is_none());
}
