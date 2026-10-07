use std::collections::HashMap;
use std::path::Path;

use crate::assembler::{Assembler, DebugLineRow, EmissionKind, ListingLine};
use crate::diagnostics::{AsmError, AsmResult};
use crate::object::dwarf::DebugConstant;
use crate::object::elf::{self, ObjSymbol, SymBinding, SymLocation, SymType};
use crate::object::section::RelocTarget;

// Maximum number of bytes to display in the listing BYTES column
const LISTING_MAX_BYTES: usize = 8;

/// ROM output configuration
pub struct RomConfig {
    pub rom_align: u16,
}

impl Default for RomConfig {
    fn default() -> Self {
        Self { rom_align: 1 }
    }
}

/// Generate the ROM binary from assembled output
pub fn generate_rom(asm: &Assembler, config: &RomConfig) -> Vec<u8> {
    let mut rom = asm.output.extract_rom();

    // Apply ROM alignment (pad end to multiple of rom_align)
    if config.rom_align > 1 {
        let align = config.rom_align as usize;
        let remainder = rom.len() % align;
        if remainder != 0 {
            rom.resize(rom.len() + (align - remainder), 0);
        }
    }

    rom
}

/// Get the start address of the ROM
pub fn rom_start_address(asm: &Assembler) -> u16 {
    asm.output.min_addr().unwrap_or(0)
}

/// Write ROM to file
pub fn write_rom(rom: &[u8], path: &Path) -> AsmResult<()> {
    std::fs::write(path, rom)
        .map_err(|e| AsmError::new(format!("Failed to write ROM file: {}", e)))
}

/// Build an `ET_EXEC` debug companion for direct ROM output.
pub fn generate_debug_companion(asm: &Assembler, config: &RomConfig) -> Vec<u8> {
    let image = generate_rom(asm, config);
    let mut symbols = Vec::new();
    let mut names: Vec<_> = asm.symbols.all_globals()
        .values()
        .filter(|info| {
            (info.is_code_label || is_debug_constant(info))
                && !info.original_name.starts_with('@')
                && !is_macro_scoped_name(&info.original_name)
        })
        .collect();
    names.sort_by(|left, right| left.original_name.cmp(&right.original_name));
    for info in names {
        let offset = info.value.unwrap_or(0) as u32;
        let (kind, size) = if info.is_code_label {
            debug_symbol_kind_and_size(
                offset,
                None,
                next_debug_label_offset(asm, None, offset),
                &asm.debug_rows,
            )
        } else {
            (SymType::NoType, 0)
        };
        symbols.push(ObjSymbol {
            name: info.original_name.clone(),
            binding: SymBinding::Local,
            kind,
            size,
            location: SymLocation::Absolute(info.value.unwrap_or(0) as u32),
        });
    }
    let constants = debug_constants(asm);
    let debug_sections = crate::object::dwarf::debug_sections(
        &asm.debug_rows,
        &constants,
        asm.project_dir(),
    );
    elf::serialize_executable(&image, rom_start_address(asm), &symbols, &debug_sections)
}

/// Write the direct-ROM debug companion ELF.
pub fn write_debug_companion(asm: &Assembler, config: &RomConfig, path: &Path) -> AsmResult<()> {
    std::fs::write(path, generate_debug_companion(asm, config))
        .map_err(|e| AsmError::new(format!("Failed to write debug ELF file: {}", e)))
}

// ---- Object (ELF) output ----

/// Configuration for object-file emission.
#[derive(Debug, Clone, Default)]
pub struct ObjConfig {
    /// Emit DWARF v4 source metadata for debugger consumers.
    pub debug: bool,
}

