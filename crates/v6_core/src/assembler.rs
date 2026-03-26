use std::collections::HashMap;
use std::path::PathBuf;

use crate::diagnostics::{AsmError, AsmResult, SourceLocation};
use crate::encoding::{Encoding, EncodingCase, EncodingType};
use crate::expr::{eval_expr, Expr};
use crate::instructions::{encode_instruction, ParsedOperand};
use crate::lexer::tokenize_line;
use crate::parser::{self, Directive, ParsedLine, PrintArg, TextItem};
use crate::preprocessor::{SourceLine, expand_macro, parse_macro_invocation};
use crate::project::CpuMode;
use crate::symbols::SymbolTable;

const MAX_LOOP_ITERATIONS: usize = 100_000;

/// Output buffer for assembled code (sparse 64KB address space)
pub struct OutputBuffer {
    data: Vec<Option<u8>>,
    min_addr: Option<u16>,
    max_addr: Option<u16>,
}

impl OutputBuffer {
    pub fn new() -> Self {
        Self {
            data: vec![None; 65536],
            min_addr: None,
            max_addr: None,
        }
    }

    pub fn write_byte(&mut self, addr: u16, byte: u8) {
        self.data[addr as usize] = Some(byte);
        self.min_addr = Some(self.min_addr.map_or(addr, |m: u16| m.min(addr)));
        self.max_addr = Some(self.max_addr.map_or(addr, |m: u16| m.max(addr)));
    }

    pub fn write_bytes(&mut self, start_addr: u16, bytes: &[u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            self.write_byte(start_addr.wrapping_add(i as u16), b);
        }
    }

    /// Extract the contiguous ROM bytes
    pub fn extract_rom(&self) -> Vec<u8> {
        let min = match self.min_addr {
            Some(a) => a as usize,
            None => return Vec::new(),
        };
        let max = match self.max_addr {
            Some(a) => a as usize,
            None => return Vec::new(),
        };
        let mut rom = Vec::with_capacity(max - min + 1);
        for i in min..=max {
            rom.push(self.data[i].unwrap_or(0));
        }
        rom
    }

    pub fn min_addr(&self) -> Option<u16> {
        self.min_addr
    }

    pub fn max_addr(&self) -> Option<u16> {
        self.max_addr
    }
}

/// Debug info collected during assembly
#[derive(Debug, Default)]
pub struct DebugInfo {
    pub labels: HashMap<String, LabelInfo>,
    pub consts: HashMap<String, ConstInfo>,
    pub macros: HashMap<String, MacroDebugInfo>,
    pub line_addresses: HashMap<String, HashMap<usize, Vec<u16>>>,
    pub data_lines: HashMap<String, HashMap<usize, DataLineInfo>>,
}

#[derive(Debug, Clone)]
pub struct LabelInfo {
    pub addr: u16,
    pub src: String,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct ConstInfo {
    pub value: i64,
    pub line: usize,
    pub src: String,
}

#[derive(Debug, Clone)]
pub struct MacroDebugInfo {
    pub src: String,
    pub line: usize,
    pub params: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DataLineInfo {
    pub addr: u16,
    pub byte_length: usize,
    pub unit_bytes: usize,
}

/// Assembler settings that can be modified by .setting
#[derive(Debug, Clone)]
pub struct AssemblerSettings {
    pub optional_enabled: bool,
}

impl Default for AssemblerSettings {
    fn default() -> Self {
        Self {
            optional_enabled: true,
        }
    }
}

/// The main assembler context
pub struct Assembler {
    pub symbols: SymbolTable,
    pub output: OutputBuffer,
    pub debug_info: DebugInfo,
    pub pc: u16,
    pub cpu_mode: CpuMode,
    pub encoding: Encoding,
    pub settings: AssemblerSettings,
    pub errors: Vec<AsmError>,
    pub quiet: bool,
    project_dir: PathBuf,

    // Tracking for .optional blocks
    optional_stack: Vec<OptionalBlock>,
    optional_blocks: Vec<OptionalBlockInfo>,

    // Loop/if expansion depth tracking
    macro_depth: usize,
}

struct OptionalBlock {
    start_idx: usize,
    symbols_defined: Vec<String>,
}

struct OptionalBlockInfo {
    start_line_idx: usize,
    end_line_idx: usize,
    symbols_defined: Vec<String>,
}

impl Assembler {
    pub fn new(cpu_mode: CpuMode, project_dir: PathBuf) -> Self {
        Self {
            symbols: SymbolTable::new(),
            output: OutputBuffer::new(),
            debug_info: DebugInfo::default(),
            pc: 0,
            cpu_mode,
            encoding: Encoding::default(),
            settings: AssemblerSettings::default(),
            errors: Vec::new(),
            quiet: false,
            project_dir,
            optional_stack: Vec::new(),
            optional_blocks: Vec::new(),
            macro_depth: 0,
        }
    }

