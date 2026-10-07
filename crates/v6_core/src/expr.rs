use crate::diagnostics::{AsmError, AsmResult, SourceLocation};
use crate::lexer::{LocatedToken, Token};
use crate::object::section::RelocTarget;

/// Expression AST node
#[derive(Debug, Clone)]
pub enum Expr {
    Number(i64),
    Symbol(String),
    LocalSymbol(String),
    CurrentPC,
    BoolLiteral(bool),
    UnaryOp { op: UnaryOp, expr: Box<Expr> },
    BinaryOp { op: BinaryOp, left: Box<Expr>, right: Box<Expr> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Plus,
    Minus,
    Not,
    BitNot,
    LowByte,
    HighByte,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Shl,
    Shr,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    BitAnd,
    BitXor,
    BitOr,
    LogAnd,
    LogOr,
}

impl BinaryOp {
    fn precedence(self) -> u8 {
        match self {
            BinaryOp::LogOr => 1,
            BinaryOp::LogAnd => 2,
            BinaryOp::BitOr => 3,
            BinaryOp::BitXor => 4,
            BinaryOp::BitAnd => 5,
            BinaryOp::Eq | BinaryOp::Ne => 6,
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => 7,
            BinaryOp::Shl | BinaryOp::Shr => 8,
            BinaryOp::Add | BinaryOp::Sub => 9,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 10,
        }
    }
}

/// Evaluate a parsed expression given a symbol resolver.
/// Returns Err if a symbol cannot be resolved.
pub fn eval_expr(
    expr: &Expr,
    resolve_symbol: &dyn Fn(&str) -> Option<i64>,
    pc: u16,
) -> AsmResult<i64> {
    match expr {
        Expr::Number(n) => Ok(*n),
        Expr::BoolLiteral(b) => Ok(if *b { 1 } else { 0 }),
        Expr::CurrentPC => Ok(pc as i64),
        Expr::Symbol(name) => {
            resolve_symbol(name).ok_or_else(|| AsmError::new(format!("Undefined symbol: {}", name)))
        }
        Expr::LocalSymbol(name) => {
            resolve_symbol(name).ok_or_else(|| AsmError::new(format!("Undefined local symbol: @{}", name)))
        }
        Expr::UnaryOp { op, expr } => {
            let val = eval_expr(expr, resolve_symbol, pc)?;
            Ok(match op {
                UnaryOp::Plus => val,
                UnaryOp::Minus => -val,
                UnaryOp::Not => if val == 0 { 1 } else { 0 },
                UnaryOp::BitNot => !val,
                UnaryOp::LowByte => val & 0xFF,
                UnaryOp::HighByte => (val >> 8) & 0xFF,
            })
        }
        Expr::BinaryOp { op, left, right } => {
            let l = eval_expr(left, resolve_symbol, pc)?;
            let r = eval_expr(right, resolve_symbol, pc)?;
            Ok(match op {
                BinaryOp::Add => l.wrapping_add(r),
                BinaryOp::Sub => l.wrapping_sub(r),
                BinaryOp::Mul => l.wrapping_mul(r),
                BinaryOp::Div => {
                    if r == 0 {
                        return Err(AsmError::new("Division by zero"));
                    }
                    l / r
                }
                BinaryOp::Mod => {
                    if r == 0 {
                        return Err(AsmError::new("Modulo by zero"));
                    }
                    l % r
                }
                BinaryOp::Shl => l.wrapping_shl(r as u32),
                BinaryOp::Shr => l.wrapping_shr(r as u32),
                BinaryOp::Lt => if l < r { 1 } else { 0 },
                BinaryOp::Le => if l <= r { 1 } else { 0 },
                BinaryOp::Gt => if l > r { 1 } else { 0 },
                BinaryOp::Ge => if l >= r { 1 } else { 0 },
                BinaryOp::Eq => if l == r { 1 } else { 0 },
                BinaryOp::Ne => if l != r { 1 } else { 0 },
                BinaryOp::BitAnd => l & r,
                BinaryOp::BitXor => l ^ r,
                BinaryOp::BitOr => l | r,
                BinaryOp::LogAnd => if l != 0 && r != 0 { 1 } else { 0 },
                BinaryOp::LogOr => if l != 0 || r != 0 { 1 } else { 0 },
            })
        }
    }
}

/// The byte-extraction operation applied to a relocatable value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOp {
    /// No byte operation — full-width value.
    None,
    /// Low byte (`<expr`).
    Lo,
    /// High byte (`>expr`).
    Hi,
}

/// Classification of a symbol for relocatable evaluation.
#[derive(Debug, Clone)]
pub enum SymValue {
    /// A pure constant value, foldable into the addend.
    Absolute(i64),
    /// A section-relative defined label.
    Section { index: usize, offset: i64 },
    /// Referenced but not defined in this translation unit.
    Undefined,
}

