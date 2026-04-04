use std::path::{Path, PathBuf};

use crate::diagnostics::{AsmError, AsmResult, SourceLocation};
use crate::encoding::{Encoding, EncodingCase, EncodingType};
use crate::expr::{eval_expr, Expr};
use crate::instructions::{encode_instruction, ParsedOperand};
use crate::lexer::tokenize_line;
use crate::parser::{self, Directive, ParsedLine, PrintArg, TextItem};
use crate::preprocessor::{SourceLine, OriginalSource, expand_macro, parse_macro_invocation};
use crate::project::CpuMode;
use crate::symbols::SymbolTable;

const MAX_LOOP_ITERATIONS: usize = 100_000;

/// Output buffer for assembled code (sparse 64KB address space)
pub struct OutputBuffer {
    data: Vec<Option<u8>>,
    min_addr: Option<u16>,
    max_addr: Option<u16>,
    write_count: usize,
}

impl OutputBuffer {
    pub fn new() -> Self {
        Self {
            data: vec![None; 65536],
            min_addr: None,
            max_addr: None,
            write_count: 0,
        }
    }

    pub fn write_byte(&mut self, addr: u16, byte: u8) {
        self.data[addr as usize] = Some(byte);
        self.min_addr = Some(self.min_addr.map_or(addr, |m: u16| m.min(addr)));
        self.max_addr = Some(self.max_addr.map_or(addr, |m: u16| m.max(addr)));
        self.write_count += 1;
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

    pub fn read_byte(&self, addr: u16) -> Option<u8> {
        self.data[addr as usize]
    }

    pub fn write_count(&self) -> usize {
        self.write_count
    }
}

/// A single entry for the listing file
#[derive(Debug, Clone)]
pub struct ListingLine {
    pub file: String,
    pub line_num: usize,
    pub text: String,
    pub addr: u16,
    pub byte_count: usize,
    /// If this line is from a macro expansion
    pub macro_expansion: bool,
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
    pub listing_data: Vec<ListingLine>,
    pub original_sources: Vec<OriginalSource>,
    pub pc: u16,
    pub cpu_mode: CpuMode,
    pub encoding: Encoding,
    pub settings: AssemblerSettings,
    pub errors: Vec<AsmError>,
    pub quiet: bool,
    project_dir: PathBuf,

    // Tracking for .optional blocks
    _optional_stack: Vec<OptionalBlock>,
    _optional_blocks: Vec<OptionalBlockInfo>,

    // Loop/if expansion depth tracking
    macro_depth: usize,
}

struct OptionalBlock {
    _start_idx: usize,
    _symbols_defined: Vec<String>,
}

struct OptionalBlockInfo {
    _start_line_idx: usize,
    _end_line_idx: usize,
    _symbols_defined: Vec<String>,
}

impl Assembler {
    pub fn new(cpu_mode: CpuMode, project_dir: PathBuf) -> Self {
        Self {
            symbols: SymbolTable::new(),
            output: OutputBuffer::new(),
            listing_data: Vec::new(),
            original_sources: Vec::new(),
            pc: 0,
            cpu_mode,
            encoding: Encoding::default(),
            settings: AssemblerSettings::default(),
            errors: Vec::new(),
            quiet: false,
            project_dir,
            _optional_stack: Vec::new(),
            _optional_blocks: Vec::new(),
            macro_depth: 0,
        }
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
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
                self.expand_macro_pass1(line, &macro_name, &args)
                    .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
                i += 1;
                continue;
            }

            let tokens = tokenize_line(&line.text, &line.file, line.line_num)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
            if tokens.is_empty() {
                i += 1;
                continue;
            }

