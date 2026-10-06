use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use v6_core::assembler::Assembler;
use v6_core::diagnostics::AsmError;
use v6_core::preprocessor::preprocess;
use v6_core::project::CpuMode;

// ── TOML schema ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TestCase {
    cpu: Option<String>,
    quiet: Option<bool>,
    files: HashMap<String, String>,
    #[serde(default)]
    binary_files: HashMap<String, String>,
    #[serde(default)]
    include_dirs: Vec<String>,
    expect: Expected,
}

#[derive(Deserialize)]
struct Expected {
    rom: Option<String>,
    min_addr: Option<String>,
    #[serde(default)]
    symbols: HashMap<String, i64>,
    error_contains: Option<String>,
    error_line: Option<usize>,
    error_file: Option<String>,
    optional_enabled: Option<bool>,
    force_once: Option<bool>,
}

// ── helpers ─────────────────────────────────────────────────────────────────

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TestProject {
    root: PathBuf,
}

impl TestProject {
    fn from_case(case: &TestCase) -> Self {
        let unique = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("v6asm-tests-{}-{}", nanos, unique));
        fs::create_dir_all(&root).unwrap();

        for (path, content) in &case.files {
            let full_path = root.join(path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full_path, content.as_bytes()).unwrap();
        }

        for (path, hex) in &case.binary_files {
            let full_path = root.join(path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full_path, parse_hex_bytes(hex)).unwrap();
        }

        Self { root }
    }
}

impl Drop for TestProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn parse_hex_bytes(s: &str) -> Vec<u8> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).unwrap())
        .collect()
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(" ")
}

fn assemble_case(
    project: &TestProject,
    case: &TestCase,
) -> Result<Assembler, AsmError> {
    let cpu = match case.cpu.as_deref() {
        Some("z80") | Some("Z80") => CpuMode::Z80,
        _ => CpuMode::I8080,
    };
    let main_path = project.root.join("main.asm");
    let mut assembler = Assembler::new(cpu, project.root.clone());
    assembler.quiet = case.quiet.unwrap_or(true);

    let include_dirs: Vec<PathBuf> = case.include_dirs.iter().map(|d| project.root.join(d)).collect();
    let lines = preprocess(&main_path, &project.root, &include_dirs, &mut assembler.symbols, &|path| {
        fs::read_to_string(path).map_err(|err| AsmError::new(err.to_string()))
    })?;
    assembler.assemble(&lines)?;
    Ok(assembler)
}

// ── single test execution ───────────────────────────────────────────────────

struct TestResult {
    name: String,
    passed: bool,
    duration_ms: u128,
    details: String,
}

fn run_single_test(name: &str, toml_content: &str) -> TestResult {
    let start = Instant::now();
    let mut details = String::new();

    let case: TestCase = match toml::from_str(toml_content) {
        Ok(c) => c,
        Err(e) => {
            return TestResult {
                name: name.to_string(),
                passed: false,
                duration_ms: start.elapsed().as_millis(),
                details: format!("Failed to parse TOML: {}", e),
            };
        }
    };

    let project = TestProject::from_case(&case);
    let result = assemble_case(&project, &case);

    let passed = check_expectations(name, &case.expect, result, &mut details);

    TestResult {
        name: name.to_string(),
        passed,
        duration_ms: start.elapsed().as_millis(),
        details,
    }
}

