//! Tests for relocatable ELF object output (`-f obj`).
//!
//! These drive the assembler in object mode and inspect the resulting section
//! / relocation model, plus the serialized ELF32 bytes.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use v6_core::assembler::{Assembler, EmissionKind, OutputFormat};
use v6_core::diagnostics::AsmError;
use v6_core::object::elf::{serialize_with_extra, ExtraSection};
use v6_core::object::section::{RelocKind, RelocTarget, SHT_PROGBITS};
use v6_core::output::{generate_debug_companion, generate_object, generate_rom, ObjConfig, RomConfig};
use v6_core::preprocessor::preprocess;
use v6_core::project::CpuMode;

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A throwaway temp directory that is removed when dropped.
struct TempDir {
    root: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let unique = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("v6asm-obj-tests-{}-{}", nanos, unique));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Assemble `source` in object mode and return the assembler for inspection.
fn assemble_obj(source: &str) -> Result<Assembler, AsmError> {
    let dir = TempDir::new();
    let main_path = dir.root.join("main.asm");
    fs::write(&main_path, source).unwrap();

    let mut asm = Assembler::new(CpuMode::I8080, dir.root.clone());
    asm.quiet = true;
    asm.output_format = OutputFormat::Obj;

    let lines = preprocess(&main_path, &dir.root, &[], &mut asm.symbols, &|path| {
        fs::read_to_string(path).map_err(|err| AsmError::new(err.to_string()))
    })?;
    asm.assemble(&lines)?;
    Ok(asm)
}

fn assemble_rom(source: &str) -> Result<Assembler, AsmError> {
    let dir = TempDir::new();
    let main_path = dir.root.join("main.asm");
    fs::write(&main_path, source).unwrap();

    let mut asm = Assembler::new(CpuMode::I8080, dir.root.clone());
    asm.quiet = true;
    let lines = preprocess(&main_path, &dir.root, &[], &mut asm.symbols, &|path| {
        fs::read_to_string(path).map_err(|err| AsmError::new(err.to_string()))
    })?;
    asm.assemble(&lines)?;
    Ok(asm)
}

/// Find the relocation in section 0 whose fixup offset equals `offset`.
fn reloc_at(asm: &Assembler, offset: u32) -> &v6_core::object::section::Reloc {
    asm.obj.sections[0]
        .relocs
        .iter()
        .find(|r| r.offset == offset)
        .unwrap_or_else(|| panic!("no relocation at offset {:#x}", offset))
}

#[test]
fn debug_rows_track_instructions_but_not_data_directives() {
    let asm = assemble_obj("nop\n.byte 1\nhlt\n").unwrap();
    let instructions: Vec<_> = asm.debug_rows.iter()
        .filter(|row| row.kind == EmissionKind::Instruction)
        .collect();

    assert_eq!(instructions.len(), 2);
    assert_eq!(instructions[0].section, Some(0));
    assert_eq!(instructions[0].offset_or_address, 0);
    assert_eq!(instructions[0].byte_len, 1);
    assert_eq!(instructions[0].line_num, 1);
    assert_eq!(instructions[1].offset_or_address, 2);
    assert_eq!(instructions[1].line_num, 3);
    assert!(asm.debug_rows.iter().any(|row| row.kind == EmissionKind::Data && !row.is_stmt));
}

#[test]
fn debug_rows_preserve_nested_include_paths_and_optional_sections() {
    let dir = TempDir::new();
    fs::create_dir_all(dir.root.join("dir_a")).unwrap();
    fs::create_dir_all(dir.root.join("dir_b")).unwrap();
    let main_path = dir.root.join("main.asm");
    fs::write(
        &main_path,
        ".include \"dir_a/shared.asm\"\n\
         .include \"dir_b/shared.asm\"\n\
         entry:\n\
         call feature\n\
         .optional\n\
         feature:\n\
         nop\n\
         .endoptional\n",
    )
    .unwrap();
    fs::write(dir.root.join("dir_a/shared.asm"), ".include \"nested.asm\"\nnop\n").unwrap();
    fs::write(dir.root.join("dir_a/nested.asm"), "hlt\n").unwrap();
    fs::write(dir.root.join("dir_b/shared.asm"), "nop\n").unwrap();

    let mut asm = Assembler::new(CpuMode::I8080, dir.root.clone());
    asm.quiet = true;
    asm.output_format = OutputFormat::Obj;
    let lines = preprocess(&main_path, &dir.root, &[], &mut asm.symbols, &|path| {
        fs::read_to_string(path).map_err(|err| AsmError::new(err.to_string()))
    })
    .unwrap();
    asm.assemble(&lines).unwrap();

    let files: Vec<_> = asm.debug_rows.iter().map(|row| row.file.as_str()).collect();
    assert!(files.contains(&"dir_a/nested.asm"));
    assert!(files.contains(&"dir_a/shared.asm"));
    assert!(files.contains(&"dir_b/shared.asm"));
    assert!(asm.debug_rows.iter().any(|row| {
        row.file == "main.asm" && row.line_num == 7 && row.section != Some(0)
    }));
    assert!(asm.debug_rows.iter().all(|row| row.is_stmt));
}

#[test]
fn debug_symbols_have_function_and_data_extents() {
    let asm = assemble_obj("entry:\nnop\nnext:\nhlt\ndata:\n.byte 1, 2, 3\n").unwrap();
    let bytes = generate_object(&asm, &ObjConfig { debug: true }).unwrap();
    let e_shoff = read_u32(&bytes, 32) as usize;
    let e_shentsize = read_u16(&bytes, 46) as usize;
    let e_shnum = read_u16(&bytes, 48) as usize;
    let e_shstrndx = read_u16(&bytes, 50) as usize;
    let sh = |index: usize| e_shoff + index * e_shentsize;
    let shstr_off = read_u32(&bytes, sh(e_shstrndx) + 16) as usize;
    let section_name = |index: usize| {
        let start = shstr_off + read_u32(&bytes, sh(index)) as usize;
        let end = start + bytes[start..].iter().position(|&byte| byte == 0).unwrap();
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };
    let names: Vec<String> = (0..e_shnum).map(section_name).collect();
    let symtab_index = names.iter().position(|name| name == ".symtab").unwrap();
    let strtab_index = names.iter().position(|name| name == ".strtab").unwrap();
    let symtab_off = read_u32(&bytes, sh(symtab_index) + 16) as usize;
    let symtab_size = read_u32(&bytes, sh(symtab_index) + 20) as usize;
    let strtab_off = read_u32(&bytes, sh(strtab_index) + 16) as usize;

    let mut symbols = std::collections::HashMap::new();
    for index in 0..symtab_size / 16 {
        let base = symtab_off + index * 16;
        let name_start = strtab_off + read_u32(&bytes, base) as usize;
        let name_end = name_start + bytes[name_start..].iter().position(|&byte| byte == 0).unwrap();
        let name = String::from_utf8_lossy(&bytes[name_start..name_end]).into_owned();
        symbols.insert(name, (read_u32(&bytes, base + 8), bytes[base + 12] & 0x0f));
    }

    assert_eq!(symbols["entry"], (1, 2));
    assert_eq!(symbols["next"], (1, 2));
    assert_eq!(symbols["data"], (3, 1));
}

