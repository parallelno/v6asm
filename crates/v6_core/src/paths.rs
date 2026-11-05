//! Shared directive path resolution.
//!
//! Every path operand (`.include`, `.incbin`, `.filesize`) is resolved by a
//! single [`PathResolver`]. The resolver captures the assembler invocation's
//! current working directory — the **compilation directory** — and the ordered
//! `-I` include directories once, so preprocessing and both assembly passes
//! always choose the same file for the same operand.

use std::path::{Path, PathBuf};

use crate::diagnostics::{AsmError, AsmResult};

/// Ordered search configuration shared by preprocessing and assembly.
#[derive(Debug, Clone)]
pub struct PathResolver {
    /// Absolute compilation directory (the assembler's current working directory).
    compilation_dir: PathBuf,
    /// Include directories in command-line order, made absolute against the
    /// compilation directory.
    include_dirs: Vec<PathBuf>,
}

impl PathResolver {
    /// Build a resolver from the compilation directory and the raw `-I` values.
    pub fn new(compilation_dir: PathBuf, include_dirs: &[PathBuf]) -> Self {
        let include_dirs = include_dirs
            .iter()
            .map(|dir| {
                if dir.is_absolute() {
                    dir.clone()
                } else {
                    compilation_dir.join(dir)
                }
            })
            .collect();
        Self {
            compilation_dir,
            include_dirs,
        }
    }

    /// The absolute compilation directory used for relative `-I` entries and
    /// as the final search location.
    pub fn compilation_dir(&self) -> &Path {
        &self.compilation_dir
    }

    /// Include directories in search order (relative ones resolved).
    pub fn include_dirs(&self) -> &[PathBuf] {
        &self.include_dirs
    }

    /// Resolve `operand`, a decoded directive path, relative to `source_dir`.
    ///
    /// Absolute operands are used as-is. Relative operands are tried in the
    /// source directory, then each include directory in order, then the
    /// compilation directory. The first existing file wins.
    pub fn resolve(&self, operand: &str, source_dir: &Path) -> AsmResult<PathBuf> {
        let candidate = Path::new(operand);
        if candidate.is_absolute() {
            return if candidate.is_file() {
                Ok(absolute(candidate))
            } else {
                Err(missing(operand))
            };
        }

        for base in std::iter::once(source_dir)
            .chain(self.include_dirs.iter().map(PathBuf::as_path))
            .chain(std::iter::once(self.compilation_dir.as_path()))
        {
            let path = base.join(candidate);
            if path.is_file() {
                return Ok(absolute(&path));
            }
        }

        Err(missing(operand))
    }
}

/// Error reported when no search location contains the operand.
fn missing(operand: &str) -> AsmError {
    AsmError::new(format!("Cannot find file: {}", operand))
}

/// Make `path` absolute against the process current directory when needed.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Render a resolved source path for diagnostics, listings, and DWARF.
///
/// The result is relative to the compilation directory when `path` lies inside
/// it (the usual case for a project), otherwise it stays absolute. Separators
/// are normalized to `/` so the same string can be used in DWARF directory and
/// file tables.
pub fn source_display(path: &Path, compilation_dir: &Path) -> String {
    let rendered = match path.strip_prefix(compilation_dir) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative,
        _ => path,
    };
    rendered.to_string_lossy().replace('\\', "/")
}

/// Decode a double-quoted path operand.
///
/// `input` must start with the opening quote. Returns the decoded value and the
/// remaining text after the closing quote. Escape sequences follow the lexer so
/// that a path means the same thing in `.include` and in `.incbin`.
pub fn decode_quoted_path(input: &str) -> Result<(String, String), String> {
    let mut chars = input.chars();
    if chars.next() != Some('"') {
        return Err("expected a double-quoted path".to_string());
    }

    let mut value = String::new();
    let mut consumed = 1usize;
    loop {
        let ch = chars
            .next()
            .ok_or_else(|| "unterminated path string".to_string())?;
        consumed += 1;
        match ch {
            '"' => break,
            '\\' => {
                let escaped = chars
                    .next()
                    .ok_or_else(|| "unterminated escape in path string".to_string())?;
                consumed += 1;
                value.push(decode_escape(escaped)?);
            }
            _ => value.push(ch),
        }
    }

    let tail: String = input.chars().skip(consumed).collect();
    Ok((value, tail))
}

/// Decode one escape sequence using the lexer's rules.
fn decode_escape(escaped: char) -> Result<char, String> {
    match escaped {
        'n' => Ok('\n'),
        't' => Ok('\t'),
        'r' => Ok('\r'),
        '0' => Ok('\0'),
        '\\' => Ok('\\'),
        '"' => Ok('"'),
        '\'' => Ok('\''),
        other => Err(format!("unknown escape sequence: \\{}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_quoted_path_decodes_escapes() {
        assert_eq!(
            decode_quoted_path(r#""dir\\file.bin" rest"#).unwrap(),
            ("dir\\file.bin".to_string(), " rest".to_string())
        );
        assert_eq!(
            decode_quoted_path(r#""a\"b.bin""#).unwrap(),
            ("a\"b.bin".to_string(), String::new())
        );
    }

    #[test]
    fn decode_quoted_path_rejects_bad_input() {
        assert!(decode_quoted_path("'single.asm'").is_err());
        assert!(decode_quoted_path("\"unterminated").is_err());
        assert!(decode_quoted_path(r#""bad\q""#).is_err());
    }

    #[test]
    fn source_display_is_relative_to_compilation_dir() {
        let comp_dir = if cfg!(windows) {
            Path::new(r"C:\project")
        } else {
            Path::new("/project")
        };
        let inside = comp_dir.join("src").join("main.asm");
        assert_eq!(source_display(&inside, comp_dir), "src/main.asm");

        let outside = if cfg!(windows) {
            Path::new(r"D:\shared\macros.asm")
        } else {
            Path::new("/shared/macros.asm")
        };
        assert_eq!(source_display(outside, comp_dir), outside.to_string_lossy().replace('\\', "/"));
    }

    #[test]
    fn resolve_prefers_source_directory_then_include_dirs_then_cwd() {
        let dir = std::env::temp_dir().join(format!(
            "v6asm-resolver-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let src = dir.join("src");
        let libs = dir.join("libs");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&libs).unwrap();
        std::fs::write(src.join("dup.bin"), b"src").unwrap();
        std::fs::write(libs.join("dup.bin"), b"libs").unwrap();
        std::fs::write(libs.join("only_lib.bin"), b"libs").unwrap();
        std::fs::write(dir.join("cwd_only.bin"), b"cwd").unwrap();

        let resolver = PathResolver::new(dir.clone(), &[PathBuf::from("libs")]);

        assert_eq!(resolver.resolve("dup.bin", &src).unwrap(), src.join("dup.bin"));
        assert_eq!(
            resolver.resolve("only_lib.bin", &src).unwrap(),
            libs.join("only_lib.bin")
        );
        assert_eq!(
            resolver.resolve("cwd_only.bin", &src).unwrap(),
            dir.join("cwd_only.bin")
        );

        let missing = resolver.resolve("missing.bin", &src).unwrap_err();
        assert_eq!(missing.message, "Cannot find file: missing.bin");

        let _ = std::fs::remove_dir_all(&dir);
    }
}