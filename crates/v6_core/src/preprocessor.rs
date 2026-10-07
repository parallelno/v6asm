use std::collections::HashSet;
use std::path::{Path, PathBuf};
use crate::diagnostics::{AsmError, AsmResult};
use crate::symbols::{MacroBodyLine, MacroDef, MacroParam, SymbolTable};

const MAX_INCLUDE_DEPTH: usize = 16;
#[allow(dead_code)]
const MAX_MACRO_DEPTH: usize = 32;
#[allow(dead_code)]
const MAX_LOOP_ITERATIONS: usize = 100_000;

/// Original source file content for listing generation
#[derive(Debug, Clone)]
pub struct OriginalSource {
    pub file: String,
    pub lines: Vec<String>,
}

/// A preprocessed source line with metadata
#[derive(Debug, Clone)]
pub struct SourceLine {
    pub file: String,
    pub line_num: usize,
    /// Zero-based offset of this split statement within its physical line.
    pub column_offset: usize,
    pub text: String,
    pub macro_context: Option<String>,
    pub expansion: Vec<ExpansionSite>,
    /// Where this line's text actually originates. `Some((file, line))` for a
    /// macro-expanded line (the macro body line), `None` when it coincides with
    /// `file`/`line_num`. Diagnostics use this so errors raised while expanding
    /// a macro point at the macro definition rather than the invocation.
    pub origin: Option<(String, usize)>,
}

impl SourceLine {
    /// File to attribute diagnostics to: the macro definition file for an
    /// expanded line, otherwise the line's own file.
    pub fn diag_file(&self) -> &str {
        match &self.origin {
            Some((f, _)) => f.as_str(),
            None => self.file.as_str(),
        }
    }

    /// Line number to attribute diagnostics to: the macro definition line for
    /// an expanded line, otherwise the line's own line number.
    pub fn diag_line(&self) -> usize {
        match &self.origin {
            Some((_, l)) => *l,
            None => self.line_num,
        }
    }
}

/// The kind of source expansion contributing to a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpansionKind {
    Macro,
    Loop,
}

/// The origin of an expansion contributing to a source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpansionSite {
    pub kind: ExpansionKind,
    pub name: Option<String>,
    pub definition_file: String,
    pub definition_line: usize,
    pub invocation_file: String,
    pub invocation_line: usize,
    pub iteration: Option<usize>,
}

/// Read and preprocess source files
pub fn preprocess(
    main_file: &Path,
    project_dir: &Path,
    include_dirs: &[PathBuf],
    symbols: &mut SymbolTable,
    read_file: &dyn Fn(&Path) -> AsmResult<String>,
) -> AsmResult<Vec<SourceLine>> {
    let content = read_file(main_file)?;
    let file_name = path_relative_to(main_file, project_dir);

    // Step 1: Strip multi-line comments
    let content = strip_multiline_comments(&content);

    // Step 2: Load and inline includes, collect macros
    let raw_lines = content_to_lines(&content, &file_name);
    let mut seen: HashSet<PathBuf> = HashSet::new();
    // The main file itself is not subject to .pragma once deduplication —
    // only included files are. So we don't insert main_file into `seen` here.
    // If the main file declares .setting force_once, propagate to all its includes.
    let main_force_once = scan_force_once_setting(&content);
    let mut expanded = expand_includes(raw_lines, main_file, project_dir, include_dirs, read_file, 0, &mut seen, main_force_once)?;

    // Step 3: Collect macro definitions
    collect_macros(&mut expanded, symbols)?;

    // Step 4: Expand macros, loops, and conditionals
    // (This will be done during assembly passes since .if/.loop need expression evaluation)

    Ok(expanded)
}

fn path_relative_to(file: &Path, base: &Path) -> String {
    // A relative path is already relative to the current working directory, so
    // keep it intact. This yields a path that is clickable from where the
    // assembler was invoked and reflects the include search (e.g. the `-I`
    // directory) used to locate the file.
    let rel = if file.is_relative() {
        file.to_string_lossy().to_string()
    } else if let Ok(stripped) = file.strip_prefix(base) {
        // Absolute path under the project directory: make it relative.
        stripped.to_string_lossy().to_string()
    } else {
        // Absolute path elsewhere: fall back to the bare file name.
        file.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    };
    // Normalize separators so the path renders consistently (and clickably)
    // regardless of how each include path fragment was written.
    rel.replace('\\', "/")
}