#[test]
fn debug_elf_exports_immutable_absolute_constants() {
    let object = generate_object(
        &assemble_obj("ARRAY_ADDR = 0x4000\nentry:\nnop\n").unwrap(),
        &ObjConfig { debug: true },
    ).unwrap();
    let companion = generate_debug_companion(
        &assemble_rom("ARRAY_ADDR = 0x4000\nentry:\nnop\n").unwrap(),
        &RomConfig::default(),
    );

    for bytes in [&object, &companion] {
        let e_shoff = read_u32(bytes, 32) as usize;
        let e_shentsize = read_u16(bytes, 46) as usize;
        let e_shnum = read_u16(bytes, 48) as usize;
        let e_shstrndx = read_u16(bytes, 50) as usize;
        let sh = |index: usize| e_shoff + index * e_shentsize;
        let shstr_off = read_u32(bytes, sh(e_shstrndx) + 16) as usize;
        let section_name = |index: usize| {
            let start = shstr_off + read_u32(bytes, sh(index)) as usize;
            let end = start + bytes[start..].iter().position(|&byte| byte == 0).unwrap();
            String::from_utf8_lossy(&bytes[start..end]).into_owned()
        };
        let section_names: Vec<String> = (0..e_shnum).map(section_name).collect();
        let symtab_index = section_names.iter().position(|name| name == ".symtab").unwrap();
        let strtab_index = section_names.iter().position(|name| name == ".strtab").unwrap();
        let symtab_off = read_u32(bytes, sh(symtab_index) + 16) as usize;
        let symtab_size = read_u32(bytes, sh(symtab_index) + 20) as usize;
        let strtab_off = read_u32(bytes, sh(strtab_index) + 16) as usize;

        let constant = (0..symtab_size / 16).find_map(|index| {
            let base = symtab_off + index * 16;
            let name_start = strtab_off + read_u32(bytes, base) as usize;
            let name_end = name_start + bytes[name_start..].iter().position(|&byte| byte == 0).unwrap();
            (String::from_utf8_lossy(&bytes[name_start..name_end]) == "ARRAY_ADDR")
                .then(|| (read_u32(bytes, base + 4), bytes[base + 12], read_u16(bytes, base + 14)))
        }).unwrap();

        assert_eq!(constant, (0x4000, 0, 0xfff1));
    }
}

#[test]
fn extra_sections_preserve_explicit_elf_metadata() {
    let asm = assemble_obj("nop\n").unwrap();
    let bytes = serialize_with_extra(
        &asm.obj.sections,
        &[],
        &[ExtraSection {
            name: ".debug_custom".to_string(),
            sh_type: SHT_PROGBITS,
            flags: 0,
            addralign: 8,
            link: 3,
            info: 4,
            entsize: 6,
            data: vec![1, 2, 3],
            relocs: Vec::new(),
        }],
    );
    let e_shoff = read_u32(&bytes, 32) as usize;
    let e_shentsize = read_u16(&bytes, 46) as usize;
    let e_shnum = read_u16(&bytes, 48) as usize;
    let e_shstrndx = read_u16(&bytes, 50) as usize;
    let sh = |index: usize| e_shoff + index * e_shentsize;
    let shstr_off = read_u32(&bytes, sh(e_shstrndx) + 16) as usize;
    let section_name = |index: usize| {
        let start = shstr_off + read_u32(&bytes, sh(index)) as usize;
        let end = start + bytes[start..].iter().position(|&byte| byte == 0).unwrap();
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };
    let custom = (0..e_shnum)
        .find(|&index| section_name(index) == ".debug_custom")
        .unwrap();

    assert_eq!(read_u32(&bytes, sh(custom) + 24), 3);
    assert_eq!(read_u32(&bytes, sh(custom) + 28), 4);
    assert_eq!(read_u32(&bytes, sh(custom) + 32), 8);
    assert_eq!(read_u32(&bytes, sh(custom) + 36), 6);
}

#[test]
fn debug_rows_keep_macro_definition_and_invocation_provenance() {
    let asm = assemble_obj(".macro pause\nnop\n.endmacro\npause()\n").unwrap();

    assert_eq!(asm.debug_rows.len(), 1);
    let row = &asm.debug_rows[0];
    assert_eq!(row.line_num, 4);
    assert_eq!(row.origin, Some(("main.asm".to_string(), 2)));
    assert_eq!(row.expansion.len(), 1);
    assert_eq!(row.expansion[0].name.as_deref(), Some("pause"));
    assert_eq!(row.expansion[0].definition_line, 1);
    assert_eq!(row.expansion[0].invocation_line, 4);
}

#[test]
fn debug_rows_capture_columns_loop_provenance_and_data_ranges() {
    let asm = assemble_obj(".loop 2\n  nop\n.endloop\n  nop \\ hlt\n.byte 1\n.align 4\n").unwrap();

    let instructions: Vec<_> = asm.debug_rows.iter()
        .filter(|row| row.kind == EmissionKind::Instruction)
        .collect();
    assert_eq!(instructions.len(), 4);
    assert_eq!(instructions[0].column, 3);
    assert_eq!(instructions[0].expansion[0].iteration, Some(0));
    assert_eq!(instructions[1].expansion[0].iteration, Some(1));
    assert_eq!(instructions[2].line_num, 4);
    assert_eq!(instructions[3].line_num, 4);
    assert_eq!(instructions[2].column, 3);
    assert_eq!(instructions[3].column, 9);

    assert!(asm.debug_rows.iter().any(|row| row.kind == EmissionKind::Data && !row.is_stmt));
    assert!(asm.debug_rows.iter().any(|row| row.kind == EmissionKind::Padding && !row.is_stmt));
}

// ── relocation kinds ─────────────────────────────────────────────────────────

#[test]
fn jmp_to_label_emits_abs16() {
    // jmp target -> R_V6C_16 against .text + offset(target)
    let asm = assemble_obj(
        "start:\n\
         jmp target\n\
         target:\n\
         nop\n",
    )
    .unwrap();

    let sec = &asm.obj.sections[0];
    assert_eq!(sec.name, ".text");
    // jmp = opcode at 0, operand at offset 1.
    let r = reloc_at(&asm, 1);
    assert_eq!(r.kind, RelocKind::Abs16);
    assert_eq!(r.target, RelocTarget::Section(0));
    assert_eq!(r.addend, 3); // target follows the 3-byte jmp
    // placeholder operand bytes are zeroed.
    assert_eq!(sec.bytes[1], 0);
    assert_eq!(sec.bytes[2], 0);
}

#[test]
fn call_and_lxi_emit_abs16() {
    let asm = assemble_obj(
        "entry:\n\
         call entry\n\
         lxi h, entry\n",
    )
    .unwrap();

    // call operand at offset 1.
    let c = reloc_at(&asm, 1);
    assert_eq!(c.kind, RelocKind::Abs16);
    assert_eq!(c.target, RelocTarget::Section(0));
    assert_eq!(c.addend, 0);

    // lxi opcode at offset 3, operand at offset 4.
    let l = reloc_at(&asm, 4);
    assert_eq!(l.kind, RelocKind::Abs16);
    assert_eq!(l.target, RelocTarget::Section(0));
    assert_eq!(l.addend, 0);
}

#[test]
fn lo_hi_byte_ops_emit_lo8_hi8() {
    let asm = assemble_obj(
        "sym:\n\
         mvi a, <(sym)\n\
         mvi h, >(sym)\n",
    )
    .unwrap();

    // first mvi operand at offset 1.
    let lo = reloc_at(&asm, 1);
    assert_eq!(lo.kind, RelocKind::Lo8);
    assert_eq!(lo.target, RelocTarget::Section(0));
    assert_eq!(lo.addend, 0);

    // second mvi operand at offset 3.
    let hi = reloc_at(&asm, 3);
    assert_eq!(hi.kind, RelocKind::Hi8);
    assert_eq!(hi.target, RelocTarget::Section(0));
    assert_eq!(hi.addend, 0);
}

