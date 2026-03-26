use std::path::{Path, PathBuf};
use std::process;
use std::time::Instant;

use clap::Parser;
use v6_core::assembler::Assembler;
use v6_core::diagnostics::{AsmError, AsmResult};
use v6_core::fdd::filesystem::Filesystem;
use v6_core::output::{generate_debug_json, generate_rom, rom_start_address, write_debug_json, write_rom, RomConfig};
use v6_core::preprocessor;
use v6_core::project::ProjectConfig;
use v6_core::symbols::SymbolTable;

/// Embedded project template
const TEMPLATE_ASM: &str = include_str!("templates/main.asm");

/// Embedded FDD template
const TEMPLATE_FDD: &[u8] = include_bytes!("../../../crates/v6_core/src/fdd/rds308.fdd");

/// v6asm — Vector-06c Assembler
#[derive(Parser)]
#[command(name = "v6asm", about = "Assembler for Vector-06c (Intel 8080 / Z80 compatible)")]
struct Cli {
    /// Project file (.project.json) to assemble
    project: Option<PathBuf>,

    /// Initialize a new project with the given name
    #[arg(long = "init")]
    init: Option<String>,

    /// Compile dependent projects before the main one
    #[arg(long = "deps")]
    deps: Option<PathBuf>,

    /// Suppress .print output
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,

    /// Extra diagnostics
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
}

fn main() {
    env_logger::init();
    let cli = Cli::parse();
    let started_at = Instant::now();

    let result = if let Some(ref name) = cli.init {
        cmd_init(name)
    } else if let Some(ref deps_path) = cli.deps {
        cmd_deps(deps_path, &cli)
    } else if let Some(ref project_path) = cli.project {
        cmd_assemble(project_path, &cli)
    } else {
        eprintln!("Usage: v6asm <project.json> | --init <name> | --deps <project.json>");
        process::exit(1);
    };

    if let Err(e) = result {
        print_error(&e);
        process::exit(1);
    }

    if cli.project.is_some() || cli.deps.is_some() {
        eprintln!("Compilation completed in {}", format_elapsed_time(started_at.elapsed()));
    }
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
        eprint!("{}:{}:{}: ", loc.file, loc.line, loc.col);
    }
    eprintln!("error: {}", e.message);
    if let Some(ref src) = e.source_line {
        eprintln!("  {}", src);
    }
}

// ---- Init command ----

#[cfg(test)]
mod tests {
    use super::format_elapsed_time;
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
}

fn cmd_init(name: &str) -> Result<(), AsmError> {
    let project_file = format!("{}.project.json", name);
    let asm_file = format!("{}.asm", name);

    if Path::new(&project_file).exists() {
        return Err(AsmError::new(format!("{} already exists", project_file)));
    }

    let config = serde_json::json!({
        "name": name,
        "asmPath": asm_file,
        "debugPath": format!("{}.debug.json", name),
        "romPath": format!("{}.rom", name),
        "cpu": "i8080",
        "settings": {}
    });

    std::fs::write(&project_file, serde_json::to_string_pretty(&config).unwrap())
        .map_err(|e| AsmError::new(format!("Cannot write {}: {}", project_file, e)))?;

    std::fs::write(&asm_file, TEMPLATE_ASM)
        .map_err(|e| AsmError::new(format!("Cannot write {}: {}", asm_file, e)))?;

    std::fs::create_dir_all("out")
        .map_err(|e| AsmError::new(format!("Cannot create out/: {}", e)))?;

    eprintln!("Created project: {}", project_file);
    eprintln!("Created source:  {}", asm_file);
    Ok(())
}

// ---- Deps command ----

fn cmd_deps(deps_path: &Path, cli: &Cli) -> Result<(), AsmError> {
    let config = load_project(deps_path)?;
    let project_dir = deps_path.parent().unwrap_or(Path::new("."));

    // Look for dependentProjectsDir
    if let Some(ref dep_dir_str) = config.dependent_projects_dir {
        let dep_dir = project_dir.join(dep_dir_str);
        if dep_dir.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(&dep_dir)
                .map_err(|e| AsmError::new(format!("Cannot read {}: {}", dep_dir.display(), e)))?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path().extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext == "json")
                        .unwrap_or(false)
                    && e.path().to_string_lossy().contains(".project.")
                })
                .collect();
            entries.sort_by_key(|e| e.file_name());

            for entry in entries {
                eprintln!("Compiling dependency: {}", entry.path().display());
                cmd_assemble(&entry.path(), cli)?;
            }
        }
    }

    // Now compile the main project
    cmd_assemble(deps_path, cli)
}

// ---- Assemble command ----

