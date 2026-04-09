use v6_core::lexer::{tokenize_line, Token};

#[test]
fn test_decimal() {
    let tokens = tokenize_line("42", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(42));
}

#[test]
fn test_hex_dollar() {
    let tokens = tokenize_line("$FF", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::Number(255));
}

#[test]
fn test_hex_0x() {
    let tokens = tokenize_line("0xFF", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::Number(255));
}

#[test]
fn test_binary_percent() {
    let tokens = tokenize_line("%1010", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::Number(10));
}

#[test]
fn test_binary_0b() {
    let tokens = tokenize_line("0b1010", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::Number(10));
}

#[test]
fn test_binary_b_prefix() {
    let tokens = tokenize_line("b1010", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::Number(10));
}

#[test]
fn test_binary_with_underscores() {
    let tokens = tokenize_line("%11_00", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::Number(12));
}

#[test]
fn test_char_literal() {
    let tokens = tokenize_line("'A'", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::CharLiteral('A'));
}

#[test]
fn test_char_escape() {
    let tokens = tokenize_line("'\\n'", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::CharLiteral('\n'));
}

#[test]
fn test_string_literal() {
    let tokens = tokenize_line("\"hello\\nworld\"", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::StringLiteral("hello\nworld".to_string()));
}

#[test]
fn test_instruction_line() {
    let tokens = tokenize_line("  mvi a, 0x10  ; comment", "test", 1).unwrap();
    assert_eq!(tokens.len(), 4);
    assert_eq!(tokens[0].value, Token::Identifier("mvi".to_string()));
    assert_eq!(tokens[1].value, Token::Identifier("a".to_string()));
    assert_eq!(tokens[2].value, Token::Comma);
    assert_eq!(tokens[3].value, Token::Number(16));
}

#[test]
fn test_operators() {
    let tokens = tokenize_line("<< >> <= >= == != && ||", "test", 1).unwrap();
    assert_eq!(tokens.len(), 8);
    assert_eq!(tokens[0].value, Token::Operator("<<".to_string()));
    assert_eq!(tokens[1].value, Token::Operator(">>".to_string()));
}

#[test]
fn test_label_with_colon() {
    let tokens = tokenize_line("start:", "test", 1).unwrap();
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].value, Token::Identifier("start".to_string()));
    assert_eq!(tokens[1].value, Token::Colon);
}

#[test]
fn test_local_label() {
    let tokens = tokenize_line("@loop:", "test", 1).unwrap();
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0].value, Token::At);
    assert_eq!(tokens[1].value, Token::Identifier("loop".to_string()));
    assert_eq!(tokens[2].value, Token::Colon);
}

#[test]
fn test_directive() {
    let tokens = tokenize_line(".org 0x100", "test", 1).unwrap();
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0].value, Token::Dot);
    assert_eq!(tokens[1].value, Token::Identifier("org".to_string()));
    assert_eq!(tokens[2].value, Token::Number(256));
}

#[test]
fn test_b_prefix_binary() {
    let tokens = tokenize_line("b00_011_111", "test", 1).unwrap();
    assert_eq!(tokens[0].value, Token::Number(0b00_011_111));
}

#[test]
fn test_inline_multiline_comment() {
    let tokens = tokenize_line("mvi a, 5 /* comment */ mvi b, 6", "test", 1).unwrap();
    assert_eq!(tokens.len(), 8);
}

#[test]
fn test_hex_h_suffix() {
    let tokens = tokenize_line("07Fh", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(0x7F));
}

#[test]
fn test_hex_h_suffix_080h() {
    let tokens = tokenize_line("080h", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(0x80));
}

#[test]
fn test_hex_h_suffix_0ffh() {
    let tokens = tokenize_line("0FFh", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(0xFF));
}

#[test]
fn test_hex_h_suffix_zero() {
    let tokens = tokenize_line("0h", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(0));
}

#[test]
fn test_hex_h_suffix_single_digit() {
    let tokens = tokenize_line("7h", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(7));
}

#[test]
fn test_hex_h_suffix_uppercase() {
    let tokens = tokenize_line("0FFH", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(0xFF));
}

#[test]
fn test_hex_h_suffix_16bit() {
    let tokens = tokenize_line("0ABCDh", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(0xABCD));
}

#[test]
fn test_hex_h_suffix_with_underscores() {
    let tokens = tokenize_line("0F_Fh", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(0xFF));
}

#[test]
fn test_decimal_still_works_with_hex_h() {
    let tokens = tokenize_line("1234", "test", 1).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].value, Token::Number(1234));
}

#[test]
fn test_hex_h_in_instruction() {
    let tokens = tokenize_line("mvi a, 07Fh", "test", 1).unwrap();
    assert_eq!(tokens.len(), 4);
    assert_eq!(tokens[0].value, Token::Identifier("mvi".to_string()));
    assert_eq!(tokens[1].value, Token::Identifier("a".to_string()));
    assert_eq!(tokens[2].value, Token::Comma);
    assert_eq!(tokens[3].value, Token::Number(0x7F));
}