#[test]
fn dw_and_shld_self_modifying_addend() {
    // dw label+1 and shld label+1 are the classic self-modifying patterns.
    let asm = assemble_obj(
        "label:\n\
         shld label + 1\n\
         dw label + 1\n",
    )
    .unwrap();

    // shld operand at offset 1.
    let s = reloc_at(&asm, 1);
    assert_eq!(s.kind, RelocKind::Abs16);
    assert_eq!(s.target, RelocTarget::Section(0));
    assert_eq!(s.addend, 1);

    // dw at offset 3 (after the 3-byte shld).
    let d = reloc_at(&asm, 3);
    assert_eq!(d.kind, RelocKind::Abs16);
    assert_eq!(d.target, RelocTarget::Section(0));
    assert_eq!(d.addend, 1);
}

#[test]
fn undefined_external_becomes_symbol_reloc() {
    // A symbol that is never defined is an external reference.
    let asm = assemble_obj("call controls_check\n").unwrap();

    let r = reloc_at(&asm, 1);
    assert_eq!(r.kind, RelocKind::Abs16);
    assert_eq!(r.target, RelocTarget::Symbol("controls_check".to_string()));
    assert_eq!(r.addend, 0);
}

#[test]
fn macro_invocation_line_label_is_defined() {
    // A label may share its line with a macro call. The line is replaced by the
    // expansion, but the label still defines a symbol where the expansion
    // starts — it must not leak out as an undefined external.
    let asm = assemble_obj(
        ".macro EMIT(v)\n\
         db v\n\
         .endmacro\n\
         @tbl:\n\
         dw @h05\n\
         dw @h06\n\
         @h05:\t\tEMIT(0x11)\n\
         @h06:\t\tEMIT(0x22)\n",
    )
    .unwrap();

    let sec = &asm.obj.sections[0];
    assert_eq!(sec.name, ".text");
    // Two words plus the two data bytes emitted by the expansions.
    assert_eq!(sec.bytes.len(), 6);

    // `dw @h05` (offset 0) and `dw @h06` (offset 2) resolve inside .text.
    let first = reloc_at(&asm, 0);
    assert_eq!(first.kind, RelocKind::Abs16);
    assert_eq!(first.target, RelocTarget::Section(0));
    assert_eq!(first.addend, 4); // @h05 follows the two 2-byte words
    let second = reloc_at(&asm, 2);
    assert_eq!(second.kind, RelocKind::Abs16);
    assert_eq!(second.target, RelocTarget::Section(0));
    assert_eq!(second.addend, 5);

    // Nothing references the labels as dropped externals.
    for reloc in &sec.relocs {
        assert_ne!(reloc.target, RelocTarget::Symbol("h05".to_string()));
        assert_ne!(reloc.target, RelocTarget::Symbol("h06".to_string()));
    }
}

#[test]
fn absolute_constant_is_baked_without_reloc() {
    // `= $7331` is a pure constant: emitted directly, no relocation.
    let asm = assemble_obj(
        "value = $7331\n\
         lxi h, value\n",
    )
    .unwrap();

    let sec = &asm.obj.sections[0];
    assert!(sec.relocs.is_empty(), "constant must not produce a reloc");
    // lxi h, $7331 => 0x21 0x31 0x73
    assert_eq!(sec.bytes[0], 0x21);
    assert_eq!(sec.bytes[1], 0x31);
    assert_eq!(sec.bytes[2], 0x73);
}

#[test]
fn immediate_constant_byte_is_baked() {
    let asm = assemble_obj(
        "port = $0c\n\
         mvi a, port\n",
    )
    .unwrap();

    let sec = &asm.obj.sections[0];
    assert!(sec.relocs.is_empty());
    // mvi a, $0c => 0x3E 0x0C
    assert_eq!(sec.bytes[0], 0x3E);
    assert_eq!(sec.bytes[1], 0x0C);
}

// ── sections ─────────────────────────────────────────────────────────────────

#[test]
fn section_directives_create_distinct_sections() {
    let asm = assemble_obj(
        ".section .data\n\
         dw 0\n\
         .bss\n\
         .storage 4\n\
         .text\n\
         nop\n",
    )
    .unwrap();

    let names: Vec<&str> = asm.obj.sections.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&".text"));
    assert!(names.contains(&".data"));
    assert!(names.contains(&".bss"));

    let bss = asm
        .obj
        .sections
        .iter()
        .find(|s| s.name == ".bss")
        .unwrap();
    assert!(bss.is_nobits());
    assert_eq!(bss.size, 4);
    assert!(bss.bytes.is_empty());
}

#[test]
fn cross_section_reference_uses_target_section() {
    // A label defined in .data, referenced from .text.
    let asm = assemble_obj(
        ".section .data\n\
         buffer:\n\
         .storage 2\n\
         .text\n\
         lxi h, buffer\n",
    )
    .unwrap();

    let text_idx = asm
        .obj
        .sections
        .iter()
        .position(|s| s.name == ".text")
        .unwrap();
    let data_idx = asm
        .obj
        .sections
        .iter()
        .position(|s| s.name == ".data")
        .unwrap();

    let text = &asm.obj.sections[text_idx];
    // lxi operand at offset 1 within .text.
    let r = text.relocs.iter().find(|r| r.offset == 1).unwrap();
    assert_eq!(r.kind, RelocKind::Abs16);
    assert_eq!(r.target, RelocTarget::Section(data_idx));
    assert_eq!(r.addend, 0);
}

// ── .optional blocks become sections ─────────────────────────────────────────

#[test]
fn optional_code_block_becomes_text_section() {
    use v6_core::object::section::{SHF_ALLOC, SHF_EXECINSTR, SHT_PROGBITS};

    let asm = assemble_obj(
        "call helper\n\
         .opt\n\
         helper:\n\
         mvi a, 1\n\
         ret\n\
         .endopt\n",
    )
    .unwrap();

    let sec = asm
        .obj
        .sections
        .iter()
        .find(|s| s.name == ".text.helper")
        .expect("expected a .text.helper section");
    assert_eq!(sec.flags, SHF_ALLOC | SHF_EXECINSTR);
    assert_eq!(sec.sh_type, SHT_PROGBITS);
    // mvi a,1 (2 bytes) + ret (1 byte)
    assert_eq!(sec.size, 3);
}

#[test]
fn optional_data_block_becomes_data_section() {
    use v6_core::object::section::{SHF_ALLOC, SHF_WRITE, SHT_PROGBITS};

    let asm = assemble_obj(
        "lxi h, table\n\
         .opt\n\
         table:\n\
         .byte 1, 2, 3\n\
         .endopt\n",
    )
    .unwrap();

    let sec = asm
        .obj
        .sections
        .iter()
        .find(|s| s.name == ".data.table")
        .expect("expected a .data.table section");
    assert_eq!(sec.flags, SHF_ALLOC | SHF_WRITE);
    assert_eq!(sec.sh_type, SHT_PROGBITS);
    assert_eq!(sec.size, 3);
    assert_eq!(sec.bytes, vec![1, 2, 3]);
}

#[test]
fn optional_section_uses_first_referenced_label() {
    // The block defines `internal` first, but only `entry` is referenced from
    // outside, so the section is named after `entry`.
    let asm = assemble_obj(
        "call entry\n\
         .opt\n\
         internal:\n\
         nop\n\
         entry:\n\
         ret\n\
         .endopt\n",
    )
    .unwrap();

    let names: Vec<&str> = asm.obj.sections.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&".text.entry"), "sections: {names:?}");
    assert!(!names.contains(&".text.internal"), "sections: {names:?}");
}