/// The result of a relocatable expression evaluation: at most one linear
/// symbol term plus a constant addend, optionally tagged with a byte op.
#[derive(Debug, Clone)]
pub struct RelocValue {
    /// Constant part of the value.
    pub addend: i64,
    /// The single relocatable term, if any. `None` means a pure constant.
    pub target: Option<RelocTarget>,
    /// Byte operation applied to the (whole) value.
    pub byte_op: ByteOp,
}

impl RelocValue {
    fn constant(v: i64) -> Self {
        Self { addend: v, target: None, byte_op: ByteOp::None }
    }

    /// Returns the constant value if this is not relocatable, else an error.
    pub fn require_constant(&self) -> AsmResult<i64> {
        if let Some(target) = &self.target {
            let what = match target {
                RelocTarget::Symbol(name) => format!("symbol '{}'", name),
                RelocTarget::Section(_) => "a section-relative address (label)".to_string(),
            };
            return Err(AsmError::new(format!(
                "expression must be a constant here, but it references {}, whose address is not known until link time",
                what
            )));
        }
        Ok(match self.byte_op {
            ByteOp::None => self.addend,
            ByteOp::Lo => self.addend & 0xFF,
            ByteOp::Hi => (self.addend >> 8) & 0xFF,
        })
    }
}

/// Evaluate an expression to a relocatable value. Unlike [`eval_expr`], a
/// section-relative or undefined symbol does not cause an error — it is carried
/// symbolically so the caller can emit a relocation.
///
/// `resolve` classifies a symbol name (the boolean is true for `@local`
/// symbols). `cur_section` is the active section index, used for `*`/PC.
pub fn eval_expr_reloc(
    expr: &Expr,
    resolve: &dyn Fn(&str, bool) -> SymValue,
    pc: u16,
    cur_section: usize,
) -> AsmResult<RelocValue> {
    match expr {
        Expr::Number(n) => Ok(RelocValue::constant(*n)),
        Expr::BoolLiteral(b) => Ok(RelocValue::constant(if *b { 1 } else { 0 })),
        Expr::CurrentPC => Ok(RelocValue {
            addend: pc as i64,
            target: Some(RelocTarget::Section(cur_section)),
            byte_op: ByteOp::None,
        }),
        Expr::Symbol(name) => Ok(sym_to_value(resolve(name, false), name)),
        Expr::LocalSymbol(name) => Ok(sym_to_value(resolve(name, true), name)),
        Expr::UnaryOp { op, expr } => {
            let val = eval_expr_reloc(expr, resolve, pc, cur_section)?;
            match op {
                UnaryOp::Plus => Ok(val),
                UnaryOp::LowByte => apply_byte_op(val, ByteOp::Lo),
                UnaryOp::HighByte => apply_byte_op(val, ByteOp::Hi),
                UnaryOp::Minus => {
                    if val.target.is_some() {
                        return Err(AsmError::new(
                            "cannot negate a relocatable symbol",
                        ));
                    }
                    Ok(RelocValue::constant(-val.require_constant()?))
                }
                UnaryOp::Not => Ok(RelocValue::constant(
                    if val.require_constant()? == 0 { 1 } else { 0 },
                )),
                UnaryOp::BitNot => Ok(RelocValue::constant(!val.require_constant()?)),
            }
        }
        Expr::BinaryOp { op, left, right } => {
            let l = eval_expr_reloc(left, resolve, pc, cur_section)?;
            let r = eval_expr_reloc(right, resolve, pc, cur_section)?;
            match op {
                BinaryOp::Add => combine_add(l, r, false),
                BinaryOp::Sub => combine_add(l, r, true),
                _ => {
                    // All other operators require constant operands.
                    let lc = l.require_constant()?;
                    let rc = r.require_constant()?;
                    let v = eval_binary_const(*op, lc, rc)?;
                    Ok(RelocValue::constant(v))
                }
            }
        }
    }
}

fn sym_to_value(sv: SymValue, name: &str) -> RelocValue {
    match sv {
        SymValue::Absolute(v) => RelocValue::constant(v),
        SymValue::Section { index, offset } => RelocValue {
            addend: offset,
            target: Some(RelocTarget::Section(index)),
            byte_op: ByteOp::None,
        },
        SymValue::Undefined => RelocValue {
            addend: 0,
            target: Some(RelocTarget::Symbol(name.to_string())),
            byte_op: ByteOp::None,
        },
    }
}

