use crate::diagnostics::{AsmError, AsmResult};
use crate::expr::{Expr, ExprParser};
use crate::instructions::{
    Condition, ParsedOperand, Register, RegisterPair,
};
use crate::lexer::{LocatedToken, Token};
use crate::project::CpuMode;

/// A parsed assembly line
#[derive(Debug)]
pub enum ParsedLine {
    /// Empty or comment-only line
    Empty,
    /// Label definition (global)
    Label(String),
    /// Local label definition (@name)
    LocalLabel(String),
    /// An instruction with mnemonic and operands
    Instruction {
        mnemonic: String,
        operands: Vec<ParsedOperand>,
        /// Expression(s) for immediates/addresses
        expressions: Vec<Expr>,
    },
    /// Directive with its specific data
    Directive(Directive),
    /// Constant definition: Name = Expr or Name EQU expr
    ConstDef {
        name: String,
        is_local: bool,
        expr: Expr,
    },
    /// Variable definition: .var Name value
    VarDef {
        name: String,
        expr: Expr,
    },
}

#[derive(Debug)]
pub enum Directive {
    Org(Expr),
    Include(String),
    FileSize { name: String, path: String },
    IncBin { path: String, offset: Option<Expr>, length: Option<Expr> },
    MacroDef { name: String, params: Vec<MacroParamDef>, body_start: bool },
    EndMacro,
    If(Expr),
    EndIf,
    Loop(Expr),
    EndLoop,
    Optional,
    EndOptional,
    Setting(Vec<(String, String)>),
    Align(Expr),
    Storage { length: Expr, filler: Option<Expr> },
    Byte(Vec<Expr>),
    Word(Vec<Expr>),
    Dword(Vec<Expr>),
    Text(Vec<TextItem>),
    Encoding { enc_type: String, case: Option<String> },
    Print(Vec<PrintArg>),
    Error(Vec<PrintArg>),
}

#[derive(Debug)]
pub struct MacroParamDef {
    pub name: String,
    pub default: Option<String>,
}

#[derive(Debug)]
pub enum TextItem {
    Str(String),
    Char(char),
}

#[derive(Debug)]
pub enum PrintArg {
    Str(String),
    Expr(Expr),
}

/// Parse a tokenized line
pub fn parse_line(tokens: &[LocatedToken], cpu_mode: CpuMode) -> AsmResult<Vec<ParsedLine>> {
    if tokens.is_empty() {
        return Ok(vec![ParsedLine::Empty]);
    }

    let mut results = Vec::new();
    let mut pos = 0;

    // Check for label at the start
    pos = parse_labels(tokens, pos, &mut results)?;

    if pos >= tokens.len() {
        if results.is_empty() {
            return Ok(vec![ParsedLine::Empty]);
        }
        return Ok(results);
    }

    // Check for directive (.xxx)
    if let Some(Token::Dot) = tokens.get(pos).map(|t| &t.value) {
        pos += 1;
        if pos < tokens.len() {
            if let Token::Identifier(name) = &tokens[pos].value {
                let directive_name = name.to_uppercase();
                pos += 1;
                let directive = parse_directive(&directive_name, tokens, &mut pos)?;
                results.push(ParsedLine::Directive(directive));
                return Ok(results);
            }
        }
    }

    // Check for constant definition: NAME = expr, NAME EQU expr, NAME: = expr, NAME: EQU expr
    if let Some(Token::Identifier(name)) = tokens.get(pos).map(|t| &t.value) {
        let name = name.clone();
        // Allow optional colon for label-style definitions: NAME: = expr or NAME: EQU expr
        let mut op_pos = pos + 1;
        if let Some(Token::Colon) = tokens.get(op_pos).map(|t| &t.value) {
            op_pos += 1;
        }
        if let Some(next) = tokens.get(op_pos).map(|t| &t.value) {
            if matches!(next, Token::Operator(ref s) if s == "=") {
                let (expr, _consumed) = parse_expr_from(tokens, op_pos + 1)?;
                results.push(ParsedLine::ConstDef {
                    name,
                    is_local: false,
                    expr,
                });
                return Ok(results);
            }
            if matches!(next, Token::Identifier(ref s) if s.to_uppercase() == "EQU") {
                let (expr, _consumed) = parse_expr_from(tokens, op_pos + 1)?;
                results.push(ParsedLine::ConstDef {
                    name,
                    is_local: false,
                    expr,
                });
                return Ok(results);
            }
        }

        // Check for NAME .filesize "path"
        if let Some(Token::Dot) = tokens.get(pos + 1).map(|t| &t.value) {
            if let Some(Token::Identifier(dname)) = tokens.get(pos + 2).map(|t| &t.value) {
                if dname.to_uppercase() == "FILESIZE" {
                    pos += 3;
                    let path = parse_string_arg(tokens, &mut pos)?;
                    results.push(ParsedLine::Directive(Directive::FileSize { name, path }));
                    return Ok(results);
                }
                if dname.to_uppercase() == "VAR" {
                    pos += 3;
                    let (expr, _) = parse_expr_from(tokens, pos)?;
                    results.push(ParsedLine::VarDef { name, expr });
                    return Ok(results);
                }
            }
        }
    }

    // Check for local constant: @name = expr or @name: = expr
    if let Some(Token::At) = tokens.get(pos).map(|t| &t.value) {
        if let Some(Token::Identifier(name)) = tokens.get(pos + 1).map(|t| &t.value) {
            let name = name.clone();
            let mut check_pos = pos + 2;
            // Skip optional colon
            if let Some(Token::Colon) = tokens.get(check_pos).map(|t| &t.value) {
                check_pos += 1;
            }
            if let Some(Token::Operator(ref s)) = tokens.get(check_pos).map(|t| &t.value) {
                if s == "=" {
                    let (expr, _) = parse_expr_from(tokens, check_pos + 1)?;
                    results.push(ParsedLine::ConstDef {
                        name,
                        is_local: true,
                        expr,
                    });
                    return Ok(results);
                }
            }
        }
    }

    // Must be an instruction or macro invocation
    if pos < tokens.len() {
        let parsed_instr = parse_instruction(tokens, &mut pos, cpu_mode)?;
        results.push(parsed_instr);
    }

    if results.is_empty() {
        Ok(vec![ParsedLine::Empty])
    } else {
        Ok(results)
    }
}