#[test]
fn optional_storage_only_block_becomes_bss_section() {
    use v6_core::object::section::{SHF_ALLOC, SHF_WRITE, SHT_NOBITS};

    // A block that only reserves space with `.storage` (no filler) carries no
    // file bytes and belongs in a `.bss.<label>` (SHT_NOBITS) section.
    let asm = assemble_obj(
        "lxi h, buffer\n\
         .opt\n\
         buffer:\n\
         .storage 8\n\
         .endopt\n",
    )
    .unwrap();

    let sec = asm
        .obj
        .sections
        .iter()
        .find(|s| s.name == ".bss.buffer")
        .expect("expected a .bss.buffer section");
    assert_eq!(sec.flags, SHF_ALLOC | SHF_WRITE);
    assert_eq!(sec.sh_type, SHT_NOBITS);
    assert_eq!(sec.size, 8);
    assert!(sec.bytes.is_empty(), "bss section must store no bytes");
}

#[test]
fn optional_storage_with_filler_stays_in_data_section() {
    use v6_core::object::section::{SHF_ALLOC, SHF_WRITE, SHT_PROGBITS};

    // `.storage N, filler` emits initialized bytes, so it stays in `.data.*`.
    let asm = assemble_obj(
        "lxi h, table\n\
         .opt\n\
         table:\n\
         .storage 3, 0x7E\n\
         .endopt\n",
    )
    .unwrap();

    let sec = asm
        .obj
        .sections
        .iter()
        .find(|s| s.name == ".data.table")
        .expect("expected a .data.table section");
    assert_eq!(sec.flags, SHF_ALLOC | SHF_WRITE);
    assert_eq!(sec.sh_type, SHT_PROGBITS);
    assert_eq!(sec.bytes, vec![0x7E, 0x7E, 0x7E]);
}

#[test]
fn align_before_optional_propagates_to_section() {
    // A `.align` placed just before `.opt` must transfer its alignment to the
    // section the optional block creates, so the linker can honor it.
    let asm = assemble_obj(
        "lxi h, buffer\n\
         .align 256\n\
         .opt\n\
         buffer:\n\
         .storage 8, 0\n\
         .endopt\n",
    )
    .unwrap();

    let sec = asm
        .obj
        .sections
        .iter()
        .find(|s| s.name == ".data.buffer")
        .expect("expected a .data.buffer section");
    assert_eq!(sec.addralign, 256);
}

#[test]
fn align_before_section_propagates_to_section() {
    // The same rule applies to an explicit `.section` switch.
    let asm = assemble_obj(
        ".align 64\n\
         .section .mydata\n\
         .byte 1, 2, 3\n",
    )
    .unwrap();

    let sec = asm
        .obj
        .sections
        .iter()
        .find(|s| s.name == ".mydata")
        .expect("expected a .mydata section");
    assert_eq!(sec.addralign, 64);
}

#[test]
fn align_inside_section_raises_its_alignment() {
    // `.align` within a section raises that section's own alignment.
    let asm = assemble_obj(
        ".section .data\n\
         .byte 1\n\
         .align 16\n\
         .byte 2\n",
    )
    .unwrap();

    let sec = asm
        .obj
        .sections
        .iter()
        .find(|s| s.name == ".data")
        .expect("expected a .data section");
    assert_eq!(sec.addralign, 16);
}

#[test]
fn no_align_leaves_default_alignment() {
    let asm = assemble_obj(
        "lxi h, buffer\n\
         .opt\n\
         buffer:\n\
         .storage 8, 0\n\
         .endopt\n",
    )
    .unwrap();

    let sec = asm
        .obj
        .sections
        .iter()
        .find(|s| s.name == ".data.buffer")
        .expect("expected a .data.buffer section");
    assert_eq!(sec.addralign, 1);
}

#[test]
fn nested_optional_blocks_create_separate_sections() {
    let asm = assemble_obj(
        "call outer\n\
         .opt\n\
         outer:\n\
         call inner\n\
         ret\n\
         .opt\n\
         inner:\n\
         ret\n\
         .endopt\n\
         .endopt\n",
    )
    .unwrap();

    let names: Vec<&str> = asm.obj.sections.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&".text.outer"), "sections: {names:?}");
    assert!(names.contains(&".text.inner"), "sections: {names:?}");
}

#[test]
fn setting_optional_prune_disables_sections_in_obj_mode() {
    // With prune mode, an unreferenced block is dropped and no dedicated
    // section is created.
    let asm = assemble_obj(
        ".setting optional, prune\n\
         .opt\n\
         unused:\n\
         .byte 9\n\
         .endopt\n\
         nop\n",
    )
    .unwrap();

    let names: Vec<&str> = asm.obj.sections.iter().map(|s| s.name.as_str()).collect();
    assert!(
        !names.iter().any(|n| n.starts_with(".data.") || n.starts_with(".text.unused")),
        "expected no dedicated optional section, got: {names:?}"
    );
}

#[test]
fn optional_block_without_symbols_is_an_error() {
    let err = match assemble_obj(
        ".opt\n\
         nop\n\
         .endopt\n",
    ) {
        Ok(_) => panic!("expected an error for a label-less .optional block"),
        Err(e) => e,
    };
    let msg = format!("{}", err).to_lowercase();
    assert!(
        msg.contains("optional"),
        "expected an .optional error, got: {msg}"
    );
}

// ── org rejected in obj mode ─────────────────────────────────────────────────

#[test]
fn org_is_rejected_in_object_mode() {
    let err = match assemble_obj(".org $8000\nnop\n") {
        Ok(_) => panic!("expected .org to be rejected in object mode"),
        Err(e) => e,
    };
    let msg = format!("{}", err);
    assert!(
        msg.to_lowercase().contains("org"),
        "expected an .org error, got: {msg}"
    );
}

// ── .pack blocks ─────────────────────────────────────────────────────────────

