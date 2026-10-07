use std::fmt;

/// Source location for error reporting
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: String,
    pub line: usize,
    pub col: usize,
}

impl fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}

/// Assembler error types
#[derive(Debug, Clone)]
pub struct AsmError {
    pub location: Option<SourceLocation>,
    pub message: String,
    pub source_line: Option<String>,
    /// Additional context notes, e.g. the chain of macro invocations that led
    /// to the failing line. Rendered under the main diagnostic.
    pub notes: Vec<String>,
}

impl fmt::Display for AsmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error: {}", self.message)?;
        if let Some(loc) = &self.location {
            write!(f, "\n  --> {}:{}", loc.file, loc.line)?;
            if loc.col > 0 {
                write!(f, ":{}", loc.col)?;
            }
        }
        if let Some(line) = &self.source_line {
            write!(f, "\n   |\n   | {}", line)?;
            // Draw a caret under the offending column when we know it.
            if let Some(loc) = &self.location {
                if loc.col > 0 {
                    write!(f, "\n   | {}^", " ".repeat(loc.col - 1))?;
                }
            }
        }
        for note in &self.notes {
            write!(f, "\n   = note: {}", note)?;
        }
        Ok(())
    }
}

impl std::error::Error for AsmError {}

impl AsmError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            location: None,
            message: message.into(),
            source_line: None,
            notes: Vec::new(),
        }
    }

    pub fn with_location(mut self, loc: SourceLocation) -> Self {
        self.location = Some(loc);
        self
    }

    pub fn with_source_line(mut self, line: impl Into<String>) -> Self {
        self.source_line = Some(line.into());
        self
    }

    /// Attach location only if one isn't already set.
    pub fn ensure_location(mut self, file: &str, line: usize) -> Self {
        if self.location.is_none() {
            self.location = Some(SourceLocation {
                file: file.to_string(),
                line,
                col: 0,
            });
        }
        self
    }

    /// Attach context notes only if none are set yet. Called while unwinding
    /// through nested expansions, so the innermost (most specific) chain wins.
    pub fn ensure_notes(mut self, notes: Vec<String>) -> Self {
        if self.notes.is_empty() {
            self.notes = notes;
        }
        self
    }
}

pub type AsmResult<T> = Result<T, AsmError>;