fn parse_labels(tokens: &[LocatedToken], mut pos: usize, results: &mut Vec<ParsedLine>) -> AsmResult<usize> {
    loop {
        // Local label: @name or @name:
        if let Some(Token::At) = tokens.get(pos).map(|t| &t.value) {
            if let Some(Token::Identifier(name)) = tokens.get(pos + 1).map(|t| &t.value) {
                let name = name.clone();
                pos += 2;
                // Check for colon
                if let Some(Token::Colon) = tokens.get(pos).map(|t| &t.value) {
                    pos += 1;
                }
                // Check if this is actually a constant definition (@name: = expr)
                if let Some(Token::Operator(ref s)) = tokens.get(pos).map(|t| &t.value) {
                    if s == "=" {
                        // Don't consume - let the caller handle it
                        return Ok(pos - if tokens.get(pos - 1).map(|t| &t.value) == Some(&Token::Colon) { 3 } else { 2 });
                    }
                }
                results.push(ParsedLine::LocalLabel(name));
                continue;
            }
        }

        // Global label: name: (must have colon to be a label at start of line, unless it's
        // a known identifier pattern)
        if let Some(Token::Identifier(name)) = tokens.get(pos).map(|t| &t.value) {
            if let Some(Token::Colon) = tokens.get(pos + 1).map(|t| &t.value) {
                let name = name.clone();
                // Check if followed by = or EQU (constant definition like "CONST: = value" or "CONST: EQU value")
                if let Some(tok) = tokens.get(pos + 2).map(|t| &t.value) {
                    match tok {
                        Token::Operator(ref s) if s == "=" => break,
                        Token::Identifier(ref s) if s.to_uppercase() == "EQU" => break,
                        _ => {}
                    }
                }
                pos += 2; // skip name and colon
                results.push(ParsedLine::Label(name));
                continue;
            }
        }

        break;
    }
    Ok(pos)
}