/// Build the ELF symbol model from the assembler's object state and serialize a
/// relocatable ELF32 object.
pub fn generate_object(asm: &Assembler, config: &ObjConfig) -> AsmResult<Vec<u8>> {
    let sections = &asm.obj.sections;
    let mut symbols: Vec<ObjSymbol> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    let mut push_named = |name: &str, binding: SymBinding| {
        if seen.iter().any(|s| s.eq_ignore_ascii_case(name)) {
            return;
        }
        let (kind, size, location) = match asm.symbols.get_global_info(name) {
            Some(info) => {
                if let Some(sec) = info.section {
                    let offset = info.value.unwrap_or(0) as u32;
                    let (kind, size) = if info.is_code_label {
                        debug_symbol_kind_and_size(
                            offset,
                            Some(sec),
                            next_debug_label_offset(asm, Some(sec), offset),
                            &asm.debug_rows,
                        )
                    } else {
                        (SymType::NoType, 0)
                    };
                    (kind, size, SymLocation::Section { index: sec, offset })
                } else if let Some(v) = info.value {
                    (SymType::NoType, 0, SymLocation::Absolute(v as u32))
                } else {
                    (SymType::NoType, 0, SymLocation::Undefined)
                }
            }
            None => (SymType::NoType, 0, SymLocation::Undefined),
        };
        seen.push(name.to_string());
        symbols.push(ObjSymbol { name: name.to_string(), binding, kind, size, location });
    };

    // Exported (.globl) and weak symbols first.
    for name in &asm.obj.globls {
        push_named(name, SymBinding::Global);
    }
    for name in &asm.obj.weaks {
        push_named(name, SymBinding::Weak);
    }

    // Preserve module-level labels for debugger lookup. Section symbols remain
    // the relocation targets, so adding these local symbols does not alter the
    // existing relocation model.
    if config.debug {
        let mut local_names: Vec<String> = asm.symbols.all_globals()
            .values()
            .filter(|info| {
                (info.is_code_label || is_debug_constant(info))
                    && !info.original_name.starts_with('@')
                    && !is_macro_scoped_name(&info.original_name)
            })
            .map(|info| info.original_name.clone())
            .collect();
        local_names.sort();
        for name in &local_names {
            push_named(name, SymBinding::Local);
        }
    }

    // `.global *` — export every module-level symbol (excluding `@`-prefixed locals).
    if asm.obj.glob_all {
        let mut all_names: Vec<String> = asm.symbols.all_globals()
            .values()
            .filter(|info| !info.original_name.starts_with('@') && !is_macro_scoped_name(&info.original_name))
            .map(|info| info.original_name.clone())
            .collect();
        all_names.sort();
        for name in &all_names {
            push_named(name, SymBinding::Global);
        }
    }

    // Undefined externals referenced by relocations.
    for sec in sections {
        for r in &sec.relocs {
            if let RelocTarget::Symbol(name) = &r.target {
                push_named(name, SymBinding::Global);
            }
        }
    }

    let constants = debug_constants(asm);
    let debug_sections = config.debug
        .then(|| crate::object::dwarf::debug_sections(&asm.debug_rows, &constants, asm.project_dir()))
        .unwrap_or_default();
    Ok(elf::serialize_with_extra(sections, &symbols, &debug_sections))
}

fn is_debug_constant(info: &crate::symbols::SymbolInfo) -> bool {
    !info.is_code_label && !info.is_mutable && info.section.is_none() && info.value.is_some()
}

/// A symbol whose name contains `.` is a per-invocation macro-scoped symbol
/// (`MacroName_<call-index>.Name`). These are assembler-generated and are
/// intentionally omitted from the debugger-facing named-symbol set because
/// their source spelling is synthetic; see `docs/debug-metadata.md`.
fn is_macro_scoped_name(name: &str) -> bool {
    name.contains('.')
}

fn debug_constants(asm: &Assembler) -> Vec<DebugConstant> {
    let mut constants: Vec<_> = asm.symbols.all_globals()
        .values()
        .filter(|info| is_debug_constant(info) && !info.original_name.starts_with('@') && !is_macro_scoped_name(&info.original_name))
        .map(|info| DebugConstant {
            name: info.original_name.clone(),
            file: info.file.clone(),
            line: info.line,
            value: info.value.unwrap(),
        })
        .collect();
    constants.sort_by(|left, right| left.name.cmp(&right.name));
    constants
}