fn content_to_lines(content: &str, file_name: &str) -> Vec<SourceLine> {
    let mut result = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line_num = i + 1;
        for (part, column_offset) in split_on_backslash_with_offsets(line) {
            result.push(SourceLine {
                file: file_name.to_string(),
                line_num,
                column_offset,
                text: part,
                macro_context: None,
                expansion: Vec::new(),
                origin: None,
            });
        }
    }
    result
}

/// Split a physical source line on `\` line separators, ignoring backslashes
/// inside string/char literals or after a line-comment marker (`;` or `//`).
#[allow(dead_code)]
fn split_on_backslash(line: &str) -> Vec<String> {
    split_on_backslash_with_offsets(line)
        .into_iter()
        .map(|(part, _)| part)
        .collect()
}

/// Split a physical source line and retain each statement's character offset.
fn split_on_backslash_with_offsets(line: &str) -> Vec<(String, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_string = false;
    let mut string_char = '"';

    while i < chars.len() {
        let c = chars[i];

        if in_string {
            if c == '\\' && i + 1 < chars.len() {
                // Skip escaped character inside the string/char literal.
                i += 2;
                continue;
            }
            if c == string_char {
                in_string = false;
            }
            i += 1;
            continue;
        }

        // Enter a string or char literal.
        if c == '"' || c == '\'' {
            in_string = true;
            string_char = c;
            i += 1;
            continue;
        }

        // Line comment — everything after this stays attached to the current part.
        if c == ';' {
            break;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            break;
        }

        if c == '\\' {
            parts.push((chars[start..i].iter().collect(), start));
            i += 1;
            start = i;
            continue;
        }

        i += 1;
    }

    parts.push((chars[start..].iter().collect(), start));
    parts
}

pub fn strip_multiline_comments(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char = '"';

    while i < chars.len() {
        // Track string literals to avoid stripping inside them
        if !in_string && (chars[i] == '"' || chars[i] == '\'') {
            string_char = chars[i];
            in_string = true;
            result.push(chars[i]);
            i += 1;
            continue;
        }
        if in_string {
            if chars[i] == '\\' && i + 1 < chars.len() {
                result.push(chars[i]);
                result.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if chars[i] == string_char {
                in_string = false;
            }
            result.push(chars[i]);
            i += 1;
            continue;
        }

        // Single-line comments (`;` or `//`): copy verbatim to end of line so that
        // apostrophes or `/*` inside comments don't affect parsing state.
        if chars[i] == ';' || (chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/') {
            while i < chars.len() && chars[i] != '\n' {
                result.push(chars[i]);
                i += 1;
            }
            continue;
        }

        // Check for /* ... */
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            // Preserve newlines within the comment so line numbers stay correct
            while i < chars.len() {
                if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    i += 2;
                    break;
                }
                if chars[i] == '\n' {
                    result.push('\n');
                }
                i += 1;
            }
            continue;
        }

        result.push(chars[i]);
        i += 1;
    }
    result
}

/// Return true if the first non-empty, non-comment line of `content` is
/// `.pragma once` (case-insensitive).
fn has_pragma_once(content: &str) -> bool {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        // Skip single-line comments
        if trimmed.starts_with(';') || trimmed.starts_with("//") { continue; }
        return trimmed.eq_ignore_ascii_case(".pragma once");
    }
    false
}

/// Return true if `content` contains `.setting force_once` with a non-false
/// value anywhere in the file.
fn scan_force_once_setting(content: &str) -> bool {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        if trimmed.starts_with(';') || trimmed.starts_with("//") { continue; }
        if trimmed.len() < 8 || !trimmed[..8].eq_ignore_ascii_case(".setting") { continue; }
        // Verify word boundary — avoid ".settings" matching ".setting"
        if let Some(&b) = trimmed.as_bytes().get(8) {
            if b.is_ascii_alphanumeric() || b == b'_' { continue; }
        }
        // Strip inline comment, then skip the ".setting" keyword
        let stripped = strip_single_line_comment(trimmed);
        let rest = stripped[8..].trim().to_string();
        // Walk comma-separated key[, value] pairs
        let mut parts = rest.split(',').map(|s| s.trim().to_string());
        loop {
            let key = match parts.next() {
                Some(k) if !k.is_empty() => k,
                _ => break,
            };
            if key.eq_ignore_ascii_case("force_once") {
                let val = parts.next().unwrap_or_else(|| "true".to_string());
                if !val.eq_ignore_ascii_case("false") {
                    return true;
                }
            }
        }
    }
    false
}