fn parse_directive(name: &str, tokens: &[LocatedToken], pos: &mut usize) -> AsmResult<Directive> {
    match name {
        "ORG" => {
            let (expr, consumed) = parse_expr_from(tokens, *pos)?;
            *pos += consumed;
            Ok(Directive::Org(expr))
        }
        "INCLUDE" => {
            let path = parse_string_arg(tokens, pos)?;
            Ok(Directive::Include(path))
        }
        "INCBIN" => {
            let path = parse_string_arg(tokens, pos)?;
            let mut offset = None;
            let mut length = None;
            if eat_comma(tokens, pos) {
                let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                *pos += consumed;
                offset = Some(expr);
                if eat_comma(tokens, pos) {
                    let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                    *pos += consumed;
                    length = Some(expr);
                }
            }
            Ok(Directive::IncBin { path, offset, length })
        }
        "MACRO" => {
            // Parse macro name and params
            let macro_name = if let Some(Token::Identifier(n)) = tokens.get(*pos).map(|t| &t.value) {
                let n = n.clone();
                *pos += 1;
                n
            } else {
                return Err(AsmError::new("Expected macro name"));
            };
            let mut params = Vec::new();
            // Optional parens around params
            let has_parens = matches!(tokens.get(*pos).map(|t| &t.value), Some(Token::OpenParen));
            if has_parens {
                *pos += 1;
            }
            // Parse params
            loop {
                if *pos >= tokens.len() { break; }
                if has_parens {
                    if matches!(tokens.get(*pos).map(|t| &t.value), Some(Token::CloseParen)) {
                        *pos += 1;
                        break;
                    }
                }
                if let Some(Token::Identifier(pname)) = tokens.get(*pos).map(|t| &t.value) {
                    let pname = pname.clone();
                    *pos += 1;
                    let default = if let Some(Token::Operator(ref s)) = tokens.get(*pos).map(|t| &t.value) {
                        if s == "=" {
                            *pos += 1;
                            // Read default value as raw text until comma or close paren
                            Some(read_default_value(tokens, pos, has_parens))
                        } else { None }
                    } else { None };
                    params.push(MacroParamDef { name: pname, default });
                    eat_comma(tokens, pos);
                } else {
                    break;
                }
            }
            Ok(Directive::MacroDef { name: macro_name, params, body_start: true })
        }
        "ENDMACRO" => Ok(Directive::EndMacro),
        "IF" => {
            let (expr, consumed) = parse_expr_from(tokens, *pos)?;
            *pos += consumed;
            Ok(Directive::If(expr))
        }
        "ENDIF" => Ok(Directive::EndIf),
        "LOOP" => {
            let (expr, consumed) = parse_expr_from(tokens, *pos)?;
            *pos += consumed;
            Ok(Directive::Loop(expr))
        }
        "ENDLOOP" | "ENDL" => Ok(Directive::EndLoop),
        "OPTIONAL" | "OPT" | "FUNCTION" | "FUNC" => Ok(Directive::Optional),
        "ENDOPTIONAL" | "ENDOPT" | "ENDFUNCTION" | "ENDFUNC" => Ok(Directive::EndOptional),
        "SETTING" => {
            let mut pairs = Vec::new();
            while *pos < tokens.len() {
                if let Some(Token::Identifier(key)) = tokens.get(*pos).map(|t| &t.value) {
                    let key = key.clone();
                    *pos += 1;
                    eat_comma(tokens, pos);
                    let value = if let Some(Token::Identifier(v)) = tokens.get(*pos).map(|t| &t.value) {
                        let v = v.clone();
                        *pos += 1;
                        v
                    } else if let Some(Token::StringLiteral(v)) = tokens.get(*pos).map(|t| &t.value) {
                        let v = v.clone();
                        *pos += 1;
                        v
                    } else if let Some(Token::Number(n)) = tokens.get(*pos).map(|t| &t.value) {
                        let n = *n;
                        *pos += 1;
                        n.to_string()
                    } else {
                        "true".to_string()
                    };
                    pairs.push((key, value));
                    eat_comma(tokens, pos);
                } else {
                    break;
                }
            }
            Ok(Directive::Setting(pairs))
        }
        "ALIGN" => {
            let (expr, consumed) = parse_expr_from(tokens, *pos)?;
            *pos += consumed;
            Ok(Directive::Align(expr))
        }
        "STORAGE" => {
            let (length, consumed) = parse_expr_from(tokens, *pos)?;
            *pos += consumed;
            let filler = if eat_comma(tokens, pos) {
                let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                *pos += consumed;
                Some(expr)
            } else {
                None
            };
            Ok(Directive::Storage { length, filler })
        }
        "BYTE" | "DB" => {
            let exprs = parse_expr_list(tokens, pos)?;
            Ok(Directive::Byte(exprs))
        }
        "WORD" | "DW" => {
            let exprs = parse_expr_list(tokens, pos)?;
            Ok(Directive::Word(exprs))
        }
        "DWORD" | "DD" => {
            let exprs = parse_expr_list(tokens, pos)?;
            Ok(Directive::Dword(exprs))
        }
        "TEXT" => {
            let items = parse_text_items(tokens, pos)?;
            Ok(Directive::Text(items))
        }
        "ENCODING" => {
            let enc_type = parse_string_arg(tokens, pos)?;
            let case = if eat_comma(tokens, pos) {
                Some(parse_string_arg(tokens, pos)?)
            } else {
                None
            };
            Ok(Directive::Encoding { enc_type, case })
        }
        "PRINT" => {
            let args = parse_print_args(tokens, pos)?;
            Ok(Directive::Print(args))
        }
        "ERROR" => {
            let args = parse_print_args(tokens, pos)?;
            Ok(Directive::Error(args))
        }
        "VAR" => {
            // .var Name value
            if let Some(Token::Identifier(name)) = tokens.get(*pos).map(|t| &t.value) {
                let _name = name.clone();
                *pos += 1;
                let (_expr, consumed) = parse_expr_from(tokens, *pos)?;
                *pos += consumed;
                // We return it as a Directive but actually the outer parse_line handles this
                return Err(AsmError::new("INTERNAL_VAR_DEF"));
            }
            Err(AsmError::new("Expected variable name after .var"))
        }
        "FILESIZE" => {
            // .filesize Name, "path" - but name comes before the dot usually
            let path = parse_string_arg(tokens, pos)?;
            Ok(Directive::FileSize { name: String::new(), path })
        }
        _ => Err(AsmError::new(format!("Unknown directive: .{}", name))),
    }
}