    /// Assemble preprocessed source lines (two-pass)
    pub fn assemble(&mut self, lines: &[SourceLine]) -> AsmResult<()> {
        // Pass 1: Collect symbols and sizes
        self.pass1(lines)?;

        // Resolve deferred constants
        self.resolve_deferred_constants()?;

        // Pass 2: Generate code
        self.symbols.reset_for_pass2();
        self.symbols.reset_macro_call_count();
        self.pc = 0;
        self.encoding = Encoding::default();
        self.pass2(lines)?;

        Ok(())
    }

    fn pass1(&mut self, lines: &[SourceLine]) -> AsmResult<()> {
        self.process_lines_pass1(lines)
    }

    fn process_lines_pass1(&mut self, lines: &[SourceLine]) -> AsmResult<()> {
        let mut i = 0;
        while i < lines.len() {
            let line = &lines[i];

            if let Some((macro_name, args)) = parse_macro_invocation(&line.text, &self.symbols) {
                self.expand_macro_pass1(line, &macro_name, &args)?;
                i += 1;
                continue;
            }

            let tokens = tokenize_line(&line.text, &line.file, line.line_num)?;
            if tokens.is_empty() {
                i += 1;
                continue;
            }

            let parsed = parser::parse_line(&tokens, self.cpu_mode)?;
            if parsed.len() == 1 {
                if let Some(control) = Self::control_directive(&parsed[0]) {
                    match control {
                        ControlDirective::If(expr) => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::If)?;
                            if self.eval_expr(expr)? != 0 {
                                self.process_lines_pass1(&lines[i + 1..end])?;
                            }
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::Loop(expr) => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::Loop)?;
                            let count = self.eval_expr(expr)?;
                            if count < 0 {
                                return Err(AsmError::new("Loop count must be non-negative"));
                            }
                            if count as usize > MAX_LOOP_ITERATIONS {
                                return Err(AsmError::new(format!(
                                    "Loop iteration count exceeded {}",
                                    MAX_LOOP_ITERATIONS
                                )));
                            }
                            for _ in 0..count as usize {
                                self.process_lines_pass1(&lines[i + 1..end])?;
                            }
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::Optional => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::Optional)?;
                            if !self.settings.optional_enabled
                                || self.should_include_optional_block(lines, i + 1, end)?
                            {
                                self.process_lines_pass1(&lines[i + 1..end])?;
                            }
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::EndIf
                        | ControlDirective::EndLoop
                        | ControlDirective::EndOptional => {
                            i += 1;
                            continue;
                        }
                    }
                }
            }