fn expand_includes(
    lines: Vec<SourceLine>,
    current_file: &Path,
    project_dir: &Path,
    include_dirs: &[PathBuf],
    read_file: &dyn Fn(&Path) -> AsmResult<String>,
    depth: usize,
    seen: &mut HashSet<PathBuf>,
    force_once_scope: bool,
) -> AsmResult<Vec<SourceLine>> {
    if depth >= MAX_INCLUDE_DEPTH {
        return Err(AsmError::new(format!("Include depth exceeded {} levels", MAX_INCLUDE_DEPTH)));
    }

    let mut result = Vec::new();
    let current_dir = current_file.parent().unwrap_or(project_dir);

    for line in &lines {
        let trimmed = line.text.trim();

        // Skip the `.pragma once` directive itself — it's a preprocessor hint,
        // not assembly output.
        if trimmed.eq_ignore_ascii_case(".pragma once") {
            continue;
        }

        // Check for .include "file"
        if let Some(path_str) = parse_include_directive(trimmed) {
            let include_path = resolve_include_path(&path_str, current_dir, project_dir, include_dirs)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;

            // Canonicalize so that different relative spellings of the same
            // file compare equal.
            let canonical = include_path.canonicalize().unwrap_or_else(|_| include_path.clone());

            let content = read_file(&include_path)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;

            // Determine whether this file should be included at most once.
            // True wins: force_once inherited from the parent scope, the
            // file's own .pragma once, or a .setting force_once, true
            // anywhere inside the file.
            let file_has_force_once = scan_force_once_setting(&content);
            let should_deduplicate = force_once_scope
                || has_pragma_once(&content)
                || file_has_force_once;

            if should_deduplicate {
                if seen.contains(&canonical) {
                    continue;
                }
                seen.insert(canonical);
            }

            // Propagate force_once to the included file's own sub-includes.
            let child_force_once_scope = force_once_scope || file_has_force_once;

            let content = strip_multiline_comments(&content);
            let file_name = path_relative_to(&include_path, project_dir);
            let inc_lines = content_to_lines(&content, &file_name);
            let expanded = expand_includes(inc_lines, &include_path, project_dir, include_dirs, read_file, depth + 1, seen, child_force_once_scope)?;
            result.extend(expanded);
        } else {
            result.push(line.clone());
        }
    }

    Ok(result)
}

pub fn parse_include_directive(line: &str) -> Option<String> {
    // Strip single-line comments first
    let line = strip_single_line_comment(line);
    let trimmed = line.trim();

    if !trimmed.starts_with(".include") && !trimmed.starts_with(".INCLUDE") {
        return None;
    }

    let rest = trimmed[8..].trim();
    if rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2 {
        Some(rest[1..rest.len() - 1].to_string())
    } else if rest.starts_with('\'') && rest.ends_with('\'') && rest.len() >= 2 {
        Some(rest[1..rest.len() - 1].to_string())
    } else {
        None
    }
}

fn strip_single_line_comment(line: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut in_string = false;
    let mut string_char = '"';

    while i < chars.len() {
        if !in_string {
            if chars[i] == ';' {
                break;
            }
            if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
                break;
            }
            if chars[i] == '"' || chars[i] == '\'' {
                in_string = true;
                string_char = chars[i];
            }
        } else if chars[i] == '\\' && i + 1 < chars.len() {
            result.push(chars[i]);
            result.push(chars[i + 1]);
            i += 2;
            continue;
        } else if chars[i] == string_char {
            in_string = false;
        }
        result.push(chars[i]);
        i += 1;
    }
    result
}

fn resolve_include_path(path_str: &str, current_dir: &Path, project_dir: &Path, include_dirs: &[PathBuf]) -> AsmResult<PathBuf> {
    // Try relative to current file
    let p = current_dir.join(path_str);
    if p.exists() {
        return Ok(p);
    }
    // Try relative to project dir
    let p = project_dir.join(path_str);
    if p.exists() {
        return Ok(p);
    }
    // Try each include directory in order
    for dir in include_dirs {
        let p = dir.join(path_str);
        if p.exists() {
            return Ok(p);
        }
    }
    // Try CWD
    let p = PathBuf::from(path_str);
    if p.exists() {
        return Ok(p);
    }
    Err(AsmError::new(format!("Cannot find include file: {}", path_str)))
}