fn apply_byte_op(mut val: RelocValue, op: ByteOp) -> AsmResult<RelocValue> {
    if val.target.is_some() {
        if val.byte_op != ByteOp::None {
            return Err(AsmError::new(
                "nested byte operations on a relocatable symbol are not supported",
            ));
        }
        val.byte_op = op;
        Ok(val)
    } else {
        let v = val.addend;
        Ok(RelocValue::constant(match op {
            ByteOp::Lo => v & 0xFF,
            ByteOp::Hi => (v >> 8) & 0xFF,
            ByteOp::None => v,
        }))
    }
}

fn combine_add(l: RelocValue, r: RelocValue, subtract: bool) -> AsmResult<RelocValue> {
    if l.byte_op != ByteOp::None || r.byte_op != ByteOp::None {
        return Err(AsmError::new(
            "byte operation cannot be combined with addition/subtraction of relocatable symbols",
        ));
    }
    let rhs_addend = if subtract { -r.addend } else { r.addend };
    let addend = l.addend.wrapping_add(rhs_addend);

    match (l.target, r.target) {
        (None, None) => Ok(RelocValue::constant(addend)),
        (Some(t), None) => Ok(RelocValue { addend, target: Some(t), byte_op: ByteOp::None }),
        (None, Some(t)) => {
            if subtract {
                return Err(AsmError::new("cannot subtract from a relocatable symbol"));
            }
            Ok(RelocValue { addend, target: Some(t), byte_op: ByteOp::None })
        }
        (Some(lt), Some(rt)) => {
            // Only a same-section difference folds to a constant.
            if subtract {
                if let (RelocTarget::Section(a), RelocTarget::Section(b)) = (&lt, &rt) {
                    if a == b {
                        return Ok(RelocValue::constant(addend));
                    }
                }
                Err(AsmError::new(
                    "relocatable difference across sections or symbols is not supported",
                ))
            } else {
                Err(AsmError::new(
                    "cannot add two relocatable symbols",
                ))
            }
        }
    }
}

fn eval_binary_const(op: BinaryOp, l: i64, r: i64) -> AsmResult<i64> {
    Ok(match op {
        BinaryOp::Add => l.wrapping_add(r),
        BinaryOp::Sub => l.wrapping_sub(r),
        BinaryOp::Mul => l.wrapping_mul(r),
        BinaryOp::Div => {
            if r == 0 {
                return Err(AsmError::new("Division by zero"));
            }
            l / r
        }
        BinaryOp::Mod => {
            if r == 0 {
                return Err(AsmError::new("Modulo by zero"));
            }
            l % r
        }
        BinaryOp::Shl => l.wrapping_shl(r as u32),
        BinaryOp::Shr => l.wrapping_shr(r as u32),
        BinaryOp::Lt => if l < r { 1 } else { 0 },
        BinaryOp::Le => if l <= r { 1 } else { 0 },
        BinaryOp::Gt => if l > r { 1 } else { 0 },
        BinaryOp::Ge => if l >= r { 1 } else { 0 },
        BinaryOp::Eq => if l == r { 1 } else { 0 },
        BinaryOp::Ne => if l != r { 1 } else { 0 },
        BinaryOp::BitAnd => l & r,
        BinaryOp::BitXor => l ^ r,
        BinaryOp::BitOr => l | r,
        BinaryOp::LogAnd => if l != 0 && r != 0 { 1 } else { 0 },
        BinaryOp::LogOr => if l != 0 || r != 0 { 1 } else { 0 },
    })
}

/// Expression parser using Pratt parsing / recursive descent
pub struct ExprParser<'a> {
    tokens: &'a [LocatedToken],
    pos: usize,
}

impl<'a> ExprParser<'a> {
    pub fn new(tokens: &'a [LocatedToken]) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|t| &t.value)
    }

    /// Location of the current token, or the last token if the parser is at
    /// the end of input. Returns `None` only when there are no tokens at all.
    fn cur_loc(&self) -> Option<SourceLocation> {
        self.tokens
            .get(self.pos)
            .or_else(|| self.tokens.last())
            .map(|t| t.loc.clone())
    }

    /// Build an error annotated with the current token location.
    fn error(&self, message: impl Into<String>) -> AsmError {
        let err = AsmError::new(message);
        match self.cur_loc() {
            Some(loc) => err.with_location(loc),
            None => err,
        }
    }

    fn advance(&mut self) -> Option<&LocatedToken> {
        if self.pos < self.tokens.len() {
            let t = &self.tokens[self.pos];
            self.pos += 1;
            Some(t)
        } else {
            None
        }
    }

    /// Parse a full expression
    pub fn parse_expr(&mut self) -> AsmResult<Expr> {
        self.parse_binary(0)
    }

