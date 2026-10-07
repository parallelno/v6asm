use std::collections::HashMap;
use crate::diagnostics::{AsmError, AsmResult};
use crate::expr::Expr;

/// Normalize a symbol name for case-insensitive lookup.
fn ci(name: &str) -> String {
    name.to_uppercase()
}

/// Information about a defined symbol
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub value: Option<i64>,
    pub expr: Option<Expr>,
    pub file: String,
    pub line: usize,
    pub is_mutable: bool,
    pub is_local: bool,
    pub scope_id: usize,
    /// Sequential index for local label disambiguation in debug output
    pub local_index: Option<usize>,
    /// In object mode, the section index this label is defined in (its `value`
    /// is then a section-relative offset). `None` for absolute constants.
    pub section: Option<usize>,
    /// Original (non-normalized) name as written in source
    pub original_name: String,
    /// True when the symbol was defined by a label (`foo:`) rather than a
    /// constant assignment (`foo = expr`). Used by the ELF serialiser to
    /// decide whether to set `STT_FUNC` on executable-section symbols.
    pub is_code_label: bool,
}

/// Information about a macro definition
#[derive(Debug, Clone)]
pub struct MacroDef {
    pub name: String,
    pub params: Vec<MacroParam>,
    pub body: Vec<MacroBodyLine>,
    pub file: String,
    pub line: usize,
}

