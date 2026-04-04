use std::path::{Path, PathBuf};
use crate::diagnostics::{AsmError, AsmResult};
use crate::symbols::{MacroDef, MacroParam, SymbolTable};

const MAX_INCLUDE_DEPTH: usize = 16;
#[allow(dead_code)]
const MAX_MACRO_DEPTH: usize = 32;
#[allow(dead_code)]
const MAX_LOOP_ITERATIONS: usize = 100_000;

/// A preprocessed source line with metadata
#[derive(Debug, Clone)]
pub struct SourceLine {
    pub file: String,
    pub line_num: usize,
    pub text: String,
    pub macro_context: Option<String>,
}

/// Read and preprocess source files
pub fn preprocess(
    main_file: &Path,
    project_dir: &Path,
    symbols: &mut SymbolTable,
    read_file: &dyn Fn(&Path) -> AsmResult<String>,
) -> AsmResult<Vec<SourceLine>> {
    let content = read_file(main_file)?;
    let file_name = path_relative_to(main_file, project_dir);

    // Step 1: Strip multi-line comments
    let content = strip_multiline_comments(&content);

    // Step 2: Load and inline includes, collect macros
    let raw_lines = content_to_lines(&content, &file_name);
    let mut expanded = expand_includes(raw_lines, main_file, project_dir, read_file, 0)?;

    // Step 3: Collect macro definitions
    collect_macros(&mut expanded, symbols)?;

    // Step 4: Expand macros, loops, and conditionals
    // (This will be done during assembly passes since .if/.loop need expression evaluation)

    Ok(expanded)
}

fn path_relative_to(file: &Path, base: &Path) -> String {
    if let Ok(rel) = file.strip_prefix(base) {
        rel.to_string_lossy().to_string()
    } else {
        file.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

fn content_to_lines(content: &str, file_name: &str) -> Vec<SourceLine> {
    content
        .lines()
        .enumerate()
        .map(|(i, line)| SourceLine {
            file: file_name.to_string(),
            line_num: i + 1,
            text: line.to_string(),
            macro_context: None,
        })
        .collect()
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

fn expand_includes(
    lines: Vec<SourceLine>,
    current_file: &Path,
    project_dir: &Path,
    read_file: &dyn Fn(&Path) -> AsmResult<String>,
    depth: usize,
) -> AsmResult<Vec<SourceLine>> {
    if depth >= MAX_INCLUDE_DEPTH {
        return Err(AsmError::new(format!("Include depth exceeded {} levels", MAX_INCLUDE_DEPTH)));
    }

    let mut result = Vec::new();
    let current_dir = current_file.parent().unwrap_or(project_dir);

    for line in &lines {
        let trimmed = line.text.trim();

        // Check for .include "file"
        if let Some(path_str) = parse_include_directive(trimmed) {
            let include_path = resolve_include_path(&path_str, current_dir, project_dir)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
            let content = read_file(&include_path)
                .map_err(|e| e.ensure_location(&line.file, line.line_num))?;
            let content = strip_multiline_comments(&content);
            let file_name = path_relative_to(&include_path, project_dir);
            let inc_lines = content_to_lines(&content, &file_name);
            let expanded = expand_includes(inc_lines, &include_path, project_dir, read_file, depth + 1)?;
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

fn resolve_include_path(path_str: &str, current_dir: &Path, project_dir: &Path) -> AsmResult<PathBuf> {
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
    let mut macro_body: Vec<String> = Vec::new();
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
                macro_body.push(lines[i].text.clone());
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
    _call_file: &str,
    call_line: usize,
) -> AsmResult<Vec<SourceLine>> {
    let mut body_text = Vec::new();

    for body_line in &macro_def.body {
        let mut expanded = body_line.clone();
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
        body_text.push(SourceLine {
            file: macro_def.file.clone(),
            line_num: call_line,
            text: expanded,
            macro_context: Some(format!("{}_{}", macro_def.name, call_index)),
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
                    result.push_str(value);
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

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Parse a macro invocation from a line of text. Returns (macro_name, arguments) if found.
pub fn parse_macro_invocation(line: &str, symbols: &SymbolTable) -> Option<(String, Vec<String>)> {
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
        return None;
    }

    // Check if this is a known macro
    if symbols.get_macro(&name).is_none() {
        return None;
    }

    // Parse arguments
    let rest: String = chars.collect();
    let rest = rest.trim();
    let args = if rest.starts_with('(') {
        let end = rest.rfind(')')?;
        parse_macro_args(&rest[1..end])
    } else if !rest.is_empty() {
        // Arguments without parens (space-separated isn't standard, but handle comma-separated)
        parse_macro_args(rest)
    } else {
        Vec::new()
    };

    Some((name, args))
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