            self.process_parsed_line_pass1(line, &parsed)?;
            i += 1;
        }
        Ok(())
    }

    fn process_parsed_line_pass1(&mut self, line: &SourceLine, parsed: &[ParsedLine]) -> AsmResult<()> {

        for item in parsed {
            match item {
                ParsedLine::Empty => {}
                ParsedLine::Label(name) => {
                    self.symbols.define_label(name, self.pc, &line.file, line.line_num)?;
                }
                ParsedLine::LocalLabel(name) => {
                    self.symbols.define_local_label(name, self.pc, &line.file, line.line_num)?;
                }
                ParsedLine::ConstDef { name, is_local, expr } => {
                    // Try to evaluate immediately, defer if forward reference
                    let resolver = |sym: &str| -> Option<i64> {
                        self.symbols.resolve(sym)
                    };
                    match eval_expr(expr, &resolver, self.pc) {
                        Ok(val) => {
                            if *is_local {
                                self.symbols.define_local_constant(name, val, &line.file, line.line_num)?;
                            } else {
                                if self.symbols.is_mutable(name) {
                                    self.symbols.update_variable(name, val)?;
                                } else if self.symbols.exists(name) {
                                    self.symbols.define_constant(name, val, &line.file, line.line_num)?;
                                } else {
                                    self.symbols.define_constant(name, val, &line.file, line.line_num)?;
                                }
                            }
                        }
                        Err(_) => {
                            // Defer evaluation
                            if !*is_local {
                                self.symbols.define_constant_deferred(name, expr.clone(), &line.file, line.line_num)?;
                            }
                        }
                    }

                    // Record address for this line
                    self.record_line_address(&line.file, line.line_num, self.pc);
                }
                ParsedLine::VarDef { name, expr } => {
                    let resolver = |sym: &str| -> Option<i64> {
                        self.symbols.resolve(sym)
                    };
                    if let Ok(val) = eval_expr(expr, &resolver, self.pc) {
                        self.symbols.define_variable(name, val, &line.file, line.line_num)?;
                    }
                }
                ParsedLine::Instruction { mnemonic, operands, .. } => {
                    let size = self.instruction_size(mnemonic, operands)?;
                    self.record_line_address(&line.file, line.line_num, self.pc);
                    self.pc = self.pc.wrapping_add(size as u16);
                }
                ParsedLine::Directive(dir) => {
                    self.process_directive_pass1(dir, &line.file, line.line_num)?;
                }
            }
        }
        Ok(())
    }

    fn process_directive_pass1(&mut self, dir: &Directive, file: &str, line_num: usize) -> AsmResult<()> {
        match dir {
            Directive::Org(expr) => {
                let val = self.eval_expr(expr)?;
                self.pc = val as u16;
                self.record_line_address(file, line_num, self.pc);
            }
            Directive::Align(expr) => {
                let alignment = self.eval_expr(expr)? as u16;
                if alignment > 0 {
                    let mask = alignment - 1;
                    if self.pc & mask != 0 {
                        self.pc = (self.pc | mask) + 1;
                    }
                }
            }
            Directive::Storage { length, filler } => {
                let len = self.eval_expr(length)? as u16;
                self.record_line_address(file, line_num, self.pc);
                self.pc = self.pc.wrapping_add(len);
            }
            Directive::Byte(exprs) => {
                self.record_line_address(file, line_num, self.pc);
                self.pc = self.pc.wrapping_add(exprs.len() as u16);
            }
            Directive::Word(exprs) => {
                self.record_line_address(file, line_num, self.pc);
                self.pc = self.pc.wrapping_add((exprs.len() * 2) as u16);
            }
            Directive::Dword(exprs) => {
                self.record_line_address(file, line_num, self.pc);
                self.pc = self.pc.wrapping_add((exprs.len() * 4) as u16);
            }
            Directive::Text(items) => {
                self.record_line_address(file, line_num, self.pc);
                let byte_count = self.text_byte_count(items);
                self.pc = self.pc.wrapping_add(byte_count as u16);
            }
            Directive::Encoding { enc_type, case } => {
                if let Some(et) = EncodingType::from_str(enc_type) {
                    self.encoding.encoding_type = et;
                }
                if let Some(c) = case {
                    if let Some(ec) = EncodingCase::from_str(c) {
                        self.encoding.case = ec;
                    }
                }
            }
            Directive::Setting(pairs) => {
                for (key, val) in pairs {
                    if key.eq_ignore_ascii_case("optional") {
                        self.settings.optional_enabled = !val.eq_ignore_ascii_case("false");
                    }
                }
            }
            Directive::If(_) | Directive::EndIf | Directive::Loop(_) | Directive::EndLoop => {
                // These should have been expanded in preprocessing for simple cases
                // For now, handle in-line during pass processing
                self.record_line_address(file, line_num, self.pc);
            }
            Directive::Optional | Directive::EndOptional => {
                self.record_line_address(file, line_num, self.pc);
            }
            Directive::IncBin { path, offset, length } => {
                self.record_line_address(file, line_num, self.pc);
                // For pass 1 we need to know the size
                let resolved = self.resolve_file_path(path)?;
                let file_len = std::fs::metadata(&resolved)
                    .map_err(|e| AsmError::new(format!("Cannot read {}: {}", path, e)))?
                    .len() as usize;
                let off = offset.as_ref().map(|e| self.eval_expr(e).unwrap_or(0) as usize).unwrap_or(0);
                let len = length.as_ref().map(|e| self.eval_expr(e).unwrap_or(0) as usize).unwrap_or(file_len - off);
                self.pc = self.pc.wrapping_add(len as u16);
            }
            Directive::FileSize { name, path } => {
                let resolved = self.resolve_file_path(path)?;
                let size = std::fs::metadata(&resolved)
                    .map_err(|e| AsmError::new(format!("Cannot stat {}: {}", path, e)))?
                    .len() as i64;
                if !name.is_empty() {
                    self.symbols.define_constant(name, size, file, line_num)?;
                }
                self.record_line_address(file, line_num, self.pc);
            }
            Directive::Include(_) => {
                // Should have been expanded already
            }
            Directive::MacroDef { .. } | Directive::EndMacro => {
                // Should have been collected already
            }
            Directive::Print(_) | Directive::Error(_) => {
                // Only processed in pass 2
            }
        }
        Ok(())
    }

    fn pass2(&mut self, lines: &[SourceLine]) -> AsmResult<()> {
        self.process_lines_pass2(lines)
    }

    fn process_lines_pass2(&mut self, lines: &[SourceLine]) -> AsmResult<()> {
        let mut i = 0;
        while i < lines.len() {
            let line = &lines[i];

            if let Some((macro_name, args)) = parse_macro_invocation(&line.text, &self.symbols) {
                self.expand_macro_pass2(line, &macro_name, &args)?;
                i += 1;
                continue;
            }

            let tokens = tokenize_line(&line.text, &line.file, line.line_num)?;
            if tokens.is_empty() {
                i += 1;
                continue;
            }

            let parsed = parser::parse_line(&tokens, self.cpu_mode)?;
            if parsed.len() == 1 {
                if let Some(control) = Self::control_directive(&parsed[0]) {
                    match control {
                        ControlDirective::If(expr) => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::If)?;
                            if self.eval_expr(expr)? != 0 {
                                self.process_lines_pass2(&lines[i + 1..end])?;
                            }
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::Loop(expr) => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::Loop)?;
                            let count = self.eval_expr(expr)?;
                            if count < 0 {
                                return Err(AsmError::new("Loop count must be non-negative"));
                            }
                            if count as usize > MAX_LOOP_ITERATIONS {
                                return Err(AsmError::new(format!(
                                    "Loop iteration count exceeded {}",
                                    MAX_LOOP_ITERATIONS
                                )));
                            }
                            for _ in 0..count as usize {
                                self.process_lines_pass2(&lines[i + 1..end])?;
                            }
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::Optional => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::Optional)?;
                            if !self.settings.optional_enabled
                                || self.should_include_optional_block(lines, i + 1, end)?
                            {
                                self.process_lines_pass2(&lines[i + 1..end])?;
                            }
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::EndIf
                        | ControlDirective::EndLoop
                        | ControlDirective::EndOptional => {
                            i += 1;
                            continue;
                        }
                    }
                }
            }

            self.process_parsed_line_pass2(line, &parsed)?;
            i += 1;
        }
        Ok(())
    }

    fn process_parsed_line_pass2(&mut self, line: &SourceLine, parsed: &[ParsedLine]) -> AsmResult<()> {

        for item in parsed {
            match item {
                ParsedLine::Empty => {}
                ParsedLine::Label(name) => {
                    self.symbols.define_label(name, self.pc, &line.file, line.line_num)?;
                    self.debug_info.labels.insert(name.clone(), LabelInfo {
                        addr: self.pc,
                        src: line.file.clone(),
                        line: line.line_num,
                    });
                }
                ParsedLine::LocalLabel(name) => {
                    self.symbols.define_local_label(name, self.pc, &line.file, line.line_num)?;
                    // Add to debug with disambiguation suffix
                    let idx = self.debug_info.labels.iter()
                        .filter(|(k, _)| k.starts_with(&format!("@{}_", name)))
                        .count();
                    let debug_name = format!("@{}_{}", name, idx);
                    self.debug_info.labels.insert(debug_name, LabelInfo {
                        addr: self.pc,
                        src: line.file.clone(),
                        line: line.line_num,
                    });
                }
                ParsedLine::ConstDef { name, is_local, expr } => {
                    let val = self.eval_expr(expr)?;
                    if *is_local {
                        self.symbols.define_local_constant(name, val, &line.file, line.line_num)?;
                    } else {
                        if self.symbols.is_mutable(name) {
                            self.symbols.update_variable(name, val)?;
                        } else if self.symbols.exists(name) {
                            self.symbols.define_constant(name, val, &line.file, line.line_num)?;
                        } else {
                            self.symbols.define_constant(name, val, &line.file, line.line_num)?;
                        }
                        self.debug_info.consts.insert(name.clone(), ConstInfo {
                            value: val,
                            line: line.line_num,
                            src: line.file.clone(),
                        });
                    }
                    self.record_line_address(&line.file, line.line_num, self.pc);
                }
                ParsedLine::VarDef { name, expr } => {
                    let val = self.eval_expr(expr)?;
                    if self.symbols.exists(name) {
                        let _ = self.symbols.update_variable(name, val);
                    } else {
                        self.symbols.define_variable(name, val, &line.file, line.line_num)?;
                    }
                }
                ParsedLine::Instruction { mnemonic, operands, expressions } => {
                    self.record_line_address(&line.file, line.line_num, self.pc);
                    self.emit_instruction(mnemonic, operands, expressions)?;
                }
                ParsedLine::Directive(dir) => {
                    self.process_directive_pass2(dir, &line.file, line.line_num)?;
                }
            }
        }
        Ok(())
    }

    fn expand_macro_pass1(&mut self, line: &SourceLine, macro_name: &str, args: &[String]) -> AsmResult<()> {
        if self.macro_depth >= 32 {
            return Err(AsmError::new("Macro expansion depth exceeded 32 levels"));
        }
        let macro_def = self.symbols.get_macro(macro_name).unwrap().clone();
        let call_idx = self.symbols.macro_call_count() + 1;
        let expanded = expand_macro(&macro_def, args, call_idx, &line.file, line.line_num)?;
        self.symbols.begin_macro_expansion(macro_name);
        self.macro_depth += 1;
        let result = self.process_lines_pass1(&expanded);
        self.macro_depth -= 1;
        self.symbols.end_macro_expansion();
        result
    }

    fn expand_macro_pass2(&mut self, line: &SourceLine, macro_name: &str, args: &[String]) -> AsmResult<()> {
        if self.macro_depth >= 32 {
            return Err(AsmError::new("Macro expansion depth exceeded 32 levels"));
        }
        let macro_def = self.symbols.get_macro(macro_name).unwrap().clone();
        let call_idx = self.symbols.macro_call_count() + 1;
        let expanded = expand_macro(&macro_def, args, call_idx, &line.file, line.line_num)?;
        self.symbols.begin_macro_expansion(macro_name);
        self.macro_depth += 1;
        let result = self.process_lines_pass2(&expanded);
        self.macro_depth -= 1;
        self.symbols.end_macro_expansion();
        result
    }

    fn control_directive<'a>(parsed: &'a ParsedLine) -> Option<ControlDirective<'a>> {
        match parsed {
            ParsedLine::Directive(Directive::If(expr)) => Some(ControlDirective::If(expr)),
            ParsedLine::Directive(Directive::EndIf) => Some(ControlDirective::EndIf),
            ParsedLine::Directive(Directive::Loop(expr)) => Some(ControlDirective::Loop(expr)),
            ParsedLine::Directive(Directive::EndLoop) => Some(ControlDirective::EndLoop),
            ParsedLine::Directive(Directive::Optional) => Some(ControlDirective::Optional),
            ParsedLine::Directive(Directive::EndOptional) => Some(ControlDirective::EndOptional),
            _ => None,
        }
    }

    fn find_matching_block_end(&self, lines: &[SourceLine], start: usize, kind: BlockKind) -> AsmResult<usize> {
        let mut depth = 0usize;
        for (idx, line) in lines.iter().enumerate().skip(start) {
            let tokens = tokenize_line(&line.text, &line.file, line.line_num)?;
            if tokens.is_empty() {
                continue;
            }
            let parsed = parser::parse_line(&tokens, self.cpu_mode)?;
            if parsed.len() != 1 {
                continue;
            }
            if let Some(control) = Self::control_directive(&parsed[0]) {
                match (kind, control) {
                    (BlockKind::If, ControlDirective::If(_))
                    | (BlockKind::Loop, ControlDirective::Loop(_))
                    | (BlockKind::Optional, ControlDirective::Optional) => {
                        depth += 1;
                    }
                    (BlockKind::If, ControlDirective::EndIf)
                    | (BlockKind::Loop, ControlDirective::EndLoop)
                    | (BlockKind::Optional, ControlDirective::EndOptional) => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(idx);
                        }
                    }
                    _ => {}
                }
            }
        }
        Err(AsmError::new(format!("Missing {}", kind.end_directive_name())))
    }

    fn should_include_optional_block(&self, lines: &[SourceLine], block_start: usize, block_end: usize) -> AsmResult<bool> {
        let defined = self.collect_optional_block_symbols(&lines[block_start..block_end])?;
        if defined.is_empty() {
            return Ok(false);
        }

        for (idx, line) in lines.iter().enumerate() {
            if idx >= block_start && idx < block_end {
                continue;
            }
            let tokens = tokenize_line(&line.text, &line.file, line.line_num)?;
            if tokens.is_empty() {
                continue;
            }
            for token in &tokens {
                if let crate::lexer::Token::Identifier(name) = &token.value {
                    if defined.iter().any(|defined_name| defined_name == name) {
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }

    fn collect_optional_block_symbols(&self, lines: &[SourceLine]) -> AsmResult<Vec<String>> {
        let mut names = Vec::new();
        for line in lines {
            let tokens = tokenize_line(&line.text, &line.file, line.line_num)?;
            if tokens.is_empty() {
                continue;
            }
            let parsed = parser::parse_line(&tokens, self.cpu_mode)?;
            for item in parsed {
                match item {
                    ParsedLine::Label(name) => names.push(name),
                    ParsedLine::ConstDef { name, is_local: false, .. } => names.push(name),
                    ParsedLine::VarDef { name, .. } => names.push(name),
                    _ => {}
                }
            }
        }
        Ok(names)
    }

    fn process_directive_pass2(&mut self, dir: &Directive, file: &str, line_num: usize) -> AsmResult<()> {
        match dir {
            Directive::Org(expr) => {
                let val = self.eval_expr(expr)?;
                self.pc = val as u16;
                self.record_line_address(file, line_num, self.pc);
            }
            Directive::Align(expr) => {
                let alignment = self.eval_expr(expr)? as u16;
                if alignment > 0 {
                    let mask = alignment - 1;
                    while self.pc & mask != 0 {
                        self.output.write_byte(self.pc, 0);
                        self.pc = self.pc.wrapping_add(1);
                    }
                }
            }
            Directive::Storage { length, filler } => {
                let len = self.eval_expr(length)? as u16;
                let fill = filler.as_ref().map(|e| self.eval_expr(e)).transpose()?.map(|v| v as u8);
                self.record_line_address(file, line_num, self.pc);
                if let Some(f) = fill {
                    self.record_data_line(file, line_num, self.pc, len as usize, 1);
                    for _ in 0..len {
                        self.output.write_byte(self.pc, f);
                        self.pc = self.pc.wrapping_add(1);
                    }
                } else {
                    self.record_data_line(file, line_num, self.pc, len as usize, 1);
                    self.pc = self.pc.wrapping_add(len);
                }
            }
            Directive::Byte(exprs) => {
                self.record_line_address(file, line_num, self.pc);
                self.record_data_line(file, line_num, self.pc, exprs.len(), 1);
                for expr in exprs {
                    let val = self.eval_expr(expr)? as u8;
                    self.output.write_byte(self.pc, val);
                    self.pc = self.pc.wrapping_add(1);
                }
            }
            Directive::Word(exprs) => {
                self.record_line_address(file, line_num, self.pc);
                self.record_data_line(file, line_num, self.pc, exprs.len() * 2, 2);
                for expr in exprs {
                    let val = self.eval_expr(expr)? as u16;
                    self.output.write_byte(self.pc, (val & 0xFF) as u8);
                    self.pc = self.pc.wrapping_add(1);
                    self.output.write_byte(self.pc, ((val >> 8) & 0xFF) as u8);
                    self.pc = self.pc.wrapping_add(1);
                }
            }
            Directive::Dword(exprs) => {
                self.record_line_address(file, line_num, self.pc);
                self.record_data_line(file, line_num, self.pc, exprs.len() * 4, 4);
                for expr in exprs {
                    let val = self.eval_expr(expr)? as u32;
                    for i in 0..4 {
                        self.output.write_byte(self.pc, ((val >> (i * 8)) & 0xFF) as u8);
                        self.pc = self.pc.wrapping_add(1);
                    }
                }
            }
            Directive::Text(items) => {
                self.record_line_address(file, line_num, self.pc);
                let bytes = self.encode_text_items(items);
                let byte_count = bytes.len();
                for b in bytes {
                    self.output.write_byte(self.pc, b);
                    self.pc = self.pc.wrapping_add(1);
                }
            }
            Directive::Encoding { enc_type, case } => {
                if let Some(et) = EncodingType::from_str(enc_type) {
                    self.encoding.encoding_type = et;
                }
                if let Some(c) = case {
                    if let Some(ec) = EncodingCase::from_str(c) {
                        self.encoding.case = ec;
                    }
                }
            }
            Directive::Setting(pairs) => {
                for (key, val) in pairs {
                    if key.eq_ignore_ascii_case("optional") {
                        self.settings.optional_enabled = !val.eq_ignore_ascii_case("false");
                    }
                }
            }
            Directive::Print(args) if !self.quiet => {
                let mut output = String::new();
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 { output.push(' '); }
                    match arg {
                        PrintArg::Str(s) => output.push_str(s),
                        PrintArg::Expr(expr) => {
                            let val = self.eval_expr(expr)?;
                            output.push_str(&val.to_string());
                        }
                    }
                }
                eprintln!("{}", output);
            }
            Directive::Print(_) => {}
            Directive::Error(args) => {
                let mut output = String::new();
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 { output.push(' '); }
                    match arg {
                        PrintArg::Str(s) => output.push_str(s),
                        PrintArg::Expr(expr) => {
                            let val = self.eval_expr(expr)?;
                            output.push_str(&val.to_string());
                        }
                    }
                }
                return Err(AsmError::new(output)
                    .with_location(SourceLocation {
                        file: file.to_string(),
                        line: line_num,
                        col: 1,
                    }));
            }
            Directive::IncBin { path, offset, length } => {
                self.record_line_address(file, line_num, self.pc);
                let resolved = self.resolve_file_path(path)?;
                let data = std::fs::read(&resolved)
                    .map_err(|e| AsmError::new(format!("Cannot read {}: {}", path, e)))?;
                let off = offset.as_ref().map(|e| self.eval_expr(e).unwrap_or(0) as usize).unwrap_or(0);
                let len = length.as_ref().map(|e| self.eval_expr(e).unwrap_or(0) as usize).unwrap_or(data.len() - off);
                let slice = &data[off..off + len];
                for &b in slice {
                    self.output.write_byte(self.pc, b);
                    self.pc = self.pc.wrapping_add(1);
                }
            }
            Directive::FileSize { name, path } => {
                let resolved = self.resolve_file_path(path)?;
                let size = std::fs::metadata(&resolved)
                    .map_err(|e| AsmError::new(format!("Cannot stat {}: {}", path, e)))?
                    .len() as i64;
                if !name.is_empty() {
                    if self.symbols.exists(name) {
                        let _ = self.symbols.update_variable(name, size);
                    } else {
                        self.symbols.define_constant(name, size, file, line_num)?;
                    }
                }
                self.record_line_address(file, line_num, self.pc);
            }
            Directive::If(_) | Directive::EndIf | Directive::Loop(_) | Directive::EndLoop => {
                self.record_line_address(file, line_num, self.pc);
            }
            Directive::Optional | Directive::EndOptional => {
                self.record_line_address(file, line_num, self.pc);
            }
            Directive::Include(_) | Directive::MacroDef { .. } | Directive::EndMacro => {}
        }
        Ok(())
    }

    fn emit_instruction(&mut self, mnemonic: &str, operands: &[ParsedOperand], expressions: &[Expr]) -> AsmResult<()> {
        let mut encoded = encode_instruction(mnemonic, operands, self.cpu_mode)?;

        // Fill in immediate values from expressions
        let mut expr_idx = 0;
        if encoded.has_imm8 && expr_idx < expressions.len() {
            let val = self.eval_expr(&expressions[expr_idx])? as u8;
            encoded.bytes[1] = val;
            expr_idx += 1;
        }
        if encoded.has_imm16 && expr_idx < expressions.len() {
            let val = self.eval_expr(&expressions[expr_idx])? as u16;
            encoded.bytes[1] = (val & 0xFF) as u8;
            encoded.bytes[2] = ((val >> 8) & 0xFF) as u8;
        }

        self.output.write_bytes(self.pc, &encoded.bytes);
        self.pc = self.pc.wrapping_add(encoded.size as u16);
        Ok(())
    }

    fn instruction_size(&self, mnemonic: &str, operands: &[ParsedOperand]) -> AsmResult<usize> {
        let encoded = encode_instruction(mnemonic, operands, self.cpu_mode)?;
        Ok(encoded.size)
    }

    fn eval_expr(&self, expr: &Expr) -> AsmResult<i64> {
        let symbols = &self.symbols;
        eval_expr(expr, &|name| {
            symbols.resolve(name).or_else(|| symbols.resolve_local(name))
        }, self.pc)
    }

    fn resolve_deferred_constants(&mut self) -> AsmResult<()> {
        // Multiple passes until all constants are resolved
        for _ in 0..100 {
            let mut any_resolved = false;
            let unresolved: Vec<_> = self.symbols.all_globals()
                .iter()
                .filter(|(_, info)| info.value.is_none() && info.expr.is_some())
                .map(|(name, info)| (name.clone(), info.expr.clone().unwrap(), info.file.clone(), info.line))
                .collect();

            if unresolved.is_empty() {
                return Ok(());
            }

            for (name, expr, file, line) in unresolved {
                let resolver = |sym: &str| -> Option<i64> {
                    self.symbols.resolve(sym)
                };
                if let Ok(val) = eval_expr(&expr, &resolver, 0) {
                    self.symbols.define_constant(&name, val, &file, line)?;
                    any_resolved = true;
                }
            }

            if !any_resolved {
                // Check if there are still unresolved symbols
                let still_unresolved: Vec<_> = self.symbols.all_globals()
                    .iter()
                    .filter(|(_, info)| info.value.is_none())
                    .map(|(name, _)| name.clone())
                    .collect();
                if !still_unresolved.is_empty() {
                    return Err(AsmError::new(format!(
                        "Unresolved symbols: {}", still_unresolved.join(", ")
                    )));
                }
                break;
            }
        }
        Ok(())
    }

    fn text_byte_count(&self, items: &[TextItem]) -> usize {
        items.iter().map(|item| match item {
            TextItem::Str(s) => s.len(),
            TextItem::Char(_) => 1,
        }).sum()
    }

    fn encode_text_items(&self, items: &[TextItem]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for item in items {
            match item {
                TextItem::Str(s) => {
                    bytes.extend(self.encoding.encode_string(s));
                }
                TextItem::Char(c) => {
                    bytes.push(self.encoding.encode_char(*c));
                }
            }
        }
        bytes
    }

    fn resolve_file_path(&self, path: &str) -> AsmResult<PathBuf> {
        let p = self.project_dir.join(path);
        if p.exists() {
            return Ok(p);
        }
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
        Err(AsmError::new(format!("Cannot find file: {}", path)))
    }

    fn record_line_address(&mut self, file: &str, line_num: usize, addr: u16) {
        self.debug_info.line_addresses
            .entry(file.to_string())
            .or_default()
            .entry(line_num)
            .or_default()
            .push(addr);
    }

    fn record_data_line(&mut self, file: &str, line_num: usize, addr: u16, byte_length: usize, unit_bytes: usize) {
        self.debug_info.data_lines
            .entry(file.to_string())
            .or_default()
            .insert(line_num, DataLineInfo {
                addr,
                byte_length,
                unit_bytes,
            });
    }

    /// Collect macro debug info after preprocessing
    pub fn collect_macro_debug_info(&mut self) {
        for (name, def) in self.symbols.all_macros() {
            self.debug_info.macros.insert(name.clone(), MacroDebugInfo {
                src: def.file.clone(),
                line: def.line,
                params: def.params.iter().map(|p| p.name.clone()).collect(),
            });
        }
    }
}

#[derive(Clone, Copy)]
enum BlockKind {
    If,
    Loop,
    Optional,
}

impl BlockKind {
    fn end_directive_name(self) -> &'static str {
        match self {
            BlockKind::If => ".endif",
            BlockKind::Loop => ".endloop",
            BlockKind::Optional => ".endoptional",
        }
    }
}