#[test]
fn pack_blocks_go_into_bss_pack_nobits_section() {
    use v6_core::object::section::{SHF_ALLOC, SHF_WRITE, SHT_NOBITS};
    // Every logical block is a distinct alignment-1 NOBITS section. The two
    // anchors intentionally have the same visible section name.
    let asm = assemble_obj(
        ".global *\n\
         lxi h, pack_a\n\
         lxi d, pack_b\n\
         lxi b, pack_c\n\
         .pack align\n\
         pack_a:\n\
         .storage 256\n\
         .endpack\n\
         .pack align\n\
         pack_b:\n\
         .storage 256\n\
         .endpack\n\
         .pack\n\
         pack_c:\n\
         .storage 16\n\
         .endpack\n",
    )
    .unwrap();

    let packed: Vec<(usize, _)> = asm
        .obj
        .sections
        .iter()
        .enumerate()
        .filter(|(_, section)| section.name.starts_with(".bss.pack"))
        .collect();
    assert_eq!(packed.len(), 3);
    assert_eq!(packed[0].1.name, ".bss.pack.align");
    assert_eq!(packed[1].1.name, ".bss.pack.align");
    assert_eq!(packed[2].1.name, ".bss.pack");
    assert_eq!(
        packed
            .iter()
            .map(|(_, section)| section.size)
            .collect::<Vec<_>>(),
        vec![256, 256, 16]
    );
    for (_, section) in &packed {
        assert_eq!(section.sh_type, SHT_NOBITS);
        assert_eq!(section.flags, SHF_ALLOC | SHF_WRITE);
        assert!(section.is_nobits());
        assert!(section.bytes.is_empty());
        assert_eq!(section.addralign, 1);
    }

    // Labels are block-relative and owned by distinct section headers.
    let a = asm.symbols.get_global_info("pack_a").unwrap();
    let b = asm.symbols.get_global_info("pack_b").unwrap();
    let c = asm.symbols.get_global_info("pack_c").unwrap();
    assert_eq!(a.section, Some(packed[0].0));
    assert_eq!(b.section, Some(packed[1].0));
    assert_eq!(c.section, Some(packed[2].0));
    assert_eq!(a.value, Some(0));
    assert_eq!(b.value, Some(0));
    assert_eq!(c.value, Some(0));

    // Serialization preserves duplicate visible names as distinct headers,
    // gives each label its block's section index, and keeps one relocation
    // reachability edge to each block.
    let bytes = generate_object(&asm, &ObjConfig::default()).unwrap();
    let e_shoff = read_u32(&bytes, 32) as usize;
    let e_shentsize = read_u16(&bytes, 46) as usize;
    let e_shnum = read_u16(&bytes, 48) as usize;
    let e_shstrndx = read_u16(&bytes, 50) as usize;
    let sh = |index: usize| e_shoff + index * e_shentsize;
    let shstr_off = read_u32(&bytes, sh(e_shstrndx) + 16) as usize;
    let str_at = |base: usize, index: u32| -> String {
        let start = base + index as usize;
        let end = bytes[start..]
            .iter()
            .position(|&c| c == 0)
            .map(|pos| start + pos)
            .unwrap();
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };
    let section_names: Vec<_> = (0..e_shnum)
        .map(|index| str_at(shstr_off, read_u32(&bytes, sh(index))))
        .collect();
    let align_headers: Vec<_> = section_names
        .iter()
        .enumerate()
        .filter(|(_, name)| *name == ".bss.pack.align")
        .map(|(index, _)| index)
        .collect();
    let filler_header = section_names
        .iter()
        .position(|name| name == ".bss.pack")
        .unwrap();
    assert_eq!(align_headers.len(), 2);
    assert_ne!(align_headers[0], align_headers[1]);

    let symtab_i = section_names
        .iter()
        .position(|name| name == ".symtab")
        .unwrap();
    let strtab_i = section_names
        .iter()
        .position(|name| name == ".strtab")
        .unwrap();
    let symtab_off = read_u32(&bytes, sh(symtab_i) + 16) as usize;
    let symtab_size = read_u32(&bytes, sh(symtab_i) + 20) as usize;
    let strtab_off = read_u32(&bytes, sh(strtab_i) + 16) as usize;
    let mut symbol_sections = std::collections::HashMap::new();
    for index in 0..symtab_size / 16 {
        let base = symtab_off + index * 16;
        symbol_sections.insert(
            str_at(strtab_off, read_u32(&bytes, base)),
            read_u16(&bytes, base + 14),
        );
    }
    assert_eq!(symbol_sections["pack_a"] as usize, align_headers[0]);
    assert_eq!(symbol_sections["pack_b"] as usize, align_headers[1]);
    assert_eq!(symbol_sections["pack_c"] as usize, filler_header);

    let rela_text_i = section_names
        .iter()
        .position(|name| name == ".rela.text")
        .unwrap();
    let rela_off = read_u32(&bytes, sh(rela_text_i) + 16) as usize;
    let rela_size = read_u32(&bytes, sh(rela_text_i) + 20) as usize;
    let mut targets = Vec::new();
    for index in 0..rela_size / 12 {
        let r_info = read_u32(&bytes, rela_off + index * 12 + 4);
        let symbol_index = (r_info >> 8) as usize;
        targets.push(
            read_u16(&bytes, symtab_off + symbol_index * 16 + 14) as usize,
        );
    }
    assert_eq!(
        targets,
        vec![align_headers[0], align_headers[1], filler_header]
    );
}

#[test]
fn pack_label_alias_stays_section_relative() {
    let asm = assemble_obj(
        ".global *\n\
         lxi h, alias\n\
         .pack\n\
         base:\n\
         .storage 2\n\
         alias = base + 1\n\
         .endpack\n",
    )
    .unwrap();

    let base = asm.symbols.get_global_info("base").unwrap();
    let alias = asm.symbols.get_global_info("alias").unwrap();
    assert_eq!(base.value, Some(0));
    assert_eq!(alias.value, Some(1));
    assert_eq!(alias.section, base.section);
}

#[test]
fn pack_dataset_obj_matches_producer_contract() {
    // The full runtime dataset emits one linker-packable section per block.
    let asm = assemble_obj(include_str!("fixtures/v6_runtime_data.asm")).unwrap();

    // Only the linker knows post-GC arena metrics in object mode.
    assert!(asm.symbols.get_global_info("__PACK_ARENA_SIZE").is_none());
    assert!(asm.symbols.get_global_info("__PACK_WASTE_BYTES").is_none());

    let packed: Vec<_> = asm
        .obj
        .sections
        .iter()
        .filter(|section| section.name.starts_with(".bss.pack"))
        .collect();
    assert_eq!(packed.len(), 23);
    assert_eq!(
        packed.iter().map(|section| section.size).sum::<u32>(),
        0xCA0
    );
    assert_eq!(
        packed
            .iter()
            .filter(|section| section.name == ".bss.pack.align")
            .count(),
        4
    );
    assert_eq!(
        packed
            .iter()
            .filter(|section| section.name == ".bss.pack.window")
            .count(),
        4
    );
    assert_eq!(
        packed
            .iter()
            .filter(|section| section.name == ".bss.pack")
            .count(),
        15
    );
    assert!(packed.iter().all(|section| section.addralign == 1));

    for name in [
        "containers_inst_data_ptrs",
        "resources_inst_data_ptrs",
        "breakables_status",
        "room_tiledata",
        "room_teleports_data",
        "overlays_runtime_data",
        "hero_resources",
        "rooms_spawn_rate",
        "ram_disk_mode",
    ] {
        assert_eq!(asm.symbols.get_global_info(name).unwrap().value, Some(0));
    }
}

#[test]
fn pack_storage_uses_preceding_constant_inside_conditional() -> Result<(), AsmError> {
    let dir = TempDir::new();
    let main_path = dir.root.join("main.asm");
    fs::write(
        &main_path,
        ".setting force_once, true\n\
         WORD_LEN = 2\n\
                 .pack\n\
                 early_data:\n\
                     .storage 1\n\
                 .endpack\n\
         ENABLE = 1\n\
         .if ENABLE\n\
         .include \"sound.asm\"\n\
         .endif\n",
    )
    .unwrap();
    fs::write(
        dir.root.join("sound.asm"),
        ".include \"constants.asm\"\n\
         .include \"runtime-data.asm\"\n",
    )
    .unwrap();
    fs::write(
        dir.root.join("constants.asm"),
        ".global RAM_DISK_MUSIC\n\
         GC_TASKS = 14\n",
    )
    .unwrap();
    fs::write(
        dir.root.join("runtime-data.asm"),
        ".pack\n\
         V6_GC_TASK_SPS_LEN = GC_TASKS * WORD_LEN\n\
         v6_gc_task_sps:\n\
           .storage V6_GC_TASK_SPS_LEN\n\
         .endpack\n",
    )
    .unwrap();

    let mut asm = Assembler::new(CpuMode::I8080, dir.root.clone());
    asm.quiet = true;
    asm.output_format = OutputFormat::Obj;
    let lines = preprocess(&main_path, &dir.root, &[], &mut asm.symbols, &|path| {
        fs::read_to_string(path).map_err(|err| AsmError::new(err.to_string()))
    })?;
    asm.assemble(&lines)?;

    assert_eq!(asm.symbols.resolve("V6_GC_TASK_SPS_LEN"), Some(28));
    let packed = asm
        .obj
        .sections
        .iter()
        .filter(|section| section.name == ".bss.pack")
        .collect::<Vec<_>>();
    assert_eq!(packed.iter().map(|section| section.size).sum::<u32>(), 29);
    Ok(())
}

// ── ELF serialization ────────────────────────────────────────────────────────

fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[test]
fn elf_header_is_well_formed() {
    let asm = assemble_obj(
        ".globl entry\n\
         entry:\n\
         jmp entry\n",
    )
    .unwrap();
    let bytes = generate_object(&asm, &ObjConfig::default()).unwrap();

    // e_ident magic.
    assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    assert_eq!(bytes[4], 1, "ELFCLASS32");
    assert_eq!(bytes[5], 1, "ELFDATA2LSB");
    assert_eq!(bytes[6], 1, "EI_VERSION");
    assert_eq!(bytes[7], 0, "ELFOSABI_NONE");

    assert_eq!(read_u16(&bytes, 16), 1, "ET_REL");
    assert_eq!(read_u16(&bytes, 18), 0x8080, "EM_V6C");
    assert_eq!(read_u32(&bytes, 20), 1, "EV_CURRENT");
    assert_eq!(read_u32(&bytes, 36), 0, "e_flags");
    assert_eq!(read_u16(&bytes, 40), 52, "e_ehsize");
    assert_eq!(read_u16(&bytes, 46), 40, "e_shentsize");
}

#[test]
fn elf_contains_expected_sections_and_symbols() {
    let asm = assemble_obj(
        ".globl entry\n\
         entry:\n\
         call external\n\
         jmp entry\n",
    )
    .unwrap();
    let bytes = generate_object(&asm, &ObjConfig::default()).unwrap();

    // Parse section header table to collect section names.
    let e_shoff = read_u32(&bytes, 32) as usize;
    let e_shentsize = read_u16(&bytes, 46) as usize;
    let e_shnum = read_u16(&bytes, 48) as usize;
    let e_shstrndx = read_u16(&bytes, 50) as usize;

    // shstrtab location.
    let sh = |i: usize| e_shoff + i * e_shentsize;
    let shstr_off = read_u32(&bytes, sh(e_shstrndx) + 16) as usize;
    let str_at = |base: usize, idx: u32| -> String {
        let start = base + idx as usize;
        let end = bytes[start..]
            .iter()
            .position(|&c| c == 0)
            .map(|p| start + p)
            .unwrap();
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };

    let mut section_names = Vec::new();
    for i in 0..e_shnum {
        let name_idx = read_u32(&bytes, sh(i));
        section_names.push(str_at(shstr_off, name_idx));
    }
    assert!(section_names.iter().any(|n| n == ".text"));
    assert!(section_names.iter().any(|n| n == ".rela.text"));
    assert!(section_names.iter().any(|n| n == ".symtab"));
    assert!(section_names.iter().any(|n| n == ".strtab"));
    assert!(section_names.iter().any(|n| n == ".shstrtab"));

    // Locate .symtab and .strtab.
    let symtab_i = section_names.iter().position(|n| n == ".symtab").unwrap();
    let strtab_i = section_names.iter().position(|n| n == ".strtab").unwrap();
    let symtab_off = read_u32(&bytes, sh(symtab_i) + 16) as usize;
    let symtab_size = read_u32(&bytes, sh(symtab_i) + 20) as usize;
    let strtab_off = read_u32(&bytes, sh(strtab_i) + 16) as usize;

    // Each Elf32_Sym is 16 bytes: name(4) value(4) size(4) info(1) other(1) shndx(2)
    let mut names = Vec::new();
    let mut external_undef = false;
    let mut entry_global_defined = false;
    let count = symtab_size / 16;
    for i in 0..count {
        let base = symtab_off + i * 16;
        let name_idx = read_u32(&bytes, base);
        let info = bytes[base + 12];
        let shndx = read_u16(&bytes, base + 14);
        let bind = info >> 4;
        let name = str_at(strtab_off, name_idx);
        if name == "external" {
            external_undef = bind == 1 /* GLOBAL */ && shndx == 0 /* SHN_UNDEF */;
        }
        if name == "entry" {
            entry_global_defined = bind == 1 && shndx != 0;
        }
        names.push(name);
    }

    assert!(names.iter().any(|n| n == "entry"));
    assert!(names.iter().any(|n| n == "external"));
    assert!(external_undef, "external must be GLOBAL + SHN_UNDEF");
    assert!(entry_global_defined, "entry must be GLOBAL + defined");
}

#[test]
fn debug_object_contains_dwarf_sections_and_line_relocations() {
    let asm = assemble_obj("entry:\nnop\nhlt\n").unwrap();
    let bytes = generate_object(&asm, &ObjConfig { debug: true }).unwrap();
    let e_shoff = read_u32(&bytes, 32) as usize;
    let e_shentsize = read_u16(&bytes, 46) as usize;
    let e_shnum = read_u16(&bytes, 48) as usize;
    let e_shstrndx = read_u16(&bytes, 50) as usize;
    let sh = |index: usize| e_shoff + index * e_shentsize;
    let shstr_off = read_u32(&bytes, sh(e_shstrndx) + 16) as usize;
    let section_name = |index: usize| {
        let start = shstr_off + read_u32(&bytes, sh(index)) as usize;
        let end = start + bytes[start..].iter().position(|&byte| byte == 0).unwrap();
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };

    let names: Vec<String> = (0..e_shnum).map(section_name).collect();
    for name in [".debug_info", ".debug_abbrev", ".debug_line", ".debug_str", ".rela.debug_line"] {
        assert!(names.iter().any(|section| section == name), "missing {name}");
    }
    for name in [".debug_info", ".debug_abbrev", ".debug_line", ".debug_str"] {
        let index = names.iter().position(|section| section == name).unwrap();
        assert_eq!(read_u32(&bytes, sh(index) + 8), 0, "{name} must not be allocatable");
    }
    assert!(asm.debug_rows.iter().all(|row| row.is_stmt));
}

#[test]
fn debug_object_emits_macro_inline_dies_and_low_pc_relocations() {
    let asm = assemble_obj(".macro pause\nnop\nnop\n.endmacro\npause()\nhlt\n").unwrap();
    assert_eq!(asm.debug_rows.len(), 3);
    assert!(asm.debug_rows[2].macro_context.is_none());
    assert!(asm.debug_rows[..2].iter().all(|row| row.macro_context.is_some()));
    assert_eq!(asm.debug_rows[0].origin, Some(("main.asm".to_string(), 2)));
    assert_eq!(asm.debug_rows[1].origin, Some(("main.asm".to_string(), 3)));

    let bytes = generate_object(&asm, &ObjConfig { debug: true }).unwrap();
    let e_shoff = read_u32(&bytes, 32) as usize;
    let e_shentsize = read_u16(&bytes, 46) as usize;
    let e_shnum = read_u16(&bytes, 48) as usize;
    let e_shstrndx = read_u16(&bytes, 50) as usize;
    let sh = |index: usize| e_shoff + index * e_shentsize;
    let shstr_off = read_u32(&bytes, sh(e_shstrndx) + 16) as usize;
    let section_name = |index: usize| {
        let start = shstr_off + read_u32(&bytes, sh(index)) as usize;
        let end = start + bytes[start..].iter().position(|&byte| byte == 0).unwrap();
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };

    let mut info_index = None;
    let mut rela_info_index = None;
    let mut str_index = None;
    for index in 0..e_shnum {
        match section_name(index).as_str() {
            ".debug_info" => info_index = Some(index),
            ".rela.debug_info" => rela_info_index = Some(index),
            ".debug_str" => str_index = Some(index),
            _ => {}
        }
    }
    let rela_index = rela_info_index.expect(".rela.debug_info must exist");

    // The inline DIE low_pc fields produce R_V6C_16 relocations against the
    // .text section symbol (symtab index 1) with the run offset as addend.
    let rela_off = read_u32(&bytes, sh(rela_index) + 16) as usize;
    let rela_size = read_u32(&bytes, sh(rela_index) + 20) as usize;
    let mut abs16_against_text = Vec::new();
    for entry in 0..rela_size / 12 {
        let base = rela_off + entry * 12;
        let r_info = read_u32(&bytes, base + 4);
        if r_info & 0xff == 2 /* R_V6C_16 */ && r_info >> 8 == 1 {
            abs16_against_text.push(i32::from_le_bytes(
                bytes[base + 8..base + 12].try_into().unwrap(),
            ));
        }
    }
    assert_eq!(abs16_against_text, [0]); // both macro rows form one run at offset 0

    // The macro name lands in .debug_str for the abstract subprogram DIE, and
    // the .debug_info contribution grew to hold the inline DIEs.
    let str_off = read_u32(&bytes, sh(str_index.unwrap()) + 16) as usize;
    assert!(bytes[str_off..].windows(b"pause\0".len()).any(|window| window == b"pause\0"));
    let info_off = read_u32(&bytes, sh(info_index.unwrap()) + 16) as usize;
    let info_size = read_u32(&bytes, sh(info_index.unwrap()) + 20) as usize;
    assert!(info_size > 24, "inline DIEs must extend the compilation unit");
    assert!(bytes[info_off..info_off + info_size].len() == info_size);
}