fn parse_instruction(tokens: &[LocatedToken], pos: &mut usize, cpu_mode: CpuMode) -> AsmResult<ParsedLine> {
    let mnemonic = if let Some(Token::Identifier(name)) = tokens.get(*pos).map(|t| &t.value) {
        let name = name.clone();
        *pos += 1;
        name
    } else {
        return Err(AsmError::new("Expected instruction mnemonic"));
    };

    let upper = mnemonic.to_uppercase();

    // Check for DB/DW/DD as bare mnemonics (alternatives to directives)
    match upper.as_str() {
        "DB" => {
            let exprs = parse_expr_list(tokens, pos)?;
            return Ok(ParsedLine::Directive(Directive::Byte(exprs)));
        }
        "DW" => {
            let exprs = parse_expr_list(tokens, pos)?;
            return Ok(ParsedLine::Directive(Directive::Word(exprs)));
        }
        "DD" => {
            let exprs = parse_expr_list(tokens, pos)?;
            return Ok(ParsedLine::Directive(Directive::Dword(exprs)));
        }
        "EQU" => {
            // Shouldn't reach here normally but handle the case
            return Err(AsmError::new("EQU without a name"));
        }
        _ => {}
    }

    // Parse operands
    let (operands, expressions) = parse_operands(tokens, pos, &upper, cpu_mode)?;

    Ok(ParsedLine::Instruction {
        mnemonic: upper,
        operands,
        expressions,
    })
}

