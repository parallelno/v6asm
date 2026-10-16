//! Minimal, dependency-free ELF32 little-endian relocatable object writer
//! targeting the V6C machine (`EM_V6C = 0x8080`).

use super::section::{RelocTarget, Section};

// ---- ELF identification ----
const EI_NIDENT: usize = 16;
const ELFMAG: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS32: u8 = 1;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;
const ELFOSABI_NONE: u8 = 0;

// ---- ELF header fields ----
const ET_REL: u16 = 1;
const ET_EXEC: u16 = 2;
const EM_V6C: u16 = 0x8080;

// ---- Section header types ----
const SHT_SYMTAB: u32 = 2;
const SHT_STRTAB: u32 = 3;
const SHT_RELA: u32 = 4;

// ---- Special section indices ----
const SHN_UNDEF: u16 = 0;
const SHN_ABS: u16 = 0xfff1;

// ---- Symbol binding / type ----
const STB_LOCAL: u8 = 0;
const STB_GLOBAL: u8 = 1;
const STB_WEAK: u8 = 2;

const STT_NOTYPE: u8 = 0;
const STT_OBJECT: u8 = 1;
const STT_FUNC: u8 = 2;
const STT_SECTION: u8 = 3;

/// Symbol binding as exposed to the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymBinding {
    Local,
    Global,
    Weak,
}

impl SymBinding {
    fn elf(self) -> u8 {
        match self {
            SymBinding::Local => STB_LOCAL,
            SymBinding::Global => STB_GLOBAL,
            SymBinding::Weak => STB_WEAK,
        }
    }
}

/// Symbol type as exposed to the writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymType {
    NoType,
    Object,
    Func,
}

impl SymType {
    fn elf(self) -> u8 {
        match self {
            SymType::NoType => STT_NOTYPE,
            SymType::Object => STT_OBJECT,
            SymType::Func => STT_FUNC,
        }
    }
}

/// Where a symbol lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymLocation {
    /// Defined in a user section at the given offset.
    Section { index: usize, offset: u32 },
    /// An absolute constant value (`SHN_ABS`).
    Absolute(u32),
    /// Undefined external (`SHN_UNDEF`).
    Undefined,
}

/// A symbol to place into `.symtab`.
#[derive(Debug, Clone)]
pub struct ObjSymbol {
    pub name: String,
    pub binding: SymBinding,
    pub kind: SymType,
    pub size: u32,
    pub location: SymLocation,
}

/// A non-allocating section supplied by an object-file producer, such as a
/// DWARF debug section. Relocations may only target user sections.
#[derive(Debug, Clone)]
pub struct ExtraSection {
    pub name: String,
    pub sh_type: u32,
    pub flags: u32,
    pub addralign: u32,
    pub link: u32,
    pub info: u32,
    pub entsize: u32,
    pub data: Vec<u8>,
    pub relocs: Vec<super::section::Reloc>,
}

/// A string table builder.
struct StrTab {
    bytes: Vec<u8>,
}

impl StrTab {
    fn new() -> Self {
        // Index 0 is always the empty string.
        Self { bytes: vec![0] }
    }

    fn add(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            return 0;
        }
        let off = self.bytes.len() as u32;
        self.bytes.extend_from_slice(s.as_bytes());
        self.bytes.push(0);
        off
    }
}

fn align_up(n: usize, align: usize) -> usize {
    if align <= 1 {
        n
    } else {
        (n + align - 1) & !(align - 1)
    }
}

struct OutSection {
    name_off: u32,
    sh_type: u32,
    flags: u32,
    offset: u32,
    size: u32,
    link: u32,
    info: u32,
    addralign: u32,
    entsize: u32,
    data: Vec<u8>,
    is_nobits: bool,
}

/// Serialize the section/symbol model into an ELF32 relocatable object.
///
/// `sections` are the user sections (in order). `symbols` are the named
/// symbols to expose in `.symtab` (section symbols are generated automatically).
pub fn serialize(sections: &[Section], symbols: &[ObjSymbol]) -> Vec<u8> {
    serialize_with_extra(sections, symbols, &[])
}