fn collect_macros(lines: &mut Vec<SourceLine>, symbols: &mut SymbolTable) -> AsmResult<()> {
    let mut i = 0;
    let mut new_lines = Vec::new();
    let mut in_macro = false;
    let mut macro_name = String::new();
    let mut macro_params: Vec<MacroParam> = Vec::new();
    let mut macro_body: Vec<MacroBodyLine> = Vec::new();
    let mut macro_file = String::new();
    let mut macro_line = 0;

    while i < lines.len() {
        let trimmed = lines[i].text.trim().to_string();

        if in_macro {
            if trimmed.eq_ignore_ascii_case(".endmacro") {
                // Save the macro
                let def = MacroDef {
                    name: macro_name.clone(),
                    params: macro_params.clone(),
                    body: macro_body.clone(),
                    file: macro_file.clone(),
                    line: macro_line,
                };
                symbols.define_macro(def)?;
                in_macro = false;
                macro_body.clear();
            } else {
                macro_body.push(MacroBodyLine {
                    text: lines[i].text.clone(),
                    file: lines[i].file.clone(),
                    line: lines[i].line_num,
                });
            }
            i += 1;
            continue;
        }

        if let Some((name, params)) = parse_macro_def_line(&trimmed) {
            // Check for duplicate parameter names
            let mut seen = std::collections::HashSet::new();
            for p in &params {
                let key = p.name.to_uppercase();
                if !seen.insert(key) {
                    return Err(AsmError::new(format!(
                        "Duplicate parameter '{}' in macro '{}'", p.name, name
                    )).ensure_location(&lines[i].file, lines[i].line_num));
                }
            }
            in_macro = true;
            macro_name = name;
            macro_params = params;
            macro_file = lines[i].file.clone();
            macro_line = lines[i].line_num;
            i += 1;
            continue;
        }

        new_lines.push(lines[i].clone());
        i += 1;
    }

    *lines = new_lines;
    Ok(())
}

fn parse_macro_def_line(line: &str) -> Option<(String, Vec<MacroParam>)> {
    let trimmed = line.trim();
    if !trimmed.starts_with(".macro") && !trimmed.starts_with(".MACRO") {
        return None;
    }
    let rest = trimmed[6..].trim();
    // Parse macro name
    let mut chars = rest.chars().peekable();
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '_' {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if name.is_empty() {
        return None;
    }

    // Parse params (optional, may be in parens)
    let rest: String = chars.collect();
    let rest = rest.trim();
    let params = parse_macro_params(rest);

    Some((name, params))
}

pub fn parse_macro_params(s: &str) -> Vec<MacroParam> {
    let mut params = Vec::new();
    let s = s.trim();
    if s.is_empty() {
        return params;
    }

    let s = if s.starts_with('(') && s.ends_with(')') {
        &s[1..s.len() - 1]
    } else if s.starts_with('(') {
        &s[1..]
    } else {
        s
    };

    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(eq_pos) = part.find('=') {
            let name = part[..eq_pos].trim().to_string();
            let default = part[eq_pos + 1..].trim().to_string();
            params.push(MacroParam {
                name,
                default: Some(default),
            });
        } else {
            params.push(MacroParam {
                name: part.to_string(),
                default: None,
            });
        }
    }
    params
}

/// Expand a macro invocation into source lines
pub fn expand_macro(
    macro_def: &MacroDef,
    args: &[String],
    call_index: usize,
    call_source: &SourceLine,
) -> AsmResult<Vec<SourceLine>> {
    let mut body_text = Vec::new();

    for body_line in &macro_def.body {
        let mut expanded = body_line.text.clone();
        // Substitute parameters
        for (i, param) in macro_def.params.iter().enumerate() {
            let value = if i < args.len() && !args[i].is_empty() {
                &args[i]
            } else if let Some(ref default) = param.default {
                default
            } else {
                return Err(AsmError::new(format!(
                    "Missing argument '{}' for macro '{}'", param.name, macro_def.name
                )));
            };
            // Replace parameter name with value (whole word only)
            expanded = replace_param(&expanded, &param.name, value);
        }
        let mut expansion = call_source.expansion.clone();
        expansion.push(ExpansionSite {
            kind: ExpansionKind::Macro,
            name: Some(macro_def.name.clone()),
            definition_file: macro_def.file.clone(),
            definition_line: macro_def.line,
            // Use the origin location: for a call inside another macro body,
            // `file`/`line_num` hold the outer invocation, whereas the origin is
            // the actual line where this macro was invoked.
            invocation_file: call_source.diag_file().to_string(),
            invocation_line: call_source.diag_line(),
            iteration: None,
        });
        body_text.push(SourceLine {
            file: call_source.file.clone(),
            line_num: call_source.line_num,
            column_offset: call_source.column_offset,
            text: expanded,
            macro_context: Some(format!("{}_{}", macro_def.name, call_index)),
            expansion,
            origin: Some((body_line.file.clone(), body_line.line)),
        });
    }

    Ok(body_text)
}