fn parse_operands(
    tokens: &[LocatedToken],
    pos: &mut usize,
    mnemonic: &str,
    _cpu_mode: CpuMode,
) -> AsmResult<(Vec<ParsedOperand>, Vec<Expr>)> {
    let mut operands = Vec::new();
    let mut expressions = Vec::new();

    if *pos >= tokens.len() {
        return Ok((operands, expressions));
    }

    loop {
        if *pos >= tokens.len() { break; }

        // Check for register, register pair, memory, or condition
        let tok = &tokens[*pos].value;

        match tok {
            Token::Identifier(name) => {
                let upper = name.to_uppercase();

                // For instructions that expect a register pair operand,
                // check register pair FIRST (B, D, H map to BC, DE, HL)
                if expects_regpair(mnemonic, operands.len()) {
                    if let Some(rp) = RegisterPair::from_name(&upper) {
                        operands.push(ParsedOperand::RegPair(rp));
                        *pos += 1;
                    } else if upper == "M" {
                        operands.push(ParsedOperand::Memory);
                        *pos += 1;
                    } else {
                        let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                        *pos += consumed;
                        expressions.push(expr);
                        if needs_imm16(mnemonic) {
                            operands.push(ParsedOperand::Imm16);
                        } else {
                            operands.push(ParsedOperand::Imm8);
                        }
                    }
                }
                // Check for register
                else if let Some(r) = Register::from_name(&upper) {
                    // C can be both a register and a condition
                    // In context: after a conditional jump/call/ret, C means condition
                    if upper == "C" && is_conditional_mnemonic(mnemonic) && operands.is_empty() {
                        operands.push(ParsedOperand::Condition(Condition::C));
                        *pos += 1;
                    } else {
                        operands.push(ParsedOperand::Reg(r));
                        *pos += 1;
                    }
                }
                // Check for M (memory) - before condition codes since M is both
                else if upper == "M" {
                    operands.push(ParsedOperand::Memory);
                    *pos += 1;
                }
                // Check for condition code
                else if let Some(cc) = Condition::from_name(&upper) {
                    if is_conditional_mnemonic(mnemonic) && operands.is_empty() {
                        operands.push(ParsedOperand::Condition(cc));
                        *pos += 1;
                    } else {
                        // It's a symbol reference in an expression
                        let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                        *pos += consumed;
                        expressions.push(expr);
                        if needs_imm16(mnemonic) {
                            operands.push(ParsedOperand::Imm16);
                        } else {
                            operands.push(ParsedOperand::Imm8);
                        }
                    }
                }
                // Check for register pair (when not already caught by expects_regpair)
                else if let Some(rp) = RegisterPair::from_name(&upper) {
                    operands.push(ParsedOperand::RegPair(rp));
                    *pos += 1;
                }
                // Otherwise it's a symbol (part of an expression)
                else {
                    let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                    *pos += consumed;
                    expressions.push(expr);
                    if needs_imm16(mnemonic) {
                        operands.push(ParsedOperand::Imm16);
                    } else {
                        operands.push(ParsedOperand::Imm8);
                    }
                }
            }
            Token::OpenParen => {
                // Could be (HL), (BC), (DE), (SP), or expression in parens
                if let Some(Token::Identifier(name)) = tokens.get(*pos + 1).map(|t| &t.value) {
                    let upper = name.to_uppercase();
                    if upper == "HL" && matches!(tokens.get(*pos + 2).map(|t| &t.value), Some(Token::CloseParen)) {
                        operands.push(ParsedOperand::Memory);
                        *pos += 3;
                    } else if (upper == "BC" || upper == "DE" || upper == "SP")
                        && matches!(tokens.get(*pos + 2).map(|t| &t.value), Some(Token::CloseParen))
                    {
                        if let Some(rp) = RegisterPair::from_name(&upper) {
                            operands.push(ParsedOperand::RegPair(rp));
                            *pos += 3;
                        } else {
                            let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                            *pos += consumed;
                            expressions.push(expr);
                            operands.push(ParsedOperand::Imm16);
                        }
                    } else {
                        // Expression in parens (direct memory reference, e.g. (nn))
                        let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                        *pos += consumed;
                        expressions.push(expr);
                        operands.push(ParsedOperand::Mem16);
                    }
                } else {
                    // Expression in parens (direct memory reference, e.g. (nn))
                    let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                    *pos += consumed;
                    expressions.push(expr);
                    operands.push(ParsedOperand::Mem16);
                }
            }
            Token::Number(_) | Token::CharLiteral(_) | Token::Operator(_) | Token::At => {
                // Expression (immediate or address)
                let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                *pos += consumed;
                expressions.push(expr);

                // For RST, the operand is 0-7
                if mnemonic == "RST" {
                    if let Expr::Number(n) = &expressions[expressions.len() - 1] {
                        if *n >= 0 && *n <= 7 {
                            operands.push(ParsedOperand::RstNum(*n as u8));
                        } else {
                            return Err(AsmError::new("RST vector must be 0-7"));
                        }
                    } else {
                        operands.push(ParsedOperand::RstNum(0)); // placeholder
                    }
                } else if needs_imm16(mnemonic) {
                    operands.push(ParsedOperand::Imm16);
                } else {
                    operands.push(ParsedOperand::Imm8);
                }
            }
            _ => break,
        }

        // Consume comma between operands
        if !eat_comma(tokens, pos) {
            break;
        }
    }

    Ok((operands, expressions))
}

fn is_conditional_mnemonic(mnemonic: &str) -> bool {
    matches!(mnemonic,
        "JP" | "CALL" | "RET" |
        "JNZ" | "JZ" | "JNC" | "JC" | "JPO" | "JPE" | "JM" |
        "CNZ" | "CZ" | "CNC" | "CC" | "CPO" | "CPE" | "CM" |
        "RNZ" | "RZ" | "RNC" | "RC" | "RPO" | "RPE" | "RM"
    )
}

