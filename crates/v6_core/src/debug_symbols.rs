use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::assembler::DebugInfo;
use crate::symbols::SymbolTable;

// ── Serializable output types ───────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugSymbols {
    pub symbols: HashMap<String, SymbolEntry>,
    pub line_addresses: HashMap<String, HashMap<usize, Vec<u16>>>,
    pub data_lines: HashMap<String, HashMap<usize, DataLineEntry>>,
}

#[derive(Debug, Serialize)]
pub struct SymbolEntry {
    pub value: i64,
    pub path: String,
    pub line: usize,
    #[serde(rename = "type")]
    pub sym_type: SymbolType,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SymbolType {
    Label,
    Const,
    Func,
    Macro,
    #[serde(rename = "macroparam")]
    MacroParam,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataLineEntry {
    pub addr: u16,
    pub byte_length: usize,
    pub unit_bytes: usize,
}

// ── Builder ─────────────────────────────────────────────────────────────────

pub fn build_debug_symbols(info: &DebugInfo, symbols: &SymbolTable, project_dir: &Path) -> DebugSymbols {
    let mut syms = HashMap::new();

    // Labels
    for (name, label) in &info.labels {
        syms.insert(name.clone(), SymbolEntry {
            value: label.addr as i64,
            path: relativize(&label.src, project_dir),
            line: label.line,
            sym_type: SymbolType::Label,
        });
    }

    // Consts (includes variables recorded as consts)
    for (name, cst) in &info.consts {
        syms.insert(name.clone(), SymbolEntry {
            value: cst.value,
            path: relativize(&cst.src, project_dir),
            line: cst.line,
            sym_type: SymbolType::Const,
        });
    }

    // Macros and macro params
    let all_macros = symbols.all_macros();
    for (name, macro_debug) in &info.macros {
        syms.insert(name.clone(), SymbolEntry {
            value: -1,
            path: relativize(&macro_debug.src, project_dir),
            line: macro_debug.line,
            sym_type: SymbolType::Macro,
        });

        // Emit macroparam entries — get defaults from MacroDef
        if let Some(macro_def) = all_macros.get(name) {
            for param in &macro_def.params {
                let key = format!("{}.{}", name, param.name);
                let value = param.default.as_ref()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .unwrap_or(-1);
                syms.insert(key, SymbolEntry {
                    value,
                    path: relativize(&macro_debug.src, project_dir),
                    line: macro_debug.line,
                    sym_type: SymbolType::MacroParam,
                });
            }
        }
    }

    // line_addresses — relativize paths
    let line_addresses: HashMap<String, HashMap<usize, Vec<u16>>> = info
        .line_addresses
        .iter()
        .map(|(path, lines)| (relativize(path, project_dir), lines.clone()))
        .collect();

    // data_lines — relativize paths, convert DataLineInfo → DataLineEntry
    let data_lines: HashMap<String, HashMap<usize, DataLineEntry>> = info
        .data_lines
        .iter()
        .map(|(path, lines)| {
            let entries = lines
                .iter()
                .map(|(&line, dli)| {
                    (line, DataLineEntry {
                        addr: dli.addr,
                        byte_length: dli.byte_length,
                        unit_bytes: dli.unit_bytes,
                    })
                })
                .collect();
            (relativize(path, project_dir), entries)
        })
        .collect();

    DebugSymbols {
        symbols: syms,
        line_addresses,
        data_lines,
    }
}

// ── Path helper ─────────────────────────────────────────────────────────────

pub fn relativize(path: &str, project_dir: &Path) -> String {
    let p = Path::new(path);
    match p.strip_prefix(project_dir) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.replace('\\', "/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relativize_strips_prefix() {
        let dir = Path::new("/home/user/project");
        assert_eq!(relativize("/home/user/project/main.asm", dir), "main.asm");
        assert_eq!(relativize("/home/user/project/sub/file.asm", dir), "sub/file.asm");
    }

    #[test]
    fn relativize_leaves_already_relative() {
        let dir = Path::new("/home/user/project");
        assert_eq!(relativize("main.asm", dir), "main.asm");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn relativize_windows_backslashes() {
        let dir = Path::new("C:\\Work\\project");
        assert_eq!(relativize("C:\\Work\\project\\main.asm", dir), "main.asm");
        assert_eq!(relativize("C:\\Work\\project\\sub\\file.asm", dir), "sub/file.asm");
    }
}