fn cmd_assemble(project_path: &Path, cli: &Cli) -> Result<(), AsmError> {
    let config = load_project(project_path)?;
    let project_dir = project_path.parent().unwrap_or(Path::new(".")).to_path_buf();

    let asm_path = project_dir.join(&config.asm_path);
    if !asm_path.exists() {
        return Err(AsmError::new(format!("Source file not found: {}", asm_path.display())));
    }

    // Preprocess sources
    let mut symbols = SymbolTable::new();
    let read_file_fn = |path: &Path| -> AsmResult<String> {
        std::fs::read_to_string(path)
            .map_err(|e| AsmError::new(format!("Cannot read {}: {}", path.display(), e)))
    };
    let lines = preprocessor::preprocess(&asm_path, &project_dir, &mut symbols, &read_file_fn)?;

    // Assemble
    let cpu_mode = config.cpu_mode();
    let mut asm = Assembler::new(cpu_mode, project_dir.clone());
    asm.quiet = cli.quiet;
    asm.symbols = symbols;

    asm.assemble(&lines)?;
    asm.collect_macro_debug_info();

    // Generate ROM
    let rom_config = RomConfig {
        rom_align: config.rom_align.unwrap_or(1) as u16,
    };
    let rom = generate_rom(&asm, &rom_config);

    let rom_path_str = config.rom_path.as_deref().unwrap_or("output.rom");
    let rom_path = project_dir.join(rom_path_str);

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

    // Generate debug JSON
    let project_file_name = project_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let debug_json = generate_debug_json(&asm.debug_info, &project_file_name)?;

    if let Some(ref debug_path_str) = config.debug_path {
        let debug_path = project_dir.join(debug_path_str);
        write_debug_json(&debug_json, &debug_path)?;
        if cli.verbose {
            eprintln!("Debug JSON written to {}", debug_path.display());
        }
    }

    // FDD image generation
    if let Some(ref fdd_path_str) = config.fdd_path {
        let fdd_path = project_dir.join(fdd_path_str);
        build_fdd(&rom, &rom_path, &fdd_path, &config, &project_dir)?;
        eprintln!("FDD image written to {}", fdd_path.display());
    }

    Ok(())
}

fn build_fdd(
    _rom: &[u8],
    rom_path: &Path,
    fdd_path: &Path,
    config: &ProjectConfig,
    project_dir: &Path,
) -> Result<(), AsmError> {
    // Load template
    let mut fs = if let Some(ref tpl_path) = config.fdd_template_path {
        let tpl = project_dir.join(tpl_path);
        let data = std::fs::read(&tpl)
            .map_err(|e| AsmError::new(format!("Cannot read FDD template {}: {}", tpl.display(), e)))?;
        Filesystem::from_bytes(&data)
    } else {
        Filesystem::from_bytes(TEMPLATE_FDD)
    };

    // Add ROM file
    let rom_data = std::fs::read(rom_path)
        .map_err(|e| AsmError::new(format!("Cannot read ROM {}: {}", rom_path.display(), e)))?;
    let rom_name = rom_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "OUTPUT.ROM".to_string());
    fs.save_file(&rom_name, &rom_data)
        .ok_or_else(|| AsmError::new("Disk full when adding ROM file"))?;

    // Add content files
    if let Some(ref content_dir_str) = config.fdd_content_path {
        let content_dir = project_dir.join(content_dir_str);
        if content_dir.is_dir() {
            add_content_files_recursive(&mut fs, &content_dir)?;
        }
    }

    // Ensure output directory exists
    if let Some(parent) = fdd_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AsmError::new(format!("Cannot create output directory: {}", e)))?;
        }
    }

    std::fs::write(fdd_path, &fs.bytes)
        .map_err(|e| AsmError::new(format!("Cannot write FDD image: {}", e)))?;

    Ok(())
}

fn add_content_files_recursive(fs: &mut Filesystem, dir: &Path) -> Result<(), AsmError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| AsmError::new(format!("Cannot read {}: {}", dir.display(), e)))?;

    let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    sorted.sort_by_key(|e| e.file_name());

    for entry in sorted {
        let path = entry.path();
        if path.is_dir() {
            add_content_files_recursive(fs, &path)?;
        } else {
            let data = std::fs::read(&path)
                .map_err(|e| AsmError::new(format!("Cannot read {}: {}", path.display(), e)))?;
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "UNKNOWN".to_string());
            fs.save_file(&name, &data)
                .ok_or_else(|| AsmError::new(format!("Disk full when adding {}", name)))?;
        }
    }
    Ok(())
}

fn load_project(path: &Path) -> Result<ProjectConfig, AsmError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| AsmError::new(format!("Cannot read {}: {}", path.display(), e)))?;
    let config: ProjectConfig = serde_json::from_str(&text)
        .map_err(|e| AsmError::new(format!("Invalid project file {}: {}", path.display(), e)))?;
    Ok(config)
}