    fn parse_binary(&mut self, min_prec: u8) -> AsmResult<Expr> {
        let mut left = self.parse_unary()?;

        loop {
            let op = match self.peek_binary_op() {
                Some(op) if op.precedence() >= min_prec => op,
                _ => break,
            };

            self.advance(); // consume operator
            let right = self.parse_binary(op.precedence() + 1)?;
            left = Expr::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    fn peek_binary_op(&self) -> Option<BinaryOp> {
        match self.peek()? {
            Token::Operator(s) => match s.as_str() {
                "+" => Some(BinaryOp::Add),
                "-" => Some(BinaryOp::Sub),
                "*" => Some(BinaryOp::Mul),
                "/" => Some(BinaryOp::Div),
                "%" => Some(BinaryOp::Mod),
                "<<" => Some(BinaryOp::Shl),
                ">>" => Some(BinaryOp::Shr),
                "<" => Some(BinaryOp::Lt),
                "<=" => Some(BinaryOp::Le),
                ">" => Some(BinaryOp::Gt),
                ">=" => Some(BinaryOp::Ge),
                "==" => Some(BinaryOp::Eq),
                "!=" => Some(BinaryOp::Ne),
                "&" => Some(BinaryOp::BitAnd),
                "^" => Some(BinaryOp::BitXor),
                "|" => Some(BinaryOp::BitOr),
                "&&" => Some(BinaryOp::LogAnd),
                "||" => Some(BinaryOp::LogOr),
                _ => None,
            },
            _ => None,
        }
    }

    fn parse_unary(&mut self) -> AsmResult<Expr> {
        match self.peek() {
            Some(Token::Operator(s)) => {
                let op = match s.as_str() {
                    "+" => Some(UnaryOp::Plus),
                    "-" => Some(UnaryOp::Minus),
                    "!" => Some(UnaryOp::Not),
                    "~" => Some(UnaryOp::BitNot),
                    "<" => Some(UnaryOp::LowByte),
                    ">" => Some(UnaryOp::HighByte),
                    _ => None,
                };
                if let Some(op) = op {
                    self.advance();
                    let expr = self.parse_unary()?;
                    return Ok(Expr::UnaryOp {
                        op,
                        expr: Box::new(expr),
                    });
                }
                self.parse_primary()
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> AsmResult<Expr> {
        match self.peek().cloned() {
            Some(Token::Number(n)) => {
                self.advance();
                Ok(Expr::Number(n))
            }
            Some(Token::CharLiteral(c)) => {
                self.advance();
                Ok(Expr::Number(c as i64))
            }
            Some(Token::Operator(ref s)) if s == "*" => {
                self.advance();
                Ok(Expr::CurrentPC)
            }
            Some(Token::OpenParen) => {
                self.advance();
                let expr = self.parse_expr()?;
                match self.peek() {
                    Some(Token::CloseParen) => {
                        self.advance();
                        Ok(expr)
                    }
                    _ => Err(self.error("Expected closing parenthesis")),
                }
            }
            Some(Token::At) => {
                self.advance();
                match self.peek().cloned() {
                    Some(Token::Identifier(name)) => {
                        self.advance();
                        Ok(Expr::LocalSymbol(name))
                    }
                    _ => Err(self.error("Expected identifier after @")),
                }
            }
            Some(Token::Identifier(ref name)) => {
                let upper = name.to_uppercase();
                match upper.as_str() {
                    "TRUE" => {
                        self.advance();
                        Ok(Expr::BoolLiteral(true))
                    }
                    "FALSE" => {
                        self.advance();
                        Ok(Expr::BoolLiteral(false))
                    }
                    _ => {
                        let mut name = name.clone();
                        self.advance();
                        // Macro-scoped globals are addressable by their
                        // fully-qualified name `MacroName_<call-index>.Symbol`.
                        // A `.` cannot occur in an ordinary identifier, so
                        // folding `Identifier . Identifier` into one symbol is
                        // unambiguous.
                        if matches!(self.peek(), Some(Token::Dot)) {
                            self.advance();
                            match self.peek().cloned() {
                                Some(Token::Identifier(part)) => {
                                    self.advance();
                                    name.push('.');
                                    name.push_str(&part);
                                }
                                _ => return Err(self.error("Expected identifier after '.'")),
                            }
                        }
                        Ok(Expr::Symbol(name))
                    }
                }
            }
            Some(Token::StringLiteral(_)) => {
                // StringLiteral in an expression context - shouldn't happen normally
                Err(self.error("String literals not allowed in expressions"))
            }
            _ => Err(self.error("Expected expression")),
        }
    }
}

/// Convenience function to parse an expression from a token slice
pub fn parse_expression(tokens: &[LocatedToken]) -> AsmResult<(Expr, usize)> {
    let mut parser = ExprParser::new(tokens);
    let expr = parser.parse_expr()?;
    Ok((expr, parser.pos()))
}
