//! Section and relocation model for relocatable object output.

// ---- ELF section header type constants ----
pub const SHT_PROGBITS: u32 = 1;
pub const SHT_NOBITS: u32 = 8;

// ---- ELF section flag constants ----
pub const SHF_WRITE: u32 = 0x1;
pub const SHF_ALLOC: u32 = 0x2;
pub const SHF_EXECINSTR: u32 = 0x4;

// ---- V6C relocation type constants ----
pub const R_V6C_NONE: u8 = 0;
pub const R_V6C_8: u8 = 1;
pub const R_V6C_16: u8 = 2;
pub const R_V6C_LO8: u8 = 3;
pub const R_V6C_HI8: u8 = 4;
pub const R_V6C_32: u8 = 5;

/// The kind of fixup a relocation applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocKind {
    /// 8-bit absolute value (`R_V6C_8`).
    Abs8,
    /// 16-bit absolute little-endian address (`R_V6C_16`).
    Abs16,
    /// Low byte of a 16-bit address (`R_V6C_LO8`).
    Lo8,
    /// High byte of a 16-bit address (`R_V6C_HI8`).
    Hi8,
    /// 32-bit absolute little-endian value (`R_V6C_32`). Only emitted for
    /// DWARF section-relative references (e.g. `DW_FORM_strp` string offsets
    /// and `DW_FORM_sec_offset` line-table offsets); source-level operands are
    /// 8/16-bit and never produce this kind.
    Abs32,
}

impl RelocKind {
    /// The V6C ELF relocation type value.
    pub fn elf_type(self) -> u8 {
        match self {
            RelocKind::Abs8 => R_V6C_8,
            RelocKind::Abs16 => R_V6C_16,
            RelocKind::Lo8 => R_V6C_LO8,
            RelocKind::Hi8 => R_V6C_HI8,
            RelocKind::Abs32 => R_V6C_32,
        }
    }

    /// Width in bytes of the fixup field.
    pub fn width(self) -> usize {
        match self {
            RelocKind::Abs16 => 2,
            RelocKind::Abs32 => 4,
            _ => 1,
        }
    }
}

/// The target a relocation refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelocTarget {
    /// A reference relative to a section base. The section-relative offset is
    /// folded into the relocation addend (LLVM style).
    Section(usize),
    /// A reference relative to a producer-supplied metadata section (e.g. a
    /// DWARF `.debug_str`/`.debug_line`/`.debug_abbrev` reference from
    /// `.debug_info`). The section-relative offset is folded into the
    /// relocation addend, mirroring `Section`.
    ExtraSection(usize),
    /// A reference to a named symbol (typically an undefined external).
    Symbol(String),
}

/// A single relocation entry within a section.
#[derive(Debug, Clone)]
pub struct Reloc {
    /// Section-relative offset of the fixup field.
    pub offset: u32,
    /// Fixup kind / width.
    pub kind: RelocKind,
    /// What the relocation points at.
    pub target: RelocTarget,
    /// Constant addend (`A` in `S + A`).
    pub addend: i64,
}

/// An output section accumulating bytes and relocations with a
/// section-relative location counter.
#[derive(Debug, Clone)]
pub struct Section {
    /// Section name, e.g. `.text`, `.data`, `.bss`, `.text.foo`.
    pub name: String,
    /// `SHF_*` flags.
    pub flags: u32,
    /// `SHT_PROGBITS` or `SHT_NOBITS`.
    pub sh_type: u32,
    /// Emitted bytes (empty for `SHT_NOBITS`).
    pub bytes: Vec<u8>,
    /// Logical size of the section. For PROGBITS this tracks `bytes.len()`;
    /// for NOBITS (`.bss`) it grows without storing bytes.
    pub size: u32,
    /// Required address alignment (`sh_addralign`); a power of two, at least 1.
    pub addralign: u32,
    /// Relocations applying to this section.
    pub relocs: Vec<Reloc>,
}

impl Section {
    pub fn new(name: impl Into<String>, flags: u32, sh_type: u32) -> Self {
        Self {
            name: name.into(),
            flags,
            sh_type,
            bytes: Vec::new(),
            size: 0,
            addralign: 1,
            relocs: Vec::new(),
        }
    }

    /// Returns true if this section stores no bytes (`.bss`).
    pub fn is_nobits(&self) -> bool {
        self.sh_type == SHT_NOBITS
    }

    /// Append a single byte to a PROGBITS section, advancing size.
    pub fn push_byte(&mut self, b: u8) {
        if !self.is_nobits() {
            self.bytes.push(b);
        }
        self.size += 1;
    }

    /// Reserve `n` bytes of zero/space, advancing size. PROGBITS sections
    /// store zero bytes; NOBITS sections only advance size.
    pub fn reserve(&mut self, n: u32) {
        if !self.is_nobits() {
            self.bytes.resize(self.bytes.len() + n as usize, 0);
        }
        self.size += n;
    }

    /// Record a relocation.
    pub fn add_reloc(&mut self, reloc: Reloc) {
        self.relocs.push(reloc);
    }

    /// Raise the section's required alignment to at least `a` (`a` is a power
    /// of two). Lower or equal requests are ignored.
    pub fn set_align(&mut self, a: u32) {
        if a > self.addralign {
            self.addralign = a;
        }
    }

    /// Default flags for a well-known section name.
    pub fn default_flags(name: &str) -> u32 {
        match name {
            ".text" => SHF_ALLOC | SHF_EXECINSTR,
            ".data" => SHF_ALLOC | SHF_WRITE,
            ".bss" => SHF_ALLOC | SHF_WRITE,
            ".rodata" => SHF_ALLOC,
            n if n.starts_with(".text") => SHF_ALLOC | SHF_EXECINSTR,
            n if n.starts_with(".rodata") => SHF_ALLOC,
            n if n.starts_with(".data") => SHF_ALLOC | SHF_WRITE,
            n if n.starts_with(".bss") => SHF_ALLOC | SHF_WRITE,
            _ => SHF_ALLOC,
        }
    }

    /// Default section header type for a well-known section name.
    pub fn default_type(name: &str) -> u32 {
        if name == ".bss" || name.starts_with(".bss") {
            SHT_NOBITS
        } else {
            SHT_PROGBITS
        }
    }
}