enum ControlDirective<'a> {
    If(&'a Expr),
    EndIf,
    Loop(&'a Expr),
    EndLoop,
    Optional,
    EndOptional,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::preprocessor::preprocess;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestProject {
        root: PathBuf,
    }

    impl TestProject {
        fn new(files: &[(&str, &[u8])]) -> Self {
            let unique = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!("v6asm-tests-{}-{}", nanos, unique));
            fs::create_dir_all(&root).unwrap();
            for (path, contents) in files {
                let full_path = root.join(path);
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent).unwrap();
                }
                fs::write(full_path, contents).unwrap();
            }
            Self { root }
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }
    }

    impl Drop for TestProject {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn assemble_project(project: &TestProject, main_file: &str) -> Assembler {
        let main_path = project.path(main_file);
        let mut assembler = Assembler::new(CpuMode::I8080, project.root.clone());
        let lines = preprocess(&main_path, &project.root, &mut assembler.symbols, &|path| {
            fs::read_to_string(path).map_err(|err| AsmError::new(err.to_string()))
        })
        .unwrap();
        assembler.assemble(&lines).unwrap();
        assembler
    }

    fn assemble_project_result(project: &TestProject, main_file: &str) -> AsmResult<Assembler> {
        let main_path = project.path(main_file);
        let mut assembler = Assembler::new(CpuMode::I8080, project.root.clone());
        let lines = preprocess(&main_path, &project.root, &mut assembler.symbols, &|path| {
            fs::read_to_string(path).map_err(|err| AsmError::new(err.to_string()))
        })?;
        assembler.assemble(&lines)?;
        Ok(assembler)
    }

    fn rom_bytes(assembler: &Assembler) -> Vec<u8> {
        assembler.output.extract_rom()
    }

    #[test]
    fn assembles_org_byte_word_dword_align_storage_and_text() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
.byte 1, 2
.word $1234
.dword $12345678
.align 16
.storage 2, $AA
.text "Hi", '!'"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(assembler.output.min_addr(), Some(0x0100));
        assert_eq!(rom_bytes(&assembler), vec![
            0x01, 0x02,
            0x34, 0x12,
            0x78, 0x56, 0x34, 0x12,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0xAA, 0xAA,
            b'H', b'i', b'!'
        ]);
    }

    #[test]
    fn assembles_include_filesize_and_incbin() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