/// A single line of a macro body together with its definition location.
///
/// Retaining each body line's original file/line lets diagnostics that fire
/// while the macro is being expanded point back at the macro definition
/// instead of the invocation site.
#[derive(Debug, Clone)]
pub struct MacroBodyLine {
    pub text: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct MacroParam {
    pub name: String,
    pub default: Option<String>,
}

/// The symbol table manages labels, constants, variables, and macros
pub struct SymbolTable {
    /// Global symbols (labels, constants, variables)
    globals: HashMap<String, SymbolInfo>,
    /// Local symbols keyed by (name, scope_id)
    locals: HashMap<(String, usize), Vec<SymbolInfo>>,
    /// Macro definitions
    macros: HashMap<String, MacroDef>,
    /// Current scope ID (changes at each global label)
    current_scope: usize,
    /// Current global label name (for scoping)
    current_global_label: Option<String>,
    /// Counter for macro invocations
    macro_call_count: usize,
    /// Track local label indices for debug output
    local_label_counter: usize,
    /// Active macro-expansion scope stack (innermost last). Each entry is the
    /// per-invocation namespace prefix `MacroName_<call-index>` under which
    /// global symbols defined by a macro body are stored.
    macro_scope_stack: Vec<String>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            globals: HashMap::new(),
            locals: HashMap::new(),
            macros: HashMap::new(),
            current_scope: 0,
            current_global_label: None,
            macro_call_count: 0,
            local_label_counter: 0,
            macro_scope_stack: Vec::new(),
        }
    }

    /// Enter a new global label scope
    pub fn enter_scope(&mut self, label: &str) {
        self.current_scope += 1;
        self.current_global_label = Some(label.to_string());
    }

    pub fn current_scope(&self) -> usize {
        self.current_scope
    }

    /// Define a global label at an address
    pub fn define_label(&mut self, name: &str, addr: u16, file: &str, line: usize) -> AsmResult<()> {
        self.define_label_in(name, addr, None, file, line)
    }

    /// Define a global label, optionally recording the section it lives in
    /// (object mode). When `section` is `Some`, `addr` is a section-relative
    /// offset.
    pub fn define_label_in(&mut self, name: &str, addr: u16, section: Option<usize>, file: &str, line: usize) -> AsmResult<()> {
        let info = SymbolInfo {
            value: Some(addr as i64),
            expr: None,
            file: file.to_string(),
            line,
            is_mutable: false,
            is_local: false,
            scope_id: self.current_scope,
            local_index: None,
            section,
            original_name: name.to_string(),
            is_code_label: true,
        };

        let key = ci(name);
        if let Some(existing) = self.globals.get(&key) {
            if !existing.is_mutable && existing.value.is_some() {
                // Allow redef if same value (pass 2 redefining)
                if existing.value != Some(addr as i64) {
                    return Err(AsmError::new(format!("Symbol '{}' already defined", name)));
                }
            }
        }

        self.globals.insert(key, info);
        self.enter_scope(name);
        Ok(())
    }

    /// Define a local label at an address
    pub fn define_local_label(&mut self, name: &str, addr: u16, file: &str, line: usize) -> AsmResult<()> {
        self.define_local_label_in(name, addr, None, file, line)
    }

    /// Define a local label, optionally recording the section it lives in.
    pub fn define_local_label_in(&mut self, name: &str, addr: u16, section: Option<usize>, file: &str, line: usize) -> AsmResult<()> {
        let idx = self.local_label_counter;
        self.local_label_counter += 1;

        let info = SymbolInfo {
            value: Some(addr as i64),
            expr: None,
            file: file.to_string(),
            line,
            is_mutable: false,
            is_local: true,
            scope_id: self.current_scope,
            local_index: Some(idx),
            section,
            original_name: name.to_string(),
            is_code_label: true,
        };

        let key = (ci(name), self.current_scope);
        self.locals.entry(key).or_default().push(info);
        Ok(())
    }

    /// Define a constant (immutable unless previously declared with .var)
    pub fn define_constant(&mut self, name: &str, value: i64, file: &str, line: usize) -> AsmResult<()> {
        let key = ci(name);
        if let Some(existing) = self.globals.get(&key) {
            if !existing.is_mutable && existing.value.is_some() {
                // Allow same-value redefinition (pass 2)
                if existing.value != Some(value) {
                    return Err(AsmError::new(format!("Constant '{}' already defined with different value", name)));
                }
                return Ok(());
            }
        }
        let info = SymbolInfo {
            value: Some(value),
            expr: None,
            file: file.to_string(),
            line,
            is_mutable: false,
            is_local: false,
            scope_id: self.current_scope,
            local_index: None,
            section: None,
            original_name: name.to_string(),
            is_code_label: false,
        };
        self.globals.insert(key, info);
        Ok(())
    }

    /// Define a local constant
    pub fn define_local_constant(&mut self, name: &str, value: i64, file: &str, line: usize) -> AsmResult<()> {
        let idx = self.local_label_counter;
        self.local_label_counter += 1;
        let info = SymbolInfo {
            value: Some(value),
            expr: None,
            file: file.to_string(),
            line,
            is_mutable: false,
            is_local: true,
            scope_id: self.current_scope,
            local_index: Some(idx),
            section: None,
            original_name: name.to_string(),
            is_code_label: false,
        };
        let key = (ci(name), self.current_scope);
        self.locals.entry(key).or_default().push(info);
        Ok(())
    }

    /// Define a mutable variable
    pub fn define_variable(&mut self, name: &str, value: i64, file: &str, line: usize) -> AsmResult<()> {
        let info = SymbolInfo {
            value: Some(value),
            expr: None,
            file: file.to_string(),
            line,
            is_mutable: true,
            is_local: false,
            scope_id: self.current_scope,
            local_index: None,
            section: None,
            original_name: name.to_string(),
            is_code_label: false,
        };
        self.globals.insert(ci(name), info);
        Ok(())
    }

    /// Update a mutable variable or constant defined with .var. Inside a macro
    /// expansion this targets the invocations's own namespace entry.
    pub fn update_variable(&mut self, name: &str, value: i64) -> AsmResult<()> {
        let key = self.effective_global_key(name);
        if let Some(sym) = self.globals.get_mut(&key) {
            if sym.is_mutable {
                sym.value = Some(value);
                return Ok(());
            }
        }
        Err(AsmError::new(format!("Cannot reassign immutable symbol '{}'", name)))
    }

    /// Define a constant that is section-relative (obj mode alias like `foo = bar + 1`).
    /// Like `define_constant` but also records the section affiliation so the symbol
    /// appears as a section-relative (relocatable) entry in the ELF symtab instead of ABS.
    pub fn define_constant_in_section(&mut self, name: &str, value: i64, section: usize, file: &str, line: usize) -> AsmResult<()> {
        let key = ci(name);
        if let Some(existing) = self.globals.get_mut(&key) {
            if !existing.is_mutable && existing.value.is_some() {
                if existing.value != Some(value) {
                    return Err(AsmError::new(format!("Constant '{}' already defined with different value", name)));
                }
                // Same value — upgrade section affiliation recorded from pass 1.
                existing.section = Some(section);
                return Ok(());
            }
        }
        let info = SymbolInfo {
            value: Some(value),
            expr: None,
            file: file.to_string(),
            line,
            is_mutable: false,
            is_local: false,
            scope_id: self.current_scope,
            local_index: None,
            section: Some(section),
            original_name: name.to_string(),
            is_code_label: false,
        };
        self.globals.insert(key, info);
        Ok(())
    }

    /// Set a constant with deferred expression (for first pass forward refs)
    pub fn define_constant_deferred(&mut self, name: &str, expr: Expr, file: &str, line: usize) -> AsmResult<()> {
        let key = ci(name);
        let is_update = if let Some(existing) = self.globals.get(&key) {
            existing.is_mutable
        } else {
            false
        };
        let info = SymbolInfo {
            value: None,
            expr: Some(expr),
            file: file.to_string(),
            line,
            is_mutable: is_update,
            is_local: false,
            scope_id: self.current_scope,
            local_index: None,
            section: None,
            original_name: name.to_string(),
            is_code_label: false,
        };
        self.globals.insert(key, info);
        Ok(())
    }

    /// Resolve a local symbol in a specific scope (used when a label change
    /// has already moved the current scope past where the local was defined).
    pub fn resolve_local_in_scope(&self, name: &str, scope_id: usize) -> Option<i64> {
        let key = (ci(name), scope_id);
        if let Some(entries) = self.locals.get(&key) {
            for entry in entries.iter().rev() {
                if let Some(val) = entry.value {
                    return Some(val);
                }
            }
        }
        None
    }

    /// Resolve a global symbol value. Inside a macro expansion the symbols
    /// defined by the macro body are stored under a per-invocation namespace
    /// (`MacroName_<call-index>.Name`); the innermost matching namespace wins,
    /// then a plain global of the same name. A fully-qualified namespaced name
    /// (`MacroName_<call-index>.Name`) also resolves from anywhere.
    pub fn resolve(&self, name: &str) -> Option<i64> {
        self.globals.get(&self.effective_global_key(name)).and_then(|s| s.value)
    }

    /// Resolve a local symbol in the current scope
    pub fn resolve_local(&self, name: &str) -> Option<i64> {
        let key = (ci(name), self.current_scope);
        if let Some(entries) = self.locals.get(&key) {
            // Return the last defined value in this scope
            for entry in entries.iter().rev() {
                if let Some(val) = entry.value {
                    return Some(val);
                }
            }
        }
        None
    }

    /// Resolve any symbol (local with @ prefix first, then global)
    pub fn resolve_any(&self, name: &str, is_local: bool) -> Option<i64> {
        if is_local {
            self.resolve_local(name)
        } else {
            self.resolve(name)
        }
    }

    /// Look up full symbol info for a global symbol (including any active
    /// macro-expansion namespace).
    pub fn get_global_info(&self, name: &str) -> Option<&SymbolInfo> {
        self.globals.get(&self.effective_global_key(name))
    }

    /// Look up full symbol info for a local symbol in the current scope.
    pub fn get_local_info(&self, name: &str) -> Option<&SymbolInfo> {
        let key = (ci(name), self.current_scope);
        if let Some(entries) = self.locals.get(&key) {
            for entry in entries.iter().rev() {
                if entry.value.is_some() {
                    return Some(entry);
                }
            }
        }
        None
    }

    /// Look up full symbol info for a local symbol in a specific scope.
    pub fn get_local_info_in_scope(&self, name: &str, scope_id: usize) -> Option<&SymbolInfo> {
        let key = (ci(name), scope_id);
        if let Some(entries) = self.locals.get(&key) {
            for entry in entries.iter().rev() {
                if entry.value.is_some() {
                    return Some(entry);
                }
            }
        }
        None
    }

    /// Define a macro
    pub fn define_macro(&mut self, def: MacroDef) -> AsmResult<()> {
        let key = ci(&def.name);
        if self.macros.contains_key(&key) {
            return Err(AsmError::new(format!("Macro '{}' already defined", def.name))
                .ensure_location(&def.file, def.line));
        }
        self.macros.insert(key, def);
        Ok(())
    }

    /// Look up a macro definition
    pub fn get_macro(&self, name: &str) -> Option<&MacroDef> {
        self.macros.get(&ci(name))
    }

    /// Begin a macro expansion scope
    pub fn begin_macro_expansion(&mut self, macro_name: &str) -> String {
        self.macro_call_count += 1;
        let prefix = format!("{}_{}", ci(macro_name), self.macro_call_count);
        self.macro_scope_stack.push(prefix.clone());
        prefix
    }

    /// End a macro expansion scope
    pub fn end_macro_expansion(&mut self) {
        self.macro_scope_stack.pop();
    }

    /// The name under which a *global* symbol written as `name` is stored.
    ///
    /// Inside a macro expansion this is `<MacroName>_<call-index>.<name>` so
    /// that global labels, constants and variables defined by a macro body do
    /// not collide across invocations (or with outer globals). Outside a macro
    /// expansion it is simply `name`.
    pub fn scoped_global_name(&self, name: &str) -> String {
        match self.macro_scope_stack.last() {
            Some(prefix) => format!("{}.{}", prefix, name),
            None => name.to_string(),
        }
    }

    /// The effective key in `globals` for a lookup of `name`: the innermost
    /// active macro namespace that already defines it, else the plain name.
    /// A fully-qualified `MacroName_<call-index>.Name` is found by the
    /// plain-name fallback (`.` cannot occur in a normal identifier).
    fn effective_global_key(&self, name: &str) -> String {
        let upper = ci(name);
        for prefix in self.macro_scope_stack.iter().rev() {
            let key = format!("{}.{}", prefix, upper);
            if self.globals.contains_key(&key) {
                return key;
            }
        }
        upper
    }

    /// Get all global symbols for debug output
    pub fn all_globals(&self) -> &HashMap<String, SymbolInfo> {
        &self.globals
    }

    /// Get all local symbols for debug output
    pub fn all_locals(&self) -> &HashMap<(String, usize), Vec<SymbolInfo>> {
        &self.locals
    }

    /// Get all macro definitions
    pub fn all_macros(&self) -> &HashMap<String, MacroDef> {
        &self.macros
    }

    /// Check if a symbol exists (either a plain global or, inside a macro
    /// expansion, a namespaced entry).
    pub fn exists(&self, name: &str) -> bool {
        self.globals.contains_key(&self.effective_global_key(name))
    }

    pub fn is_mutable(&self, name: &str) -> bool {
        self.globals.get(&self.effective_global_key(name)).map(|info| info.is_mutable).unwrap_or(false)
    }

    /// Reset for pass 2 (keep definitions, reset scope tracking)
    pub fn reset_for_pass2(&mut self) {
        self.current_scope = 0;
        self.current_global_label = None;
        self.local_label_counter = 0;
        // Don't clear macro_scope_stack here
    }

    /// Get the macro call count (for naming)
    pub fn macro_call_count(&self) -> usize {
        self.macro_call_count
    }

    /// Reset macro call count for pass 2
    pub fn reset_macro_call_count(&mut self) {
        self.macro_call_count = 0;
    }
}