fn debug_symbol_kind_and_size(
    offset_or_address: u32,
    section: Option<usize>,
    end_bound: Option<u32>,
    rows: &[DebugLineRow],
) -> (SymType, u32) {
    let matching_rows: Vec<&DebugLineRow> = rows
        .iter()
        .filter(|row| {
            row.section == section
                && row.offset_or_address >= offset_or_address
                && end_bound.is_none_or(|end| row.offset_or_address < end)
        })
        .collect();
    let Some(first) = matching_rows
        .iter()
        .find(|row| row.offset_or_address == offset_or_address)
    else {
        return (SymType::NoType, 0);
    };

    match first.kind {
        EmissionKind::Data | EmissionKind::Storage => (SymType::Object, first.byte_len),
        EmissionKind::Instruction => {
            let end = matching_rows
                .iter()
                .take_while(|row| row.kind == EmissionKind::Instruction)
                .map(|row| row.offset_or_address + row.byte_len)
                .max()
                .unwrap_or(offset_or_address);
            (SymType::Func, end - offset_or_address)
        }
        EmissionKind::Padding => (SymType::NoType, 0),
    }
}

fn next_debug_label_offset(asm: &Assembler, section: Option<usize>, offset_or_address: u32) -> Option<u32> {
    asm.symbols
        .all_globals()
        .values()
        .filter(|info| {
            info.is_code_label
                && info.section == section
                && info.value.is_some_and(|value| value as u32 > offset_or_address)
                && !is_macro_scoped_name(&info.original_name)
        })
        .filter_map(|info| info.value.map(|value| value as u32))
        .min()
}

/// Write a relocatable ELF object to file.
pub fn write_object(asm: &Assembler, config: &ObjConfig, path: &Path) -> AsmResult<()> {
    let bytes = generate_object(asm, config)?;
    std::fs::write(path, bytes)
        .map_err(|e| AsmError::new(format!("Failed to write object file: {}", e)))
}

// ---- Listing file output ----

/// Generate listing file content from assembled data.
///
/// If original sources are available, walks through them to produce the listing
/// with proper file headers and directive lines. Otherwise falls back to
/// listing_data order.
pub fn generate_listing(asm: &Assembler) -> String {
    if asm.original_sources.is_empty() {
        return generate_listing_fallback(asm);
    }

    let mut out = String::new();
    out.push_str("ADDR   BYTES                    SOURCE\n");

    // Build lookup: (file, line_num) -> list of ListingLine entries in order
    let mut lookup: HashMap<(String, usize), Vec<&ListingLine>> = HashMap::new();
    for entry in &asm.listing_data {
        lookup.entry((entry.file.clone(), entry.line_num))
            .or_default()
            .push(entry);
    }

    // Walk sources recursively: original_sources is in depth-first order,
    // so we use an index that advances as includes are encountered.
    let mut source_idx = 0;
    emit_source_listing(&mut out, asm, &lookup, &mut source_idx, true);

    out
}