SIZE .filesize "assets/blob.bin"
.include "inc.asm"
.byte SIZE
.incbin "assets/blob.bin", 1, 3"#,
            ),
            (
                "inc.asm",
                b".byte $11\n.word $2233\n",
            ),
            (
                "assets/blob.bin",
                &[0x10, 0x20, 0x30, 0x40, 0x50],
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(assembler.symbols.resolve("SIZE"), Some(5));
        assert_eq!(rom_bytes(&assembler), vec![0x11, 0x33, 0x22, 0x05, 0x20, 0x30, 0x40]);
    }

    #[test]
    fn assembles_macro_with_defaults_and_endmacro() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
.macro PUT(value=$2A)
    .byte value
.endmacro
PUT()
PUT($7F)"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(rom_bytes(&assembler), vec![0x2A, 0x7F]);
    }

    #[test]
    fn assembles_if_blocks_and_skips_false_branches() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
FLAG = 1
.if FLAG
    .byte $AA
.endif
.if 0
    .error "should not execute"
.endif
.if FLAG == 0
    .byte $BB
.endif
.byte $CC"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(rom_bytes(&assembler), vec![0xAA, 0xCC]);
    }

    #[test]
    fn assembles_loop_blocks_and_updates_variables_per_iteration() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
Counter .var 1
.loop 3
    .byte Counter
    Counter = Counter + 1
