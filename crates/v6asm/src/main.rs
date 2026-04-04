use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use clap::{CommandFactory, Parser};
use v6_core::assembler::Assembler;
use v6_core::diagnostics::{AsmError, AsmResult};
use v6_core::output::{generate_listing, generate_rom, rom_start_address, write_listing, write_rom, RomConfig};
use v6_core::preprocessor;
use v6_core::project::CpuMode;
use v6_core::symbols::SymbolTable;

/// Embedded source template
const TEMPLATE_ASM: &str = include_str!("templates/main.asm");
const VERSION: &str = env!("V6ASM_VERSION");
const ABOUT: &str = concat!(
    "Intel 8080/Z80 assembler, version ",
    env!("V6ASM_VERSION"),
    "\n",
    "(c) Aleksandr Fedotovskikh <mailforfriend@gmail.com>",
);

/// v6asm — Vector-06c Assembler
#[derive(Parser)]
#[command(
    name = "v6asm",
    about = ABOUT,
    disable_version_flag = true
)]
struct Cli {
    /// Assembly source file (.asm) to compile
    source: Option<PathBuf>,

    /// Initialize a new project with the given name
    #[arg(short = 'i', long = "init")]
    init: Option<String>,

    /// Output ROM path (default: <source>.rom)
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Target CPU: i8080 (default) or z80
    #[arg(short = 'c', long = "cpu", default_value = "i8080")]
    cpu: String,

    /// ROM size alignment in bytes
    #[arg(short = 'a', long = "rom-align", default_value = "1")]
    rom_align: u16,

    /// Suppress .print output
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,

    /// Extra diagnostics
    #[arg(short = 'V', long = "verbose")]
    verbose: bool,

    /// Generate listing file (.lst) alongside the ROM
    #[arg(short = 'l', long = "lst")]
    lst: bool,

    /// Print version information
    #[arg(short = 'v', long = "version")]
    version: bool,
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();
    let started_at = Instant::now();

    if cli.version {
        println!("{}", version_string());
        return;
    }

    let result = if let Some(ref name) = cli.init {
        cmd_init(name)
    } else if let Some(ref source_path) = cli.source {
        cmd_assemble(source_path, &cli)
    } else {
        let mut cmd = Cli::command();
        cmd.print_help().unwrap();
        println!();
        return;
    };

    if let Err(e) = result {
        print_error(&e);
        process::exit(1);
    }

    if cli.source.is_some() {
        eprintln!("Compilation completed in {}", format_elapsed_time(started_at.elapsed()));
    }
}

fn version_string() -> &'static str {
    VERSION
}

#[cfg(test)]
fn render_help_text() -> String {
    use clap::CommandFactory;

    let mut command = Cli::command();
    command.render_help().to_string()
}

fn format_elapsed_time(elapsed: std::time::Duration) -> String {
    let elapsed_seconds = elapsed.as_secs_f64();

    if elapsed_seconds >= 3600.0 {
        format!("{:.3} hours", elapsed_seconds / 3600.0)
    } else if elapsed_seconds >= 1.0 {
        format!("{:.3} seconds", elapsed_seconds)
    } else {
        format!("{:.0} ms", elapsed.as_secs_f64() * 1000.0)
    }
}

fn print_error(e: &AsmError) {
    if let Some(ref loc) = e.location {
        eprintln!("error: {}   -->   {}:{}", e.message, loc.file, loc.line);
    } else {
        eprintln!("error: {}", e.message);
    }
    if let Some(ref src) = e.source_line {
        eprintln!("  {}", src);
    }
    eprintln!();
}

// ---- Init command ----

#[cfg(test)]
mod tests {
    use super::{format_elapsed_time, render_help_text, version_string, Cli};
    use crate::ABOUT;
    use clap::Parser;
    use std::time::Duration;

    #[test]
    fn formats_subsecond_durations_in_milliseconds() {
        assert_eq!(format_elapsed_time(Duration::from_millis(39)), "39 ms");
    }

    #[test]
    fn formats_second_durations_in_seconds() {
        assert_eq!(format_elapsed_time(Duration::from_millis(1234)), "1.234 seconds");
    }