pub fn replace_param(text: &str, param: &str, value: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = text.chars().collect();
    let param_chars: Vec<char> = param.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Check for string literals - don't replace inside them
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
            result.push(chars[i]);
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    result.push(chars[i]);
                    i += 1;
                }
                result.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                result.push(chars[i]);
                i += 1;
            }
            continue;
        }

        // Check for whole-word match of param name
        if i + param_chars.len() <= chars.len() {
            let slice: String = chars[i..i + param_chars.len()].iter().collect();
            if slice == param {
                // Check word boundaries
                let before_ok = i == 0 || !is_ident_char(chars[i - 1]);
                let after_ok = i + param_chars.len() >= chars.len()
                    || !is_ident_char(chars[i + param_chars.len()]);
                if before_ok && after_ok {
                    // Wrap multi-token values in parentheses to preserve operator precedence,
                    // but only when the space is outside a string literal.
                    // e.g. `a + b` → `(a + b)` but `"A B"` stays `"A B"`.
                    if has_space_outside_string(value) {
                        result.push('(');
                        result.push_str(value);
                        result.push(')');
                    } else {
                        result.push_str(value);
                    }
                    i += param_chars.len();
                    continue;
                }
            }
        }

        result.push(chars[i]);
        i += 1;
    }
    result
}

/// Returns true if `s` contains a space character that is not inside a string
/// or char literal. Used to decide whether a macro argument value needs to be
/// wrapped in parentheses when substituted into an expression.
fn has_space_outside_string(s: &str) -> bool {
    let mut in_string = false;
    let mut string_char = '"';
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            if c == '\\' {
                // Skip escaped character
                chars.next();
                continue;
            }
            if c == string_char {
                in_string = false;
            }
        } else if c == '"' || c == '\'' {
            in_string = true;
            string_char = c;
        } else if c == ' ' {
            return true;
        }
    }
    false
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Result of parsing a potential macro invocation.
pub enum MacroInvocationResult {
    /// Line does not look like a macro call at all, or the syntax is malformed.
    NotAMacroCall,
    /// Line looks like a macro call (Name(args...)) but the macro is not defined.
    UndefinedMacro(String),
    /// A known macro invocation with parsed arguments.
    KnownMacro(String, Vec<String>),
}

/// Parse a macro invocation from a line of text.
/// Returns a `MacroInvocationResult` indicating whether the line is a known
/// macro call, an undefined-macro call, or not a macro call at all.
pub fn parse_macro_invocation(line: &str, symbols: &SymbolTable) -> MacroInvocationResult {
    // Strip any inline comment so parentheses inside comments don't get mistaken
    // for the macro argument list closing paren.
    let line = strip_single_line_comment(line);
    let trimmed = line.trim();
    // Skip labels at the start
    let text = skip_label(trimmed);
    let text = text.trim();

    // Find the macro name (first identifier-like token)
    let mut chars = text.chars().peekable();
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '_' {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }

    if name.is_empty() {
        return MacroInvocationResult::NotAMacroCall;
    }

    // Check if this is a known macro
    if symbols.get_macro(&name).is_none() {
        // Check if this looks like a macro call: name immediately followed by '('
        // (no whitespace), which is distinct from instructions like 'out (expr)'.
        let rest: String = chars.collect();
        if rest.starts_with('(') {
            return MacroInvocationResult::UndefinedMacro(name);
        }
        return MacroInvocationResult::NotAMacroCall;
    }

    // Parse arguments
    let rest: String = chars.collect();
    let rest = rest.trim();
    let args = if rest.starts_with('(') {
        if let Some(end) = rest.rfind(')') {
            parse_macro_args(&rest[1..end])
        } else {
            Vec::new()
        }
    } else if !rest.is_empty() {
        // Arguments without parens (space-separated isn't standard, but handle comma-separated)
        parse_macro_args(rest)
    } else {
        Vec::new()
    };

    MacroInvocationResult::KnownMacro(name, args)
}

