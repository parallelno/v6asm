use std::collections::HashMap;
use crate::diagnostics::{AsmError, AsmResult};
use crate::expr::Expr;

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
}

/// Information about a macro definition
#[derive(Debug, Clone)]
pub struct MacroDef {
    pub name: String,
    pub params: Vec<MacroParam>,
    pub body: Vec<String>,
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
    /// Macro-local symbols: key = "MacroName_<call-idx>.SymbolName"
    macro_locals: HashMap<String, SymbolInfo>,
    /// Active macro scope stack
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
            macro_locals: HashMap::new(),
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
        let info = SymbolInfo {
            value: Some(addr as i64),
            expr: None,
            file: file.to_string(),
            line,
            is_mutable: false,
            is_local: false,
            scope_id: self.current_scope,
            local_index: None,
        };

        if let Some(existing) = self.globals.get(name) {
            if !existing.is_mutable && existing.value.is_some() {
                // Allow redef if same value (pass 2 redefining)
                if existing.value != Some(addr as i64) {
                    return Err(AsmError::new(format!("Symbol '{}' already defined", name)));
                }
            }
        }

        self.globals.insert(name.to_string(), info);
        self.enter_scope(name);
        Ok(())
    }

    /// Define a local label at an address
    pub fn define_local_label(&mut self, name: &str, addr: u16, file: &str, line: usize) -> AsmResult<()> {
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
        };

        let key = (name.to_string(), self.current_scope);
        self.locals.entry(key).or_default().push(info);
        Ok(())
    }

    /// Define a constant (immutable unless previously declared with .var)
    pub fn define_constant(&mut self, name: &str, value: i64, file: &str, line: usize) -> AsmResult<()> {
        if let Some(existing) = self.globals.get(name) {
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
        };
        self.globals.insert(name.to_string(), info);
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
        };
        let key = (name.to_string(), self.current_scope);
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
        };
        self.globals.insert(name.to_string(), info);
        Ok(())
    }

    /// Update a mutable variable or constant defined with .var
    pub fn update_variable(&mut self, name: &str, value: i64) -> AsmResult<()> {
        if let Some(sym) = self.globals.get_mut(name) {
            if sym.is_mutable {
                sym.value = Some(value);
                return Ok(());
            }
        }
        Err(AsmError::new(format!("Cannot reassign immutable symbol '{}'", name)))
    }

    /// Set a constant with deferred expression (for first pass forward refs)
    pub fn define_constant_deferred(&mut self, name: &str, expr: Expr, file: &str, line: usize) -> AsmResult<()> {
        let is_update = if let Some(existing) = self.globals.get(name) {
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
        };
        self.globals.insert(name.to_string(), info);
        Ok(())
    }

    /// Resolve a global symbol value
    pub fn resolve(&self, name: &str) -> Option<i64> {
        // Check macro-local scope first
        for scope_prefix in self.macro_scope_stack.iter().rev() {
            let macro_key = format!("{}.{}", scope_prefix, name);
            if let Some(info) = self.macro_locals.get(&macro_key) {
                return info.value;
            }
        }
        self.globals.get(name).and_then(|s| s.value)
    }

    /// Resolve a local symbol in the current scope
    pub fn resolve_local(&self, name: &str) -> Option<i64> {
        let key = (name.to_string(), self.current_scope);
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

    /// Define a macro
    pub fn define_macro(&mut self, def: MacroDef) -> AsmResult<()> {
        if self.macros.contains_key(&def.name) {
            return Err(AsmError::new(format!("Macro '{}' already defined", def.name)));
        }
        self.macros.insert(def.name.clone(), def);
        Ok(())
    }

    /// Look up a macro definition
    pub fn get_macro(&self, name: &str) -> Option<&MacroDef> {
        self.macros.get(name)
    }

    /// Begin a macro expansion scope
    pub fn begin_macro_expansion(&mut self, macro_name: &str) -> String {
        self.macro_call_count += 1;
        let prefix = format!("{}_{}", macro_name, self.macro_call_count);
        self.macro_scope_stack.push(prefix.clone());
        prefix
    }

    /// End a macro expansion scope
    pub fn end_macro_expansion(&mut self) {
        self.macro_scope_stack.pop();
    }

    /// Define a symbol in the current macro scope
    pub fn define_macro_local(&mut self, scope_prefix: &str, name: &str, value: i64, file: &str, line: usize) {
        let key = format!("{}.{}", scope_prefix, name);
        self.macro_locals.insert(key, SymbolInfo {
            value: Some(value),
            expr: None,
            file: file.to_string(),
            line,
            is_mutable: false,
            is_local: false,
            scope_id: 0,
            local_index: None,
        });
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

    /// Check if a symbol exists (either global or local)
    pub fn exists(&self, name: &str) -> bool {
        self.globals.contains_key(name)
    }

    pub fn is_mutable(&self, name: &str) -> bool {
        self.globals.get(name).map(|info| info.is_mutable).unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_global_label() {
        let mut st = SymbolTable::new();
        st.define_label("start", 0x100, "test.asm", 1).unwrap();
        assert_eq!(st.resolve("start"), Some(0x100));
    }

    #[test]
    fn test_constant() {
        let mut st = SymbolTable::new();
        st.define_constant("MAX", 255, "test.asm", 1).unwrap();
        assert_eq!(st.resolve("MAX"), Some(255));
    }

    #[test]
    fn test_variable() {
        let mut st = SymbolTable::new();
        st.define_variable("counter", 10, "test.asm", 1).unwrap();
        assert_eq!(st.resolve("counter"), Some(10));
        st.update_variable("counter", 9).unwrap();
        assert_eq!(st.resolve("counter"), Some(9));
    }

    #[test]
    fn test_local_label() {
        let mut st = SymbolTable::new();
        st.define_label("start", 0x100, "test.asm", 1).unwrap();
        st.define_local_label("loop", 0x110, "test.asm", 5).unwrap();
        assert_eq!(st.resolve_local("loop"), Some(0x110));
    }

    #[test]
    fn test_local_scope_isolation() {
        let mut st = SymbolTable::new();
        st.define_label("func1", 0x100, "test.asm", 1).unwrap();
        st.define_local_label("loop", 0x110, "test.asm", 5).unwrap();
        st.define_label("func2", 0x200, "test.asm", 10).unwrap();
        // In the new scope, @loop should not be found
        assert_eq!(st.resolve_local("loop"), None);
    }
}