#[test]
fn debug_object_nested_macro_emits_two_inline_runs() {
    let asm = assemble_obj(
        ".macro inner\nnop\n.endmacro\n.macro outer\ninner()\nnop\n.endmacro\nouter()\nhlt\n",
    )
    .unwrap();

    let bytes = generate_object(&asm, &ObjConfig { debug: true }).unwrap();
    let e_shoff = read_u32(&bytes, 32) as usize;
    let e_shentsize = read_u16(&bytes, 46) as usize;
    let e_shnum = read_u16(&bytes, 48) as usize;
    let e_shstrndx = read_u16(&bytes, 50) as usize;
    let sh = |index: usize| e_shoff + index * e_shentsize;
    let shstr_off = read_u32(&bytes, sh(e_shstrndx) + 16) as usize;
    let section_name = |index: usize| {
        let start = shstr_off + read_u32(&bytes, sh(index)) as usize;
        let end = start + bytes[start..].iter().position(|&byte| byte == 0).unwrap();
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };

    let mut rela_index = None;
    for index in 0..e_shnum {
        if section_name(index) == ".rela.debug_info" {
            rela_index = Some(index);
        }
    }
    let rela_index = rela_index.expect(".rela.debug_info must exist");

    // Two contiguous inline runs: INNER's body at offset 0 (nested inside
    // OUTER) and OUTER's own body at offset 1.
    let rela_off = read_u32(&bytes, sh(rela_index) + 16) as usize;
    let rela_size = read_u32(&bytes, sh(rela_index) + 20) as usize;
    let mut abs16_against_text = Vec::new();
    for entry in 0..rela_size / 12 {
        let base = rela_off + entry * 12;
        let r_info = read_u32(&bytes, base + 4);
        if r_info & 0xff == 2 /* R_V6C_16 */ && r_info >> 8 == 1 {
            abs16_against_text.push(i32::from_le_bytes(
                bytes[base + 8..base + 12].try_into().unwrap(),
            ));
        }
    }
    assert_eq!(abs16_against_text, [0, 0, 1]);
}

#[test]
fn rom_debug_companion_is_an_absolute_address_executable() {
    let asm = assemble_rom(".org 0x100\nentry:\nnop\nhlt\n").unwrap();
    let bytes = generate_debug_companion(&asm, &RomConfig::default());
    assert_eq!(read_u16(&bytes, 16), 2, "ET_EXEC");

    let e_shoff = read_u32(&bytes, 32) as usize;
    let e_shentsize = read_u16(&bytes, 46) as usize;
    let e_shnum = read_u16(&bytes, 48) as usize;
    let e_shstrndx = read_u16(&bytes, 50) as usize;
    let sh = |index: usize| e_shoff + index * e_shentsize;
    let shstr_off = read_u32(&bytes, sh(e_shstrndx) + 16) as usize;
    let section_name = |index: usize| {
        let start = shstr_off + read_u32(&bytes, sh(index)) as usize;
        let end = start + bytes[start..].iter().position(|&byte| byte == 0).unwrap();
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };
    let names: Vec<String> = (0..e_shnum).map(section_name).collect();
    let text_index = names.iter().position(|name| name == ".text").unwrap();
    assert_eq!(read_u32(&bytes, sh(text_index) + 12), 0x100);
    let text_offset = read_u32(&bytes, sh(text_index) + 16) as usize;
    let text_size = read_u32(&bytes, sh(text_index) + 20) as usize;
    assert_eq!(&bytes[text_offset..text_offset + text_size], generate_rom(&asm, &RomConfig::default()));
    for name in [".debug_info", ".debug_abbrev", ".debug_line", ".debug_str"] {
        assert!(names.iter().any(|section| section == name), "missing {name}");
    }
    assert!(!names.iter().any(|section| section == ".rela.debug_line"));
}

#[test]
fn elf_rela_entries_match_section_relocs() {
    let asm = assemble_obj(
        "entry:\n\
         call external\n\
         jmp entry\n",
    )
    .unwrap();
    let bytes = generate_object(&asm, &ObjConfig::default()).unwrap();

    let e_shoff = read_u32(&bytes, 32) as usize;
    let e_shentsize = read_u16(&bytes, 46) as usize;
    let e_shnum = read_u16(&bytes, 48) as usize;
    let e_shstrndx = read_u16(&bytes, 50) as usize;
    let sh = |i: usize| e_shoff + i * e_shentsize;
    let shstr_off = read_u32(&bytes, sh(e_shstrndx) + 16) as usize;
    let str_at = |base: usize, idx: u32| -> String {
        let start = base + idx as usize;
        let end = bytes[start..]
            .iter()
            .position(|&c| c == 0)
            .map(|p| start + p)
            .unwrap();
        String::from_utf8_lossy(&bytes[start..end]).into_owned()
    };

    let mut rela_i = None;
    for i in 0..e_shnum {
        let name_idx = read_u32(&bytes, sh(i));
        if str_at(shstr_off, name_idx) == ".rela.text" {
            rela_i = Some(i);
        }
    }
    let rela_i = rela_i.expect(".rela.text must exist");
    let rela_off = read_u32(&bytes, sh(rela_i) + 16) as usize;
    let rela_size = read_u32(&bytes, sh(rela_i) + 20) as usize;
    let rela_entsize = read_u32(&bytes, sh(rela_i) + 36) as usize;
    assert_eq!(rela_entsize, 12, "Elf32_Rela is 12 bytes");

    // Two relocations: call external (offset 1), jmp entry (offset 4).
    let count = rela_size / 12;
    assert_eq!(count, 2);

    let mut offsets = Vec::new();
    for i in 0..count {
        let base = rela_off + i * 12;
        let r_offset = read_u32(&bytes, base);
        let r_info = read_u32(&bytes, base + 4);
        let r_type = r_info & 0xff;
        // R_V6C_16 == 2.
        assert_eq!(r_type, 2, "both relocs are 16-bit absolute");
        offsets.push(r_offset);
    }
    offsets.sort();
    assert_eq!(offsets, vec![1, 4]);
}

// ---- DWARF reference rebasing across a two-object link ----------------------
//
// Regression test for the unrebased-DWARF bug: when `ld.lld` concatenates the
// `.debug_info` / `.debug_str` / `.debug_line` / `.debug_abbrev` sections of
// several objects, every section-relative reference inside them (strp string
// offsets, stmt_list, abbrev_offset) must be rebased. v6asm expresses those
// references as `.rela.debug_info` relocations (R_V6C_32 against the object's
// debug section symbols); this test replays the linker's rebasing by hand and
// asserts that the second object's compilation unit reads correctly afterwards
// — which plain byte-for-byte concatenation would not deliver.