fn skip_label(line: &str) -> &str {
    // Skip a leading label like "name:" or "@name:"
    let bytes = line.as_bytes();
    let mut i = 0;
    if i < bytes.len() && bytes[i] == b'@' {
        i += 1;
    }
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b':' {
        return &line[i + 1..];
    }
    line
}

pub fn parse_macro_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut in_string = false;
    let mut string_char = '"';

    for ch in s.chars() {
        if in_string {
            current.push(ch);
            if ch == string_char {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                in_string = true;
                string_char = ch;
                current.push(ch);
            }
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() || !args.is_empty() {
        args.push(current.trim().to_string());
    }
    args
}

/// Collect original source files in listing order (main file, then includes as they appear).
/// Each file is stored with its full text lines. The order matches how they appear in the source.
pub fn collect_original_sources(
    main_file: &Path,
    project_dir: &Path,
    include_dirs: &[PathBuf],
    read_file: &dyn Fn(&Path) -> AsmResult<String>,
) -> AsmResult<Vec<OriginalSource>> {
    let mut sources = Vec::new();
    collect_sources_recursive(main_file, project_dir, include_dirs, read_file, &mut sources, 0)?;
    Ok(sources)
}

fn collect_sources_recursive(
    file: &Path,
    project_dir: &Path,
    include_dirs: &[PathBuf],
    read_file: &dyn Fn(&Path) -> AsmResult<String>,
    sources: &mut Vec<OriginalSource>,
    depth: usize,
) -> AsmResult<()> {
    if depth >= MAX_INCLUDE_DEPTH {
        return Ok(());
    }

    let content = read_file(file)?;
    let content = strip_multiline_comments(&content);
    let file_name = path_relative_to(file, project_dir);
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let current_dir = file.parent().unwrap_or(project_dir);

    // Store this file
    sources.push(OriginalSource {
        file: file_name,
        lines: lines.clone(),
    });

    // Recurse into includes
    for line in &lines {
        if let Some(path_str) = parse_include_directive(line.trim()) {
            if let Ok(include_path) = resolve_include_path(&path_str, current_dir, project_dir, include_dirs) {
                collect_sources_recursive(&include_path, project_dir, include_dirs, read_file, sources, depth + 1)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_basic() {
        assert_eq!(split_on_backslash("a"), vec!["a".to_string()]);
        assert_eq!(
            split_on_backslash("mvi a,1 \\ mvi b,2"),
            vec!["mvi a,1 ".to_string(), " mvi b,2".to_string()]
        );
        assert_eq!(
            split_on_backslash("a \\ b \\ c"),
            vec!["a ".to_string(), " b ".to_string(), " c".to_string()]
        );
    }

    #[test]
    fn split_ignores_backslash_in_strings() {
        assert_eq!(
            split_on_backslash(".text \"hello\\nworld\""),
            vec![".text \"hello\\nworld\"".to_string()]
        );
        assert_eq!(
            split_on_backslash("mvi a, '\\n' \\ ret"),
            vec!["mvi a, '\\n' ".to_string(), " ret".to_string()]
        );
    }

    #[test]
    fn split_ignores_backslash_after_line_comment() {
        assert_eq!(
            split_on_backslash("nop ; foo \\ bar"),
            vec!["nop ; foo \\ bar".to_string()]
        );
        assert_eq!(
            split_on_backslash("nop // foo \\ bar"),
            vec!["nop // foo \\ bar".to_string()]
        );
    }

    #[test]
    fn split_trailing_and_leading() {
        // Trailing backslash yields an empty trailing fragment.
        assert_eq!(
            split_on_backslash("nop \\"),
            vec!["nop ".to_string(), "".to_string()]
        );
        // Leading backslash yields an empty leading fragment.
        assert_eq!(
            split_on_backslash("\\ nop"),
            vec!["".to_string(), " nop".to_string()]
        );
    }

    #[test]
    fn content_to_lines_preserves_line_num_across_split() {
        let out = content_to_lines("nop\nmvi a,1 \\ mvi b,2\nret", "test.asm");
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].line_num, 1);
        assert_eq!(out[0].text, "nop");
        assert_eq!(out[1].line_num, 2);
        assert_eq!(out[1].text, "mvi a,1 ");
        assert_eq!(out[2].line_num, 2);
        assert_eq!(out[2].text, " mvi b,2");
        assert_eq!(out[3].line_num, 3);
        assert_eq!(out[3].text, "ret");
    }
}
