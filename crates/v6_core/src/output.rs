use std::collections::HashMap;
use std::path::Path;

use crate::assembler::{Assembler, ListingLine};
use crate::debug_symbols::build_debug_symbols;
use crate::diagnostics::{AsmError, AsmResult};

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

    for (file_idx, source) in asm.original_sources.iter().enumerate() {
        // File separator: empty line + filepath header (no leading empty line for first file)
        if file_idx > 0 {
            out.push('\n');
        }
        out.push_str(&format!("--- {} ---\n", source.file));

        let mut in_macro_def = false;

        for (line_idx, line_text) in source.lines.iter().enumerate() {
            let line_num = line_idx + 1;
            let trimmed = line_text.trim();
            let trimmed_upper = trimmed.to_ascii_uppercase();

            // Track macro definition blocks (print as source-only)
            if trimmed_upper.starts_with(".MACRO") && !trimmed_upper.starts_with(".MACRO_") {
                in_macro_def = true;
                format_source_only(&mut out, line_num, line_text);
                continue;
            }
            if in_macro_def {
                format_source_only(&mut out, line_num, line_text);
                if trimmed_upper == ".ENDMACRO" {
                    in_macro_def = false;
                }
                continue;
            }

            // .include directives: print as source-only
            if trimmed_upper.starts_with(".INCLUDE") {
                format_source_only(&mut out, line_num, line_text);
                continue;
            }

            // Look up assembled data for this line
            let key = (source.file.clone(), line_num);
            if let Some(entries) = lookup.get(&key) {
                // Find the "primary" entry (non-macro-expansion) for this source line
                let primary = entries.iter().find(|e| !e.macro_expansion);
                let macro_expanded: Vec<&&ListingLine> = entries.iter()
                    .filter(|e| e.macro_expansion)
                    .collect();

                if let Some(entry) = primary {
                    format_listing_line(&mut out, asm, entry, line_num, line_text);
                } else if !entries.is_empty() {
                    // All entries are macro expansions — this is a macro call line
                    format_source_only(&mut out, line_num, line_text);
                }

                // Print macro expansion lines (if any)
                for exp in &macro_expanded {
                    format_listing_line(&mut out, asm, exp, line_num, &exp.text);
                }
            } else {
                // No assembled data — just print the source line
                format_source_only(&mut out, line_num, line_text);
            }
        }
    }

    out
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

// ---- Debug symbols output ----

/// Generate debug symbols JSON from assembled data
pub fn generate_debug_symbols(asm: &Assembler) -> AsmResult<String> {
    let debug_symbols = build_debug_symbols(&asm.debug_info, &asm.symbols, asm.project_dir());
    serde_json::to_string_pretty(&debug_symbols)
        .map_err(|e| AsmError::new(format!("Failed to serialize debug symbols: {}", e)))
}

/// Write debug symbols JSON to disk
pub fn write_debug_symbols(json: &str, path: &Path) -> AsmResult<()> {
    std::fs::write(path, json)
        .map_err(|e| AsmError::new(format!("Failed to write debug symbols file: {}", e)))
}