/// Serialize a fully located executable image. The image is represented by one
/// allocatable `.text` section beginning at `base_address`; debug sections are
/// non-allocating and use absolute addresses in their line program.
pub fn serialize_executable(
    image: &[u8],
    base_address: u16,
    symbols: &[ObjSymbol],
    extra_sections: &[ExtraSection],
) -> Vec<u8> {
    let mut text = Section::new(
        ".text",
        super::section::SHF_ALLOC | super::section::SHF_EXECINSTR,
        super::section::SHT_PROGBITS,
    );
    text.bytes.extend_from_slice(image);
    text.size = image.len() as u32;

    let mut bytes = serialize_with_extra(&[text], symbols, extra_sections);
    bytes[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
    let shoff = u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]) as usize;
    // The first section header after null is the image `.text` section.
    put_u32(&mut bytes, shoff + 40 + 12, base_address as u32);
    bytes
}

/// Serialize user sections plus arbitrary non-allocating metadata sections.
pub fn serialize_with_extra(
    sections: &[Section],
    symbols: &[ObjSymbol],
    extra_sections: &[ExtraSection],
) -> Vec<u8> {
    let n_user = sections.len();
    let n_extra = extra_sections.len();
    let n_primary = n_user + n_extra;

    // Section header index layout:
    //   0                     : null
    //   1 ..= n_user          : user sections
    //   next extra sections   : producer-supplied metadata
    //   next rela sections    : one per relocatable primary section
    //   symtab, strtab, shstrtab
    let mut rela_for: Vec<(String, usize, Vec<super::section::Reloc>)> = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if !section.relocs.is_empty() {
            rela_for.push((section.name.clone(), 1 + index, section.relocs.clone()));
        }
    }
    for (index, section) in extra_sections.iter().enumerate() {
        if !section.relocs.is_empty() {
            rela_for.push((section.name.clone(), 1 + n_user + index, section.relocs.clone()));
        }
    }
    let n_rela = rela_for.len();
    let symtab_idx = 1 + n_primary + n_rela;
    let strtab_idx = symtab_idx + 1;
    let shstrtab_idx = strtab_idx + 1;

    // ---- Build symbol table ----
    let mut strtab = StrTab::new();
    let mut sym_bytes: Vec<u8> = Vec::new();
    // index -> symbol bytes; track name lookup for relocations.
    let mut name_to_symidx: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();

    // Symbol 0: null.
    write_sym(&mut sym_bytes, 0, 0, 0, 0, 0, SHN_UNDEF);

    // Section symbols (local), one per user section, in order. Their symtab
    // index is therefore `1 + i`.
    for i in 0..n_user {
        let info = (STB_LOCAL << 4) | STT_SECTION;
        write_sym(&mut sym_bytes, 0, 0, 0, info, 0, (1 + i) as u16);
    }

    // Extra (metadata) section symbols, one per producer-supplied section
    // (e.g. `.debug_str` / `.debug_line` / `.debug_abbrev`), so relocations in
    // other metadata sections (such as DWARF string offsets in
    // `.rela.debug_info`) can reference them. Symtab indices continue after
    // the user section symbols: `1 + n_user + j`.
    for j in 0..n_extra {
        let info = (STB_LOCAL << 4) | STT_SECTION;
        write_sym(&mut sym_bytes, 0, 0, 0, info, 0, (1 + n_user + j) as u16);
    }

    // First global symbol index = after null + section symbols (all local).
    let first_global = 1 + n_user + n_extra;

    // Partition named symbols: locals first (defined, non-exported), then
    // globals/weaks. We currently only emit globals/weaks and undefined
    // externals as named symbols; keep ordering stable & binding-correct.
    let mut local_named: Vec<&ObjSymbol> = Vec::new();
    let mut global_named: Vec<&ObjSymbol> = Vec::new();
    for s in symbols {
        match s.binding {
            SymBinding::Local => local_named.push(s),
            _ => global_named.push(s),
        }
    }

    let mut next_index = (1 + n_user + n_extra) as u32;
    // Local named symbols would precede globals; adjust first_global if any.
    let first_global = first_global + local_named.len();

    for s in local_named.iter().chain(global_named.iter()) {
        let name_off = strtab.add(&s.name);
        let info = (s.binding.elf() << 4) | s.kind.elf();
        let (value, shndx) = match &s.location {
            SymLocation::Section { index, offset } => (*offset, (1 + *index) as u16),
            SymLocation::Absolute(v) => (*v, SHN_ABS),
            SymLocation::Undefined => (0, SHN_UNDEF),
        };
        write_sym(&mut sym_bytes, name_off, value, s.size, info, 0, shndx);
        name_to_symidx.insert(s.name.clone(), next_index);
        next_index += 1;
    }

    // ---- Build relocation sections ----
    let mut rela_data: Vec<Vec<u8>> = Vec::with_capacity(n_rela);
    for (_, _, relocs) in &rela_for {
        let mut buf = Vec::new();
        for r in relocs {
            let sym_index = match &r.target {
                // Section-relative reference uses that section's section symbol.
                RelocTarget::Section(idx) => (1 + *idx) as u32,
                // Metadata-section reference uses that extra section's symbol.
                RelocTarget::ExtraSection(idx) => (1 + n_user + *idx) as u32,
                RelocTarget::Symbol(name) => *name_to_symidx
                    .get(name)
                    .expect("relocation references an unknown symbol"),
            };
            let r_info = (sym_index << 8) | (r.kind.elf_type() as u32);
            write_u32(&mut buf, r.offset);
            write_u32(&mut buf, r_info);
            write_i32(&mut buf, r.addend as i32);
        }
        rela_data.push(buf);
    }

    // ---- Build shstrtab and the OutSection list ----
    let mut shstrtab = StrTab::new();
    let mut out: Vec<OutSection> = Vec::new();

    // index 0: null section
    out.push(OutSection {
        name_off: 0,
        sh_type: 0,
        flags: 0,
        offset: 0,
        size: 0,
        link: 0,
        info: 0,
        addralign: 0,
        entsize: 0,
        data: Vec::new(),
        is_nobits: false,
    });

    // user sections
    for s in sections {
        let name_off = shstrtab.add(&s.name);
        out.push(OutSection {
            name_off,
            sh_type: s.sh_type,
            flags: s.flags,
            offset: 0,
            size: s.size,
            link: 0,
            info: 0,
            addralign: s.addralign.max(1),
            entsize: 0,
            data: s.bytes.clone(),
            is_nobits: s.is_nobits(),
        });
    }

    // Producer-supplied metadata sections.
    for s in extra_sections {
        let name_off = shstrtab.add(&s.name);
        out.push(OutSection {
            name_off,
            sh_type: s.sh_type,
            flags: s.flags,
            offset: 0,
            size: s.data.len() as u32,
            link: s.link,
            info: s.info,
            addralign: s.addralign.max(1),
            entsize: s.entsize,
            data: s.data.clone(),
            is_nobits: false,
        });
    }

    // rela sections
    for (k, (name, target_index, _)) in rela_for.iter().enumerate() {
        let rela_name = format!(".rela{name}");
        let name_off = shstrtab.add(&rela_name);
        let data = std::mem::take(&mut rela_data[k]);
        out.push(OutSection {
            name_off,
            sh_type: SHT_RELA,
            flags: 0,
            offset: 0,
            size: data.len() as u32,
            link: symtab_idx as u32,
            info: *target_index as u32,
            addralign: 4,
            entsize: 12,
            data,
            is_nobits: false,
        });
    }

    // symtab
    {
        let name_off = shstrtab.add(".symtab");
        out.push(OutSection {
            name_off,
            sh_type: SHT_SYMTAB,
            flags: 0,
            offset: 0,
            size: sym_bytes.len() as u32,
            link: strtab_idx as u32,
            info: first_global as u32,
            addralign: 4,
            entsize: 16,
            data: sym_bytes,
            is_nobits: false,
        });
    }

    // strtab
    {
        let name_off = shstrtab.add(".strtab");
        let data = std::mem::take(&mut strtab.bytes);
        out.push(OutSection {
            name_off,
            sh_type: SHT_STRTAB,
            flags: 0,
            offset: 0,
            size: data.len() as u32,
            link: 0,
            info: 0,
            addralign: 1,
            entsize: 0,
            data,
            is_nobits: false,
        });
    }

    // shstrtab (its own name must be added before we freeze the buffer)
    let shstrtab_name_off = shstrtab.add(".shstrtab");
    let shstrtab_data = std::mem::take(&mut shstrtab.bytes);
    out.push(OutSection {
        name_off: shstrtab_name_off,
        sh_type: SHT_STRTAB,
        flags: 0,
        offset: 0,
        size: shstrtab_data.len() as u32,
        link: 0,
        info: 0,
        addralign: 1,
        entsize: 0,
        data: shstrtab_data,
        is_nobits: false,
    });

    // ---- Lay out file: header, section data, then section header table ----
    let ehdr_size = 52usize;
    let shdr_size = 40usize;

    let mut cursor = ehdr_size;
    for sec in out.iter_mut() {
        if sec.sh_type == 0 || sec.is_nobits {
            // null and NOBITS occupy no file space; offset is set but irrelevant.
            sec.offset = cursor as u32;
            continue;
        }
        let align = sec.addralign.max(1) as usize;
        cursor = align_up(cursor, align);
        sec.offset = cursor as u32;
        cursor += sec.data.len();
    }

    // Section header table aligned to 4.
    cursor = align_up(cursor, 4);
    let shoff = cursor;
    let total = shoff + out.len() * shdr_size;

    let mut buf = vec![0u8; total];

    // ELF header.
    {
        let mut ident = [0u8; EI_NIDENT];
        ident[0..4].copy_from_slice(&ELFMAG);
        ident[4] = ELFCLASS32;
        ident[5] = ELFDATA2LSB;
        ident[6] = EV_CURRENT;
        ident[7] = ELFOSABI_NONE;
        buf[0..EI_NIDENT].copy_from_slice(&ident);
        put_u16(&mut buf, 16, ET_REL);
        put_u16(&mut buf, 18, EM_V6C);
        put_u32(&mut buf, 20, EV_CURRENT as u32);
        put_u32(&mut buf, 24, 0); // e_entry
        put_u32(&mut buf, 28, 0); // e_phoff
        put_u32(&mut buf, 32, shoff as u32); // e_shoff
        put_u32(&mut buf, 36, 0); // e_flags
        put_u16(&mut buf, 40, ehdr_size as u16); // e_ehsize
        put_u16(&mut buf, 42, 0); // e_phentsize
        put_u16(&mut buf, 44, 0); // e_phnum
        put_u16(&mut buf, 46, shdr_size as u16); // e_shentsize
        put_u16(&mut buf, 48, out.len() as u16); // e_shnum
        put_u16(&mut buf, 50, shstrtab_idx as u16); // e_shstrndx
    }

    // Section contents.
    for sec in &out {
        if sec.sh_type == 0 || sec.is_nobits || sec.data.is_empty() {
            continue;
        }
        let off = sec.offset as usize;
        buf[off..off + sec.data.len()].copy_from_slice(&sec.data);
    }

    // Section header table.
    for (i, sec) in out.iter().enumerate() {
        let base = shoff + i * shdr_size;
        put_u32(&mut buf, base, sec.name_off);
        put_u32(&mut buf, base + 4, sec.sh_type);
        put_u32(&mut buf, base + 8, sec.flags);
        put_u32(&mut buf, base + 12, 0); // sh_addr
        put_u32(&mut buf, base + 16, sec.offset);
        put_u32(&mut buf, base + 20, sec.size);
        put_u32(&mut buf, base + 24, sec.link);
        put_u32(&mut buf, base + 28, sec.info);
        put_u32(&mut buf, base + 32, sec.addralign);
        put_u32(&mut buf, base + 36, sec.entsize);
    }

    buf
}

fn write_sym(
    buf: &mut Vec<u8>,
    name: u32,
    value: u32,
    size: u32,
    info: u8,
    other: u8,
    shndx: u16,
) {
    write_u32(buf, name);
    write_u32(buf, value);
    write_u32(buf, size);
    buf.push(info);
    buf.push(other);
    write_u16(buf, shndx);
}

fn write_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_i32(buf: &mut Vec<u8>, v: i32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_u16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
