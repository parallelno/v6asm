//! Console text styling for v6asm diagnostics.
//!
//! All output is emitted through [`anstream`], which automatically strips
//! ANSI escape codes when the stream is not a terminal (pipes, files, CI)
//! and honors the standard `NO_COLOR` / `CLICOLOR_FORCE` conventions.

use anstyle::{AnsiColor, Style};

/// Bold red — severity labels (`error:`, `Error ...`) and error carets.
pub fn error() -> Style {
    AnsiColor::Red.on_default().bold()
}

/// Bold yellow — severity labels (`warning:`).
pub fn warning() -> Style {
    AnsiColor::Yellow.on_default().bold()
}

/// Bold cyan — source locations (`--> file:line:col`).
pub fn location() -> Style {
    AnsiColor::Cyan.on_default().bold()
}

/// Cyan — auxiliary explanations (`= note: ...`).
pub fn note() -> Style {
    AnsiColor::Cyan.on_default()
}

/// Green — success/informational summaries (`ROM: ...`, `Compilation completed`).
pub fn success() -> Style {
    AnsiColor::Green.on_default().bold()
}

/// Dim — secondary details (verbose "written to ..." notifications).
pub fn dim() -> Style {
    Style::new().dimmed()
}

/// Wrap `text` in the ANSI codes of `style`, followed by a reset.
///
/// The escape sequences themselves are emitted through `anstream`, which
/// strips them when the stream is not a terminal.
pub fn paint(style: Style, text: impl std::fmt::Display) -> String {
    format!("{style}{text}{}", style.render_reset())
}