/// A minimal ELF32 view: sections by name plus helpers to locate content.
struct Elf32 {
    bytes: Vec<u8>,
    sections: Vec<(String, usize, usize)>, // name, offset, size
    symtab: Option<(usize, usize)>,        // offset, size
}

impl Elf32 {
    fn parse(bytes: Vec<u8>) -> Self {
        let read_u32 = |off: usize| {
            u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize
        };
        let e_shoff = read_u32(32);
        let e_shnum = u16::from_le_bytes(bytes[48..50].try_into().unwrap()) as usize;
        let e_shstrndx = u16::from_le_bytes(bytes[50..52].try_into().unwrap()) as usize;
        let sh = |i: usize| e_shoff + i * 40;
        let shstr_off = read_u32(sh(e_shstrndx) + 16);
        let name = |i: usize| {
            let start = shstr_off + read_u32(sh(i));
            let end = start + bytes[start..].iter().position(|&b| b == 0).unwrap();
            String::from_utf8_lossy(&bytes[start..end]).into_owned()
        };
        let mut sections = Vec::new();
        let mut symtab = None;
        for i in 0..e_shnum {
            let off = read_u32(sh(i) + 16);
            let size = read_u32(sh(i) + 20);
            let sname = name(i);
            if sname == ".symtab" {
                symtab = Some((off, size));
            }
            sections.push((sname, off, size));
        }
        Self { bytes, sections, symtab }
    }

    fn section(&self, name: &str) -> (usize, usize) {
        self.sections
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|&(_, off, size)| (off, size))
            .unwrap_or_else(|| panic!("missing section {name}"))
    }

    fn u32_at(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.bytes[offset..offset + 4].try_into().unwrap())
    }

    /// Symtab index of the section symbol of section `name`.
    fn section_symbol(&self, name: &str) -> u32 {
        let sec = self
            .sections
            .iter()
            .position(|(n, _, _)| n == name)
            .expect("section exists") as u16;
        let (stab_off, stab_size) = self.symtab.expect("symtab exists");
        for i in 0..stab_size / 16 {
            let shndx = u16::from_le_bytes(
                self.bytes[stab_off + i * 16 + 14..stab_off + i * 16 + 16].try_into().unwrap(),
            );
            if shndx == sec {
                return i as u32;
            }
        }
        panic!("no section symbol for {name}");
    }
}

/// The c-string at `offset` inside `data`.
fn c_str_at(data: &[u8], offset: usize) -> String {
    assert!(offset < data.len(), "offset {offset:#x} out of range");
    let end = offset + data[offset..].iter().position(|&b| b == 0).unwrap();
    String::from_utf8_lossy(&data[offset..end]).into_owned()
}

struct Rela {
    offset: u32,
    sym: u32,
    rtype: u32,
    addend: i32,
}

fn rela_entries(elf: &Elf32) -> Vec<Rela> {
    let (off, size) = elf.section(".rela.debug_info");
    (0..size / 12)
        .map(|i| {
            let base = off + i * 12;
            let info = elf.u32_at(base + 4);
            Rela {
                offset: elf.u32_at(base),
                sym: info >> 8,
                rtype: info & 0xff,
                addend: i32::from_le_bytes(elf.bytes[base + 8..base + 12].try_into().unwrap()),
            }
        })
        .collect()
}

#[test]
fn linked_second_cu_dwarf_references_are_rebased_over_colliding_string_tables() {
    // Two objects assembled in *different* project directories. Object B's
    // string table starts at small offsets that collide with object A's
    // strings, so plain concatenation would leave B's CU pointing into A's
    // table — the bug this test guards against.
    let asm_a = assemble_obj("entry:\nnop\nhlt\n").unwrap();
    let asm_b = assemble_obj("other:\nnop\nhlt\n").unwrap();

    let elf_a = Elf32::parse(generate_object(&asm_a, &ObjConfig { debug: true }).unwrap());
    let elf_b = Elf32::parse(generate_object(&asm_b, &ObjConfig { debug: true }).unwrap());

    let (info_a_off, info_a_size) = elf_a.section(".debug_info");
    let (str_a_off, str_a_size) = elf_a.section(".debug_str");
    let (str_b_off, str_b_size) = elf_b.section(".debug_str");
    let (_abbrev_a_off, abbrev_a_size) = elf_a.section(".debug_abbrev");
    let line_a_len = elf_a.section(".debug_line").1;

    let str_a = elf_a.bytes[str_a_off..str_a_off + str_a_size].to_vec();
    let str_b = elf_b.bytes[str_b_off..str_b_off + str_b_size].to_vec();
    let mut info_b = elf_b.bytes
        [elf_b.section(".debug_info").0..elf_b.section(".debug_info").0 + elf_b.section(".debug_info").1]
        .to_vec();

    // B's raw comp_dir strp (CU header 11 + abbrev code 1 + producer 4 +
    // language 2 + name 4 = offset 22) resolves inside A's table to a string
    // that is NOT B's comp_dir — i.e. the naive link is broken.
    let b_comp_dir_raw = u32::from_le_bytes(info_b[22..26].try_into().unwrap());
    let b_comp_dir = c_str_at(&str_b, b_comp_dir_raw as usize);
    assert_ne!(c_str_at(&str_a, b_comp_dir_raw as usize), b_comp_dir,
        "precondition: B's comp_dir offset must collide with A's table");

    // Replay ld.lld: apply B's .rela.debug_info entries over the merged
    // sections. A section-symbol relocation resolves to the input section's
    // offset inside the merged (non-alloc) output section plus the addend.
    let str_sym_b = elf_b.section_symbol(".debug_str");
    let line_sym_b = elf_b.section_symbol(".debug_line");
    let abbrev_sym_b = elf_b.section_symbol(".debug_abbrev");
    let entries = rela_entries(&elf_b);
    assert!(entries.len() >= 5,
        "all CU strp fields plus stmt_list and abbrev_offset must carry relocations");
    for rel in &entries {
        assert_eq!(rel.rtype, 5, "R_V6C_32");
        let base: i64 = if rel.sym == str_sym_b {
            str_a_size as i64
        } else if rel.sym == line_sym_b {
            line_a_len as i64
        } else {
            assert_eq!(rel.sym, abbrev_sym_b, "unexpected relocation target");
            abbrev_a_size as i64
        };
        let value = (base + rel.addend as i64) as u32;
        info_b[rel.offset as usize..rel.offset as usize + 4].copy_from_slice(&value.to_le_bytes());
    }

    // After rebasing, B's CU comp_dir resolves to B's own comp_dir in the
    // merged string table — and to something different from A's comp_dir.
    let merged_str = [str_a, str_b].concat();
    let patched = u32::from_le_bytes(info_b[22..26].try_into().unwrap()) as usize;
    assert_eq!(c_str_at(&merged_str, patched), b_comp_dir);
    assert_ne!(
        c_str_at(&merged_str, patched),
        c_str_at(&merged_str, u32::from_le_bytes(elf_a.bytes[info_a_off + 22..info_a_off + 26].try_into().unwrap()) as usize),
        "CU#2 comp_dir must not resolve into CU#1's comp_dir",
    );

    // stmt_list points at B's line program inside the merged .debug_line, and
    // abbrev_offset points past A's abbreviation table.
    assert_eq!(
        u32::from_le_bytes(info_b[26..30].try_into().unwrap()),
        line_a_len as u32
    );
    assert_eq!(
        u32::from_le_bytes(info_b[6..10].try_into().unwrap()),
        abbrev_a_size as u32
    );
    let _ = info_a_size;
}