            let parsed = parser::parse_line(&tokens, self.cpu_mode)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
            if parsed.len() == 1 {
                if let Some(control) = Self::control_directive(&parsed[0]) {
                    match control {
                        ControlDirective::If(expr) => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::If)?;
                            if self.eval_expr(expr)
                                .map_err(|e| e.ensure_location(&line.file, line.line_num))? != 0 {
                                self.process_lines_pass1(&lines[i + 1..end])?;
                            }
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::Loop(expr) => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::Loop)?;
                            let count = self.eval_expr(expr)
                                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
                            if count < 0 {
                                return Err(AsmError::new("Loop count must be non-negative")
                                    .ensure_location(&line.file, line.line_num));
                            }
                            if count as usize > MAX_LOOP_ITERATIONS {
                                return Err(AsmError::new(format!(
                                    "Loop iteration count exceeded {}",
                                    MAX_LOOP_ITERATIONS
                                )).ensure_location(&line.file, line.line_num));
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

            self.process_parsed_line_pass1(line, &parsed)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
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
            Directive::Storage { length, filler: _ } => {
                let len = self.eval_expr(length)? as u16;
                self.pc = self.pc.wrapping_add(len);
            }
            Directive::Byte(exprs) => {
                self.pc = self.pc.wrapping_add(exprs.len() as u16);
            }
            Directive::Word(exprs) => {
                self.pc = self.pc.wrapping_add((exprs.len() * 2) as u16);
            }
            Directive::Dword(exprs) => {
                self.pc = self.pc.wrapping_add((exprs.len() * 4) as u16);
            }
            Directive::Text(items) => {
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
            }
            Directive::Optional | Directive::EndOptional => {
            }
            Directive::IncBin { path, offset, length } => {
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

            let pc_before = self.pc;
            let wc_before = self.output.write_count();

            if let Some((macro_name, args)) = parse_macro_invocation(&line.text, &self.symbols) {
                let macro_start_pc = self.pc;
                self.expand_macro_pass2(line, &macro_name, &args)
                    .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
                self.listing_data.push(ListingLine {
                    file: line.file.clone(),
                    line_num: line.line_num,
                    text: line.text.clone(),
                    addr: macro_start_pc,
                    byte_count: 0,
                    macro_expansion: line.macro_context.is_some(),
                });
                i += 1;
                continue;
            }

            let tokens = tokenize_line(&line.text, &line.file, line.line_num)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
            if tokens.is_empty() {
                self.listing_data.push(ListingLine {
                    file: line.file.clone(),
                    line_num: line.line_num,
                    text: line.text.clone(),
                    addr: pc_before,
                    byte_count: 0,
                    macro_expansion: line.macro_context.is_some(),
                });
                i += 1;
                continue;
            }

            let parsed = parser::parse_line(&tokens, self.cpu_mode)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
            if parsed.len() == 1 {
                if let Some(control) = Self::control_directive(&parsed[0]) {
                    match control {
                        ControlDirective::If(expr) => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::If)?;
                            self.listing_data.push(ListingLine {
                                file: line.file.clone(),
                                line_num: line.line_num,
                                text: line.text.clone(),
                                addr: self.pc,
                                byte_count: 0,
                                macro_expansion: line.macro_context.is_some(),
                            });
                            if self.eval_expr(expr)
                                .map_err(|e| e.ensure_location(&line.file, line.line_num))? != 0 {
                                self.process_lines_pass2(&lines[i + 1..end])?;
                            }
                            // Record the closing .endif
                            let end_line = &lines[end];
                            self.listing_data.push(ListingLine {
                                file: end_line.file.clone(),
                                line_num: end_line.line_num,
                                text: end_line.text.clone(),
                                addr: self.pc,
                                byte_count: 0,
                                macro_expansion: end_line.macro_context.is_some(),
                            });
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::Loop(expr) => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::Loop)?;
                            let count = self.eval_expr(expr)
                                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
                            if count < 0 {
                                return Err(AsmError::new("Loop count must be non-negative")
                                    .ensure_location(&line.file, line.line_num));
                            }
                            if count as usize > MAX_LOOP_ITERATIONS {
                                return Err(AsmError::new(format!(
                                    "Loop iteration count exceeded {}",
                                    MAX_LOOP_ITERATIONS
                                )).ensure_location(&line.file, line.line_num));
                            }
                            self.listing_data.push(ListingLine {
                                file: line.file.clone(),
                                line_num: line.line_num,
                                text: line.text.clone(),
                                addr: self.pc,
                                byte_count: 0,
                                macro_expansion: line.macro_context.is_some(),
                            });
                            for _ in 0..count as usize {
                                self.process_lines_pass2(&lines[i + 1..end])?;
                            }
                            // Record the closing .endl/.endloop
                            let end_line = &lines[end];
                            self.listing_data.push(ListingLine {
                                file: end_line.file.clone(),
                                line_num: end_line.line_num,
                                text: end_line.text.clone(),
                                addr: self.pc,
                                byte_count: 0,
                                macro_expansion: end_line.macro_context.is_some(),
                            });
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::Optional => {
                            let end = self.find_matching_block_end(lines, i, BlockKind::Optional)?;
                            self.listing_data.push(ListingLine {
                                file: line.file.clone(),
                                line_num: line.line_num,
                                text: line.text.clone(),
                                addr: self.pc,
                                byte_count: 0,
                                macro_expansion: line.macro_context.is_some(),
                            });
                            if !self.settings.optional_enabled
                                || self.should_include_optional_block(lines, i + 1, end)?
                            {
                                self.process_lines_pass2(&lines[i + 1..end])?;
                            }
                            // Record the closing .endoptional
                            let end_line = &lines[end];
                            self.listing_data.push(ListingLine {
                                file: end_line.file.clone(),
                                line_num: end_line.line_num,
                                text: end_line.text.clone(),
                                addr: self.pc,
                                byte_count: 0,
                                macro_expansion: end_line.macro_context.is_some(),
                            });
                            i = end + 1;
                            continue;
                        }
                        ControlDirective::EndIf
                        | ControlDirective::EndLoop
                        | ControlDirective::EndOptional => {
                            self.listing_data.push(ListingLine {
                                file: line.file.clone(),
                                line_num: line.line_num,
                                text: line.text.clone(),
                                addr: self.pc,
                                byte_count: 0,
                                macro_expansion: line.macro_context.is_some(),
                            });
                            i += 1;
                            continue;
                        }
                    }
                }
            }

            self.process_parsed_line_pass2(line, &parsed)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
            let byte_count = self.output.write_count() - wc_before;
            self.listing_data.push(ListingLine {
                file: line.file.clone(),
                line_num: line.line_num,
                text: line.text.clone(),
                addr: pc_before,
                byte_count,
                macro_expansion: line.macro_context.is_some(),
            });
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
                }
                ParsedLine::LocalLabel(name) => {
                    self.symbols.define_local_label(name, self.pc, &line.file, line.line_num)?;
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
                    }
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
        Err(AsmError::new(format!("Missing {}", kind.end_directive_name()))
            .ensure_location(&lines[start].file, lines[start].line_num))
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
                if let Some(f) = fill {
                    for _ in 0..len {
                        self.output.write_byte(self.pc, f);
                        self.pc = self.pc.wrapping_add(1);
                    }
                } else {
                    self.pc = self.pc.wrapping_add(len);
                }
            }
            Directive::Byte(exprs) => {
                for expr in exprs {
                    let val = self.eval_expr(expr)? as u8;
                    self.output.write_byte(self.pc, val);
                    self.pc = self.pc.wrapping_add(1);
                }
            }
            Directive::Word(exprs) => {
                for expr in exprs {
                    let val = self.eval_expr(expr)? as u16;
                    self.output.write_byte(self.pc, (val & 0xFF) as u8);
                    self.pc = self.pc.wrapping_add(1);
                    self.output.write_byte(self.pc, ((val >> 8) & 0xFF) as u8);
                    self.pc = self.pc.wrapping_add(1);
                }
            }
            Directive::Dword(exprs) => {
                for expr in exprs {
                    let val = self.eval_expr(expr)? as u32;
                    for i in 0..4 {
                        self.output.write_byte(self.pc, ((val >> (i * 8)) & 0xFF) as u8);
                        self.pc = self.pc.wrapping_add(1);
                    }
                }
            }
            Directive::Text(items) => {
                let bytes = self.encode_text_items(items);
                let _byte_count = bytes.len();
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
            }
            Directive::If(_) | Directive::EndIf | Directive::Loop(_) | Directive::EndLoop => {}
            Directive::Optional | Directive::EndOptional => {}
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