.endloop
.byte Counter"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(assembler.symbols.resolve("Counter"), Some(4));
        assert_eq!(rom_bytes(&assembler), vec![0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn assembles_optional_blocks_when_referenced() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
    jmp Used
.optional
Used:
    .byte $44
.endoptional"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(rom_bytes(&assembler), vec![0xC3, 0x03, 0x01, 0x44]);
    }

    #[test]
    fn prunes_optional_blocks_when_unreferenced() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
.optional
Unused:
    .byte $44
.endoptional
.byte $55"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(rom_bytes(&assembler), vec![0x55]);
    }

    #[test]
    fn setting_optional_false_disables_pruning() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
.setting optional, false
.optional
Unused:
    .byte $44
.endoptional
.byte $55"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert!(!assembler.settings.optional_enabled);
        assert_eq!(rom_bytes(&assembler), vec![0x44, 0x55]);
    }

    #[test]
    fn supports_db_dw_and_dd_aliases() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
DB 1, 2
DW $1234
DD $12345678"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(rom_bytes(&assembler), vec![0x01, 0x02, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12]);
    }

    #[test]
    fn applies_encoding_to_text_output() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
.encoding "ascii", "upper"
.text "ab", 'c'
.encoding "screencodecommodore"
.text "@A""#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(rom_bytes(&assembler), vec![b'A', b'B', b'C', 0x00, 0x01]);
    }

    #[test]
    fn executes_print_without_failing() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