fn check_expectations(
    _name: &str,
    expect: &Expected,
    result: Result<Assembler, AsmError>,
    details: &mut String,
) -> bool {
    let mut passed = true;

    // ── error expectations ──────────────────────────────────────────────
    if let Some(ref err_contains) = expect.error_contains {
        match result {
            Ok(_) => {
                writeln!(details, "  Expected error containing {:?} but assembly succeeded", err_contains).unwrap();
                return false;
            }
            Err(ref e) => {
                if !e.message.contains(err_contains.as_str()) {
                    writeln!(details, "  Error message mismatch:").unwrap();
                    writeln!(details, "    Expected to contain: {:?}", err_contains).unwrap();
                    writeln!(details, "    Got: {:?}", e.message).unwrap();
                    passed = false;
                }
                if let Some(expected_line) = expect.error_line {
                    if let Some(ref loc) = e.location {
                        if loc.line != expected_line {
                            writeln!(details, "  Error line mismatch: expected {}, got {}", expected_line, loc.line).unwrap();
                            passed = false;
                        }
                    } else {
                        writeln!(details, "  Expected error at line {} but error has no location", expected_line).unwrap();
                        passed = false;
                    }
                }
                if let Some(ref expected_file) = expect.error_file {
                    if let Some(ref loc) = e.location {
                        if loc.file != *expected_file {
                            writeln!(details, "  Error file mismatch: expected {:?}, got {:?}", expected_file, loc.file).unwrap();
                            passed = false;
                        }
                    } else {
                        writeln!(details, "  Expected error in file {:?} but error has no location", expected_file).unwrap();
                        passed = false;
                    }
                }
                return passed;
            }
        }
    }

    // ── success expectations ────────────────────────────────────────────
    let assembler = match result {
        Ok(a) => a,
        Err(e) => {
            writeln!(details, "  Assembly failed unexpectedly: {}", e.message).unwrap();
            if let Some(loc) = &e.location {
                writeln!(details, "    at {}", loc).unwrap();
            }
            return false;
        }
    };

    // ROM bytes
    if let Some(ref expected_rom_hex) = expect.rom {
        let expected_rom = parse_hex_bytes(expected_rom_hex);
        let actual_rom = assembler.output.extract_rom();
        if actual_rom != expected_rom {
            writeln!(details, "  ROM mismatch:").unwrap();
            writeln!(details, "    Expected: {}", bytes_to_hex(&expected_rom)).unwrap();
            writeln!(details, "    Got:      {}", bytes_to_hex(&actual_rom)).unwrap();
            // Show first difference
            for (i, (e, a)) in expected_rom.iter().zip(actual_rom.iter()).enumerate() {
                if e != a {
                    writeln!(details, "    First diff at byte {}: expected 0x{:02X}, got 0x{:02X}", i, e, a).unwrap();
                    break;
                }
            }
            if expected_rom.len() != actual_rom.len() {
                writeln!(details, "    Length: expected {}, got {}", expected_rom.len(), actual_rom.len()).unwrap();
            }
            passed = false;
        }
    }

    // min_addr
    if let Some(ref expected_min) = expect.min_addr {
        let expected = u16::from_str_radix(expected_min, 16).unwrap();
        let actual = assembler.output.min_addr();
        if actual != Some(expected) {
            writeln!(details, "  min_addr mismatch: expected 0x{:04X}, got {:?}", expected, actual).unwrap();
            passed = false;
        }
    }

    // Symbols
    for (sym_name, &expected_val) in &expect.symbols {
        let actual = assembler.symbols.resolve(sym_name);
        if actual != Some(expected_val) {
            writeln!(details, "  Symbol {:?} mismatch: expected {}, got {:?}", sym_name, expected_val, actual).unwrap();
            passed = false;
        }
    }

    // optional_enabled setting
    if let Some(expected_opt) = expect.optional_enabled {
        if assembler.settings.optional_enabled != expected_opt {
            writeln!(
                details,
                "  optional_enabled mismatch: expected {}, got {}",
                expected_opt, assembler.settings.optional_enabled
            ).unwrap();
            passed = false;
        }
    }

    // force_once setting
    if let Some(expected_fo) = expect.force_once {
        if assembler.settings.force_once != expected_fo {
            writeln!(
                details,
                "  force_once mismatch: expected {}, got {}",
                expected_fo, assembler.settings.force_once
            ).unwrap();
            passed = false;
        }
    }

    passed
}

// ── main test entry point ───────────────────────────────────────────────────

#[test]
fn assembler_regression_suite() {
    let cases_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("cases");
    let mut entries: Vec<_> = fs::read_dir(&cases_dir)
        .unwrap_or_else(|e| panic!("Cannot read test cases from {:?}: {}", cases_dir, e))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    assert!(!entries.is_empty(), "No .toml test cases found in {:?}", cases_dir);

    let mut results: Vec<TestResult> = Vec::new();
    let suite_start = Instant::now();

    for entry in &entries {
        let path = entry.path();
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Cannot read {:?}: {}", path, e));
        results.push(run_single_test(&name, &content));
    }

    let suite_duration = suite_start.elapsed();

    // ── generate report ─────────────────────────────────────────────────
    let mut report = String::new();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.iter().filter(|r| !r.passed).count();
    let total = results.len();

    writeln!(report, "\n{}", "=".repeat(70)).unwrap();
    writeln!(report, "  ASSEMBLER REGRESSION TEST REPORT").unwrap();
    writeln!(report, "{}", "=".repeat(70)).unwrap();

    for r in &results {
        let status = if r.passed { "PASS" } else { "FAIL" };
        writeln!(report, "  [{}] {} ({}ms)", status, r.name, r.duration_ms).unwrap();
        if !r.details.is_empty() {
            write!(report, "{}", r.details).unwrap();
        }
    }

    writeln!(report, "{}", "-".repeat(70)).unwrap();
    writeln!(
        report,
        "  {} passed, {} failed, {} total  ({:.1?})",
        passed, failed, total, suite_duration
    ).unwrap();
    writeln!(report, "{}", "=".repeat(70)).unwrap();

    // Always print the report
    println!("{}", report);

    if failed > 0 {
        panic!("{} test(s) failed – see report above", failed);
    }
}