    #[test]
    fn formats_hour_durations_in_hours() {
        assert_eq!(format_elapsed_time(Duration::from_secs(5400)), "1.500 hours");
    }

    #[test]
    fn parses_requested_short_flags() {
        let cli = Cli::try_parse_from([
            "v6asm",
            "main.asm",
            "-c",
            "z80",
            "-a",
            "16",
            "-l",
            "-o",
            "out.rom",
            "-V",
        ]).unwrap();

        assert_eq!(cli.source.as_deref(), Some(std::path::Path::new("main.asm")));
        assert_eq!(cli.cpu, "z80");
        assert_eq!(cli.rom_align, 16);
        assert!(cli.lst);
        assert!(cli.verbose);
    }

    #[test]
    fn parses_init_short_flag() {
        let cli = Cli::try_parse_from(["v6asm", "-i", "demo"]).unwrap();
        assert_eq!(cli.init.as_deref(), Some("demo"));
    }

    #[test]
    fn parses_version_flag() {
        let cli = Cli::try_parse_from(["v6asm", "-v"]).unwrap();
        assert!(cli.version);
    }

    #[test]
    fn help_text_starts_with_custom_header() {
        let help = render_help_text();
        assert!(help.starts_with(ABOUT));
        assert!(help.contains("-i, --init"));
        assert!(help.contains("-v, --version"));
    }

    #[test]
    fn version_string_comes_from_build_metadata() {
        assert_eq!(version_string(), env!("V6ASM_VERSION"));
        assert!(version_string().contains('-'));
    }
}

fn cmd_init(name: &str) -> Result<(), AsmError> {
    let asm_file = format!("{}.asm", name);

    if Path::new(&asm_file).exists() {
        return Err(AsmError::new(format!("{} already exists", asm_file)));
    }

    std::fs::write(&asm_file, TEMPLATE_ASM)
        .map_err(|e| AsmError::new(format!("Cannot write {}: {}", asm_file, e)))?;

    eprintln!("Created source: {}", asm_file);
    Ok(())
}

// ---- Assemble command ----

fn cmd_assemble(source_path: &Path, cli: &Cli) -> Result<(), AsmError> {
    if !source_path.exists() {
        return Err(AsmError::new(format!("Source file not found: {}", source_path.display())));
    }

    let source_dir = source_path.parent().unwrap_or(Path::new(".")).to_path_buf();

    // Preprocess sources
    let mut symbols = SymbolTable::new();
    let read_file_fn = |path: &Path| -> AsmResult<String> {
        std::fs::read_to_string(path)
            .map_err(|e| AsmError::new(format!("Cannot read {}: {}", path.display(), e)))
    };
    let lines = preprocessor::preprocess(source_path, &source_dir, &mut symbols, &read_file_fn)?;

    // Collect original sources for listing
    let original_sources = preprocessor::collect_original_sources(source_path, &source_dir, &read_file_fn)?;

    // Assemble
    let cpu_mode = match cli.cpu.as_str() {
        "z80" => CpuMode::Z80,
        _ => CpuMode::I8080,
    };
    let mut asm = Assembler::new(cpu_mode, source_dir.clone());
    asm.quiet = cli.quiet;
    asm.symbols = symbols;
    asm.original_sources = original_sources;

    asm.assemble(&lines)?;

    // Generate ROM
    let rom_config = RomConfig {
        rom_align: cli.rom_align,
    };
    let rom = generate_rom(&asm, &rom_config);

    let rom_path = cli.output.clone().unwrap_or_else(|| {
        source_path.with_extension("rom")
    });

    // Ensure output directory exists
    if let Some(parent) = rom_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AsmError::new(format!("Cannot create output directory: {}", e)))?;
        }
    }

    write_rom(&rom, &rom_path)?;

    let start = rom_start_address(&asm);
    eprintln!(
        "ROM: {} bytes, start: 0x{:04X}, written to {}",
        rom.len(),
        start,
        rom_path.display()
    );

    // Generate listing file
    if cli.lst {
        let lst_path = rom_path.with_extension("lst");
        let listing = generate_listing(&asm);
        write_listing(&listing, &lst_path)?;
        if cli.verbose {
            eprintln!("Listing written to {}", lst_path.display());
        }
    }

    Ok(())
}
