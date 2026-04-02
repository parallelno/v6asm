use std::path::Path;

use crate::assembler::Assembler;
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

/// Generate listing file content from assembled data
pub fn generate_listing(asm: &Assembler) -> String {
    let mut out = String::new();
    out.push_str("ADDR   BYTES                    SOURCE\n");

    for entry in &asm.listing_data {
        let is_storage = entry.text.trim_start().to_ascii_lowercase().starts_with(".storage");
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
            addr_str, bytes_str, entry.line_num, entry.text
        ));
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