/// Recursively emit listing for one source file, inlining includes at their directive position.
fn emit_source_listing(
    out: &mut String,
    asm: &Assembler,
    lookup: &HashMap<(String, usize), Vec<&ListingLine>>,
    source_idx: &mut usize,
    is_first: bool,
) {
    let idx = *source_idx;
    if idx >= asm.original_sources.len() {
        return;
    }
    let source = &asm.original_sources[idx];
    *source_idx += 1;

    // File header
    if !is_first {
        out.push('\n');
    }
    out.push_str(&format!("--- {} ---\n", source.file));

    let file_name = source.file.clone();
    let mut in_macro_def = false;

    for (line_idx, line_text) in source.lines.iter().enumerate() {
        let line_num = line_idx + 1;
        let trimmed = line_text.trim();
        let trimmed_upper = trimmed.to_ascii_uppercase();

        // Track macro definition blocks (print as source-only)
        if trimmed_upper.starts_with(".MACRO") && !trimmed_upper.starts_with(".MACRO_") {
            in_macro_def = true;
            format_source_only(out, line_num, line_text);
            continue;
        }
        if in_macro_def {
            format_source_only(out, line_num, line_text);
            if trimmed_upper == ".ENDMACRO" {
                in_macro_def = false;
            }
            continue;
        }

        // .include directives: print the directive, then inline the included file
        if trimmed_upper.starts_with(".INCLUDE") {
            format_source_only(out, line_num, line_text);
            emit_source_listing(out, asm, lookup, source_idx, false);
            // Resume header for current file after inclusion
            out.push('\n');
            out.push_str(&format!("--- {} ---\n", file_name));
            continue;
        }

        // Look up assembled data for this line
        let key = (file_name.clone(), line_num);
        if let Some(entries) = lookup.get(&key) {
            // A single physical line can produce several listing entries when
            // the `\` line separator is used: each statement becomes its own
            // ListingLine sharing the same (file, line_num). Emit one row per
            // primary entry; only the first row carries the source text so
            // bytes from later statements line up beneath it.
            let primaries: Vec<&&ListingLine> = entries.iter()
                .filter(|e| !e.macro_expansion)
                .collect();
            let macro_expanded: Vec<&&ListingLine> = entries.iter()
                .filter(|e| e.macro_expansion)
                .collect();

            if primaries.is_empty() {
                if !entries.is_empty() {
                    // All entries are macro expansions — this is a macro call line
                    format_source_only(out, line_num, line_text);
                }
            } else {
                for (i, entry) in primaries.iter().enumerate() {
                    let text = if i == 0 { line_text.as_str() } else { "" };
                    format_listing_line(out, asm, entry, line_num, text);
                }
            }

            // Print macro expansion lines (if any)
            for exp in &macro_expanded {
                format_listing_line(out, asm, exp, line_num, &exp.text);
            }
        } else {
            // No assembled data — just print the source line
            format_source_only(out, line_num, line_text);
        }
    }
}

/// Format a source-only line (no address/bytes)
fn format_source_only(out: &mut String, line_num: usize, text: &str) {
    out.push_str(&format!(
        "       {} {:>5}  {}\n",
        " ".repeat(24), line_num, text
    ));
}

/// Format a listing line with address and bytes
fn format_listing_line(out: &mut String, asm: &Assembler, entry: &ListingLine, line_num: usize, text: &str) {
    let is_storage = text.trim_start().to_ascii_uppercase().starts_with(".STORAGE");
    let addr_str = if entry.byte_count > 0 || is_storage {
        format!("{:04X}", entry.addr)
    } else {
        "    ".to_string()
    };

    let bytes_str = if entry.byte_count > 0 {
        let display_count = entry.byte_count.min(LISTING_MAX_BYTES);
        let mut hex_parts: Vec<String> = Vec::with_capacity(display_count);
        for i in 0..display_count {
            let addr = entry.addr.wrapping_add(i as u16);
            let b = asm.output.read_byte(addr).unwrap_or(0);
            hex_parts.push(format!("{:02X}", b));
        }
        let hex = hex_parts.join(" ");
        if entry.byte_count > LISTING_MAX_BYTES {
            format!("{:<23}+", hex)
        } else {
            format!("{:<24}", hex)
        }
    } else {
        " ".repeat(24)
    };

    out.push_str(&format!(
        "{}   {} {:>5}  {}\n",
        addr_str, bytes_str, line_num, text
    ));
}

/// Fallback: generate listing from listing_data when original sources are not available
fn generate_listing_fallback(asm: &Assembler) -> String {
    let mut out = String::new();
    out.push_str("ADDR   BYTES                    SOURCE\n");

    for entry in &asm.listing_data {
        format_listing_line(&mut out, asm, entry, entry.line_num, &entry.text);
    }

    out
}

/// Write listing file to disk
pub fn write_listing(listing: &str, path: &Path) -> AsmResult<()> {
    std::fs::write(path, listing)
        .map_err(|e| AsmError::new(format!("Failed to write listing file: {}", e)))
}