VALUE = 3
.print "value", VALUE
.byte VALUE"#,
            ),
        ]);

        let main_path = project.path("main.asm");
        let mut assembler = Assembler::new(CpuMode::I8080, project.root.clone());
        assembler.quiet = false;
        let lines = preprocess(&main_path, &project.root, &mut assembler.symbols, &|path| {
            fs::read_to_string(path).map_err(|err| AsmError::new(err.to_string()))
        })
        .unwrap();
        assembler.assemble(&lines).unwrap();

        assert_eq!(rom_bytes(&assembler), vec![0x03]);
    }

    #[test]
    fn reports_error_directive_with_source_location() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
.error "boom", 7"#,
            ),
        ]);

        let error = assemble_project_result(&project, "main.asm").err().unwrap();

        assert_eq!(error.message, "boom 7");
        assert_eq!(error.location.unwrap().line, 2);
    }

    #[test]
    fn supports_var_and_reassignment() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
Counter .var 10
.byte Counter
Counter = Counter - 1
.byte Counter
Counter EQU 5
.byte Counter"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(assembler.symbols.resolve("Counter"), Some(5));
        assert_eq!(rom_bytes(&assembler), vec![10, 9, 5]);
    }

    #[test]
    fn supports_bare_storage_without_filler() {
        let project = TestProject::new(&[
            (
                "main.asm",
                br#".org $0100
.byte 1
.storage 2
.byte 2"#,
            ),
        ]);

        let assembler = assemble_project(&project, "main.asm");

        assert_eq!(rom_bytes(&assembler), vec![1, 0, 0, 2]);
    }
}