fn needs_imm16(mnemonic: &str) -> bool {
    matches!(mnemonic,
        "JMP" | "JNZ" | "JZ" | "JNC" | "JC" | "JPO" | "JPE" | "JP" | "JM" |
        "CALL" | "CNZ" | "CZ" | "CNC" | "CC" | "CPO" | "CPE" | "CP" | "CM" |
        "LDA" | "STA" | "LHLD" | "SHLD" |
        "LXI" | "LD"
    )
}

/// Returns true if the given mnemonic expects a register pair at the given operand position
fn expects_regpair(mnemonic: &str, operand_idx: usize) -> bool {
    if operand_idx != 0 {
        return false;
    }
    matches!(mnemonic,
        "LXI" | "PUSH" | "POP" | "DAD" | "INX" | "DCX" |
        "STAX" | "LDAX"
    )
}

fn eat_comma(tokens: &[LocatedToken], pos: &mut usize) -> bool {
    if let Some(Token::Comma) = tokens.get(*pos).map(|t| &t.value) {
        *pos += 1;
        true
    } else {
        false
    }
}

fn parse_string_arg(tokens: &[LocatedToken], pos: &mut usize) -> AsmResult<String> {
    match tokens.get(*pos).map(|t| &t.value) {
        Some(Token::StringLiteral(s)) => {
            let s = s.clone();
            *pos += 1;
            Ok(s)
        }
        _ => Err(AsmError::new("Expected string")),
    }
}

fn parse_expr_from(tokens: &[LocatedToken], start: usize) -> AsmResult<(Expr, usize)> {
    let slice = &tokens[start..];
    let mut parser = ExprParser::new(slice);
    let expr = parser.parse_expr()?;
    Ok((expr, parser.pos()))
}

fn parse_expr_list(tokens: &[LocatedToken], pos: &mut usize) -> AsmResult<Vec<Expr>> {
    let mut exprs = Vec::new();
    loop {
        if *pos >= tokens.len() { break; }
        let (expr, consumed) = parse_expr_from(tokens, *pos)?;
        *pos += consumed;
        exprs.push(expr);
        if !eat_comma(tokens, pos) {
            break;
        }
    }
    Ok(exprs)
}

fn parse_text_items(tokens: &[LocatedToken], pos: &mut usize) -> AsmResult<Vec<TextItem>> {
    let mut items = Vec::new();
    loop {
        if *pos >= tokens.len() { break; }
        match &tokens[*pos].value {
            Token::StringLiteral(s) => {
                items.push(TextItem::Str(s.clone()));
                *pos += 1;
            }
            Token::CharLiteral(c) => {
                items.push(TextItem::Char(*c));
                *pos += 1;
            }
            _ => break,
        }
        if !eat_comma(tokens, pos) {
            break;
        }
    }
    Ok(items)
}

fn parse_print_args(tokens: &[LocatedToken], pos: &mut usize) -> AsmResult<Vec<PrintArg>> {
    let mut args = Vec::new();
    loop {
        if *pos >= tokens.len() { break; }
        match &tokens[*pos].value {
            Token::StringLiteral(s) => {
                args.push(PrintArg::Str(s.clone()));
                *pos += 1;
            }
            _ => {
                let (expr, consumed) = parse_expr_from(tokens, *pos)?;
                *pos += consumed;
                args.push(PrintArg::Expr(expr));
            }
        }
        if !eat_comma(tokens, pos) {
            break;
        }
    }
    Ok(args)
}

fn read_default_value(tokens: &[LocatedToken], pos: &mut usize, has_parens: bool) -> String {
    let mut parts = Vec::new();
    while *pos < tokens.len() {
        match &tokens[*pos].value {
            Token::Comma => break,
            Token::CloseParen if has_parens => break,
            t => {
                parts.push(token_to_string(t));
                *pos += 1;
            }
        }
    }
    parts.join("")
}

fn token_to_string(t: &Token) -> String {
    match t {
        Token::Identifier(s) => s.clone(),
        Token::Number(n) => n.to_string(),
        Token::StringLiteral(s) => format!("\"{}\"", s),
        Token::CharLiteral(c) => format!("'{}'", c),
        Token::Operator(s) => s.clone(),
        Token::Comma => ",".to_string(),
        Token::Colon => ":".to_string(),
        Token::OpenParen => "(".to_string(),
        Token::CloseParen => ")".to_string(),
        Token::Dot => ".".to_string(),
        Token::At => "@".to_string(),
        Token::Newline => "".to_string(),
        Token::Eof => "".to_string(),
    }
}
