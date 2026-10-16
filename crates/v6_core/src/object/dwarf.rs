//! Minimal DWARF v4 metadata for relocatable V6C objects and direct-ROM companions.

use std::collections::BTreeMap;
use std::path::Path;

use crate::assembler::DebugLineRow;

use super::elf::ExtraSection;
use super::section::{Reloc, RelocKind, RelocTarget, SHT_PROGBITS};

/// Indices of the debug sections within the `Vec` returned by `debug_sections`.
/// Used as `RelocTarget::ExtraSection` relocation targets.
const DEBUG_ABBREV: usize = 1;
const DEBUG_LINE: usize = 2;
const DEBUG_STR: usize = 3;

/// Source provenance for an immutable absolute assembler constant.
#[derive(Debug, Clone)]
pub struct DebugConstant {
    pub name: String,
    pub file: String,
    pub line: usize,
    pub value: i64,
}

const DW_LNS_COPY: u8 = 1;
const DW_LNS_ADVANCE_LINE: u8 = 3;
const DW_LNS_SET_FILE: u8 = 4;
const DW_LNS_SET_COLUMN: u8 = 5;
const DW_LNE_END_SEQUENCE: u8 = 1;
const DW_LNE_SET_ADDRESS: u8 = 2;

/// Build the small DWARF v4 subset emitted under `v6asm -g -f obj`.
///
/// When `relocate` is true (object mode), every section-relative DWARF
/// reference inside `.debug_info` — each `DW_FORM_strp` string offset, the
/// `DW_AT_stmt_list` line-table offset, and the CU header's `abbrev_offset` —
/// is emitted as a relocation against the corresponding debug section symbol.
/// `ld.lld` then rebases those references while concatenating the inputs'
/// debug sections, so each compilation unit's strings, line program, and
/// abbreviation table stay valid in the merged output. When `relocate` is
/// false (direct-ROM debug companions), raw single-object offsets are kept and
/// no relocations are emitted.
pub fn debug_sections(
    rows: &[DebugLineRow],
    constants: &[DebugConstant],
    compilation_dir: &Path,
    relocate: bool,
) -> Vec<ExtraSection> {
    let mut files: Vec<String> = rows.iter().map(|row| row.file.clone()).collect();
    files.extend(constants.iter().map(|constant| constant.file.clone()));
    files.sort();
    files.dedup();
    let mut directories: Vec<String> = files
        .iter()
        .filter_map(|file| file_directory(file))
        .collect();
    directories.sort();
    directories.dedup();
    let directory_indices: BTreeMap<String, u32> = directories
        .iter()
        .enumerate()
        .map(|(index, directory)| (directory.clone(), index as u32 + 1))
        .collect();
    let file_indices: BTreeMap<String, u32> = files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.clone(), index as u32 + 1))
        .collect();

    let unit_name = files.first().cloned().unwrap_or_default();
    let comp_dir = compilation_dir.to_string_lossy().replace('\\', "/");
    let (debug_str, producer_offset, name_offset, comp_dir_offset, constant_name_offsets) =
        debug_strings(&unit_name, &comp_dir, constants);

    let (debug_info_data, strp_fields, stmt_list_field) = debug_info(
        producer_offset,
        name_offset,
        comp_dir_offset,
        constants,
        &file_indices,
        &constant_name_offsets,
    );
    // The DWARF references below point into *this object's* debug sections.
    // In object mode they are rebased by the linker through relocations
    // against the debug section symbols; in direct-ROM companion mode the
    // single CU's raw offsets are already final, so no relocations are needed.
    let mut debug_info_relocs = Vec::new();
    if relocate {
        for &(field_offset, string_offset) in &strp_fields {
            debug_info_relocs.push(Reloc {
                offset: field_offset,
                kind: RelocKind::Abs32,
                target: RelocTarget::ExtraSection(DEBUG_STR),
                addend: string_offset as i64,
            });
        }
        // DW_AT_stmt_list: offset into this object's .debug_line.
        debug_info_relocs.push(Reloc {
            offset: stmt_list_field,
            kind: RelocKind::Abs32,
            target: RelocTarget::ExtraSection(DEBUG_LINE),
            addend: 0,
        });
        // CU-header `abbrev_offset` field: right after unit_length + version.
        debug_info_relocs.push(Reloc {
            offset: 6,
            kind: RelocKind::Abs32,
            target: RelocTarget::ExtraSection(DEBUG_ABBREV),
            addend: 0,
        });
    }

    vec![
        ExtraSection {
            name: ".debug_info".to_string(),
            sh_type: SHT_PROGBITS,
            flags: 0,
            addralign: 1,
            link: 0,
            info: 0,
            entsize: 0,
            data: debug_info_data,
            relocs: debug_info_relocs,
        },
        ExtraSection {
            name: ".debug_abbrev".to_string(),
            sh_type: SHT_PROGBITS,
            flags: 0,
            addralign: 1,
            link: 0,
            info: 0,
            entsize: 0,
            data: debug_abbrev(),
            relocs: Vec::new(),
        },
        debug_line(rows, &directories, &directory_indices, &files, &file_indices),
        ExtraSection {
            name: ".debug_str".to_string(),
            sh_type: SHT_PROGBITS,
            flags: 0,
            addralign: 1,
            link: 0,
            info: 0,
            entsize: 0,
            data: debug_str,
            relocs: Vec::new(),
        },
    ]
}

fn file_directory(file: &str) -> Option<String> {
    file.rsplit_once('/').and_then(|(directory, _)| {
        (!directory.is_empty()).then(|| directory.to_string())
    })
}

fn file_name(file: &str) -> &str {
    file.rsplit_once('/').map_or(file, |(_, name)| name)
}

fn debug_strings(
    unit_name: &str,
    comp_dir: &str,
    constants: &[DebugConstant],
) -> (Vec<u8>, u32, u32, u32, Vec<u32>) {
    let mut data = Vec::new();
    let producer_offset = push_string(&mut data, "v6asm");
    let name_offset = push_string(&mut data, unit_name);
    let comp_dir_offset = push_string(&mut data, comp_dir);
    let constant_name_offsets = constants
        .iter()
        .map(|constant| push_string(&mut data, &constant.name))
        .collect();
    (data, producer_offset, name_offset, comp_dir_offset, constant_name_offsets)
}

fn push_string(data: &mut Vec<u8>, value: &str) -> u32 {
    let offset = data.len() as u32;
    data.extend_from_slice(value.as_bytes());
    data.push(0);
    offset
}

fn debug_abbrev() -> Vec<u8> {
    let mut data = Vec::new();
    write_uleb(&mut data, 1); // abbreviation code
    write_uleb(&mut data, 0x11); // DW_TAG_compile_unit
    data.push(1); // DW_CHILDREN_yes
    write_uleb(&mut data, 0x25); // DW_AT_producer
    write_uleb(&mut data, 0x0e); // DW_FORM_strp
    write_uleb(&mut data, 0x13); // DW_AT_language
    write_uleb(&mut data, 0x05); // DW_FORM_data2
    write_uleb(&mut data, 0x03); // DW_AT_name
    write_uleb(&mut data, 0x0e); // DW_FORM_strp
    write_uleb(&mut data, 0x1b); // DW_AT_comp_dir
    write_uleb(&mut data, 0x0e); // DW_FORM_strp
    write_uleb(&mut data, 0x10); // DW_AT_stmt_list
    write_uleb(&mut data, 0x17); // DW_FORM_sec_offset
    data.extend_from_slice(&[0, 0]); // attribute/form terminator
    write_uleb(&mut data, 2); // abbreviation code
    write_uleb(&mut data, 0x34); // DW_TAG_variable
    data.push(0); // DW_CHILDREN_no
    write_uleb(&mut data, 0x03); // DW_AT_name
    write_uleb(&mut data, 0x0e); // DW_FORM_strp
    write_uleb(&mut data, 0x3a); // DW_AT_decl_file
    write_uleb(&mut data, 0x0f); // DW_FORM_udata
    write_uleb(&mut data, 0x3b); // DW_AT_decl_line
    write_uleb(&mut data, 0x0f); // DW_FORM_udata
    write_uleb(&mut data, 0x1c); // DW_AT_const_value
    write_uleb(&mut data, 0x0d); // DW_FORM_sdata
    data.extend_from_slice(&[0, 0]); // attribute/form terminator
    data.push(0); // abbreviation table terminator
    data
}

fn debug_info(
    producer_offset: u32,
    name_offset: u32,
    comp_dir_offset: u32,
    constants: &[DebugConstant],
    file_indices: &BTreeMap<String, u32>,
    constant_name_offsets: &[u32],
) -> (Vec<u8>, Vec<(u32, u32)>, u32) {
    let mut data = vec![0; 4];
    data.extend_from_slice(&4u16.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.push(2); // V6C address size
    write_uleb(&mut data, 1);
    let mut strp_fields = Vec::new();
    strp_fields.push((data.len() as u32, producer_offset));
    data.extend_from_slice(&producer_offset.to_le_bytes());
    data.extend_from_slice(&0x8001u16.to_le_bytes()); // DW_LANG_Mips_Assembler
    strp_fields.push((data.len() as u32, name_offset));
    data.extend_from_slice(&name_offset.to_le_bytes());
    strp_fields.push((data.len() as u32, comp_dir_offset));
    data.extend_from_slice(&comp_dir_offset.to_le_bytes());
    let stmt_list_field = data.len() as u32;
    data.extend_from_slice(&0u32.to_le_bytes()); // .debug_line offset
    for (constant, name_offset) in constants.iter().zip(constant_name_offsets) {
        write_uleb(&mut data, 2);
        strp_fields.push((data.len() as u32, *name_offset));
        data.extend_from_slice(&name_offset.to_le_bytes());
        write_uleb(&mut data, file_indices[&constant.file] as u64);
        write_uleb(&mut data, constant.line as u64);
        write_sleb(&mut data, constant.value);
    }
    data.push(0); // End of compilation-unit children.
    write_initial_length(&mut data);
    (data, strp_fields, stmt_list_field)
}

fn debug_line(
    rows: &[DebugLineRow],
    directories: &[String],
    directory_indices: &BTreeMap<String, u32>,
    files: &[String],
    file_indices: &BTreeMap<String, u32>,
) -> ExtraSection {
    let mut data = vec![0; 4];
    data.extend_from_slice(&4u16.to_le_bytes());
    let header_length_offset = data.len();
    data.extend_from_slice(&0u32.to_le_bytes());
    let header_start = data.len();
    data.extend_from_slice(&[1, 1, 1, -5i8 as u8, 14, 13]);
    data.extend_from_slice(&[0, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1]);
    for directory in directories {
        data.extend_from_slice(directory.as_bytes());
        data.push(0);
    }
    data.push(0); // include directories terminator
    for file in files {
        data.extend_from_slice(file_name(file).as_bytes());
        data.push(0);
        let directory = file_directory(file);
        let directory_index = directory
            .as_ref()
            .and_then(|directory| directory_indices.get(directory))
            .copied()
            .unwrap_or(0);
        write_uleb(&mut data, directory_index as u64);
        write_uleb(&mut data, 0); // modification time
        write_uleb(&mut data, 0); // size
    }
    data.push(0); // file table terminator
    let header_length = (data.len() - header_start) as u32;
    data[header_length_offset..header_length_offset + 4].copy_from_slice(&header_length.to_le_bytes());

    let mut relocs = Vec::new();
    let mut active_section = None;
    let mut has_sequence = false;
    let mut current_line = 1i64;
    let mut current_file = 1u32;
    let mut current_column = 0u32;
    let mut statement_rows: Vec<&DebugLineRow> = rows.iter().filter(|row| row.is_stmt).collect();
    statement_rows.sort_by_key(|row| {
        (
            row.section,
            row.offset_or_address,
            &row.file,
            row.line_num,
            row.column,
        )
    });
    for row in statement_rows {
        if !has_sequence || active_section != row.section {
            if has_sequence {
                data.extend_from_slice(&[0, 1, DW_LNE_END_SEQUENCE]);
            }
            active_section = row.section;
            has_sequence = true;
            current_line = 1;
            current_file = 1;
            current_column = 0;
        }

        data.extend_from_slice(&[0, 3, DW_LNE_SET_ADDRESS]);
        let reloc_offset = data.len() as u32;
        data.extend_from_slice(&(row.section.map_or(row.offset_or_address as u16, |_| 0)).to_le_bytes());
        if let Some(section) = row.section {
            relocs.push(Reloc {
                offset: reloc_offset,
                kind: RelocKind::Abs16,
                target: RelocTarget::Section(section),
                addend: row.offset_or_address as i64,
            });
        }

        let file = file_indices[&row.file];
        if current_file != file {
            data.push(DW_LNS_SET_FILE);
            write_uleb(&mut data, file as u64);
            current_file = file;
        }
        let line_delta = row.line_num as i64 - current_line;
        if line_delta != 0 {
            data.push(DW_LNS_ADVANCE_LINE);
            write_sleb(&mut data, line_delta);
            current_line = row.line_num as i64;
        }
        if current_column != row.column {
            data.push(DW_LNS_SET_COLUMN);
            write_uleb(&mut data, row.column as u64);
            current_column = row.column;
        }
        data.push(DW_LNS_COPY);
    }
    if has_sequence {
        data.extend_from_slice(&[0, 1, DW_LNE_END_SEQUENCE]);
    }
    write_initial_length(&mut data);

    ExtraSection {
        name: ".debug_line".to_string(),
        sh_type: SHT_PROGBITS,
        flags: 0,
        addralign: 1,
        link: 0,
        info: 0,
        entsize: 0,
        data,
        relocs,
    }
}

fn write_initial_length(data: &mut [u8]) {
    let length = (data.len() - 4) as u32;
    data[..4].copy_from_slice(&length.to_le_bytes());
}

fn write_uleb(data: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        data.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn write_sleb(data: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        data.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::assembler::{DebugLineRow, EmissionKind};

    use super::{debug_sections, write_sleb, write_uleb, DebugConstant, ExtraSection};
    use super::{DEBUG_ABBREV, DEBUG_LINE, DEBUG_STR};
    use crate::object::section::{RelocKind, RelocTarget};

    fn row(section: Option<usize>, address: u32, file: &str, line: usize, column: u32) -> DebugLineRow {
        DebugLineRow {
            section,
            offset_or_address: address,
            byte_len: 1,
            file: file.to_string(),
            line_num: line,
            column,
            is_stmt: true,
            kind: EmissionKind::Instruction,
            macro_context: None,
            expansion: Vec::new(),
        }
    }

    fn section<'a>(sections: &'a [ExtraSection], name: &str) -> &'a ExtraSection {
        sections.iter().find(|section| section.name == name).unwrap()
    }

    fn initial_length(data: &[u8]) -> usize {
        u32::from_le_bytes(data[..4].try_into().unwrap()) as usize
    }

    #[test]
    fn leb128_encoders_match_known_values() {
        let mut unsigned = Vec::new();
        write_uleb(&mut unsigned, 624_485);
        assert_eq!(unsigned, [0xe5, 0x8e, 0x26]);

        let mut signed = Vec::new();
        write_sleb(&mut signed, -624_485);
        assert_eq!(signed, [0x9b, 0xf1, 0x59]);
    }

    #[test]
    fn debug_sections_encode_deterministic_dwarf_v4_metadata() {
        let rows = vec![
            row(Some(1), 2, "src/main.asm", 9, 6),
            row(Some(0), 1, "lib/util.asm", 4, 0),
        ];
        let sections = debug_sections(&rows, &[], Path::new("C:\\project"), true);
        let reversed_rows: Vec<DebugLineRow> = rows.into_iter().rev().collect();
        let reversed = debug_sections(&reversed_rows, &[], Path::new("C:\\project"), true);

        assert_eq!(sections.len(), 4);
        assert_eq!(
            sections.iter().map(|section| section.name.as_str()).collect::<Vec<_>>(),
            [".debug_info", ".debug_abbrev", ".debug_line", ".debug_str"],
        );
        for name in [".debug_info", ".debug_line"] {
            let section = section(&sections, name);
            assert_eq!(initial_length(&section.data), section.data.len() - 4);
        }
        assert_eq!(&section(&sections, ".debug_info").data[4..11], &[4, 0, 0, 0, 0, 0, 2]);
        assert_eq!(section(&sections, ".debug_abbrev").data.last(), Some(&0));
        assert_eq!(section(&sections, ".debug_str").data, b"v6asm\0lib/util.asm\0C:/project\0");

        let line = section(&sections, ".debug_line");
        assert!(line.data.windows(b"lib\0src\0".len()).any(|window| window == b"lib\0src\0"));
        assert!(line.data.windows(b"util.asm\0\x01\0\0main.asm\0\x02\0\0".len()).any(|window| {
            window == b"util.asm\0\x01\0\0main.asm\0\x02\0\0"
        }));
        assert_eq!(line.relocs.len(), 2);
        let header_length = u32::from_le_bytes(line.data[6..10].try_into().unwrap()) as usize;
        let program = &line.data[10 + header_length..];
        assert_eq!(program.windows(3).filter(|window| *window == [0, 1, 1]).count(), 2);

        for (section, reversed_section) in sections.iter().zip(reversed.iter()) {
            assert_eq!(section.data, reversed_section.data);
            assert_eq!(section.relocs.len(), reversed_section.relocs.len());
        }
    }

    #[test]
    fn direct_rom_line_rows_use_absolute_addresses_without_relocations() {
        let sections = debug_sections(
            &[row(None, 0x1234, "game.asm", 3, 7)],
            &[],
            Path::new("."),
            false,
        );
        let line = section(&sections, ".debug_line");

        assert!(line.relocs.is_empty());
        assert!(line.data.windows(5).any(|window| window == [0, 3, 2, 0x34, 0x12]));
        assert!(line.data.windows(3).any(|window| window == [5, 7, 1]));
    }

    #[test]
    fn debug_sections_include_constant_declarations_and_source_files() {
        let constants = [DebugConstant {
            name: "ARRAY_ADDR".to_string(),
            file: "include/constants.asm".to_string(),
            line: 12,
            value: 0x4000,
        }];
        let sections = debug_sections(&[], &constants, Path::new("."), true);

        assert!(section(&sections, ".debug_str").data.windows(b"ARRAY_ADDR\0".len())
            .any(|window| window == b"ARRAY_ADDR\0"));
        let line = section(&sections, ".debug_line");
        assert!(line.data.windows(b"constants.asm\0\x01\0\0".len())
            .any(|window| window == b"constants.asm\0\x01\0\0"));
        let name_offset = section(&sections, ".debug_str").data
            .windows(b"ARRAY_ADDR\0".len())
            .position(|window| window == b"ARRAY_ADDR\0")
            .unwrap() as u32;
        let mut declaration = vec![2];
        declaration.extend_from_slice(&name_offset.to_le_bytes());
        declaration.extend_from_slice(&[1, 12, 0x80, 0x80, 0x01]);
        let info = &section(&sections, ".debug_info").data;
        assert!(info.windows(declaration.len()).any(|window| window == declaration));
    }

    /// Read a little-endian u32 at `offset` inside `data`.
    fn read_u32(data: &[u8], offset: u32) -> u32 {
        u32::from_le_bytes(data[offset as usize..offset as usize + 4].try_into().unwrap())
    }

    #[test]
    fn object_mode_debug_info_relocates_every_dwarf_section_reference() {
        let constants = [
            DebugConstant {
                name: "FIRST".to_string(),
                file: "first.asm".to_string(),
                line: 1,
                value: 1,
            },
            DebugConstant {
                name: "SECOND".to_string(),
                file: "first.asm".to_string(),
                line: 2,
                value: 2,
            },
        ];
        let sections = debug_sections(&[], &constants, Path::new("C:/proj"), true);
        let str_data = &section(&sections, ".debug_str").data;
        let info = section(&sections, ".debug_info");
        assert_eq!(info.relocs.len(), 3 /* CU strp */ + 2 /* constant strp */ + 1 /* stmt_list */ + 1 /* abbrev_offset */);

        // Every DW_FORM_strp relocation targets this object's .debug_str with
        // the string offset folded into the addend, and the baked field bytes
        // equal the addend (single-object semantics).
        for reloc in &info.relocs {
            if let RelocTarget::ExtraSection(target) = &reloc.target {
                if *target == DEBUG_STR {
                    assert_eq!(reloc.kind, RelocKind::Abs32);
                    let field = read_u32(&info.data, reloc.offset);
                    assert_eq!(field as i64, reloc.addend);
                    let end = str_data[reloc.addend as usize..]
                        .iter()
                        .position(|b| *b == 0)
                        .unwrap()
                        + reloc.addend as usize;
                    assert!(end <= str_data.len());
                }
            }
        }

        // All three CU-header strp fields (producer, name, comp_dir) are
        // covered: they sit at the three 4-byte fields after the 12-byte CU
        // header + abbrev code.
        for expected_offset in [12u32, 18, 22] {
            assert!(
                info.relocs.iter().any(|r| r.offset == expected_offset
                    && r.kind == RelocKind::Abs32
                    && matches!(&r.target, RelocTarget::ExtraSection(t) if *t == DEBUG_STR)),
                "missing .debug_str relocation at offset {expected_offset}",
            );
        }

        // DW_AT_stmt_list points at this object's .debug_line with addend 0.
        assert!(info.relocs.iter().any(|r| r.kind == RelocKind::Abs32
            && matches!(&r.target, RelocTarget::ExtraSection(t) if *t == DEBUG_LINE)
            && r.addend == 0
            && read_u32(&info.data, r.offset) == 0));

        // CU-header abbrev_offset (at offset 6) points at .debug_abbrev.
        assert!(info.relocs.iter().any(|r| r.offset == 6
            && r.kind == RelocKind::Abs32
            && matches!(&r.target, RelocTarget::ExtraSection(t) if *t == DEBUG_ABBREV)
            && r.addend == 0));
    }

    #[test]
    fn rom_companion_debug_info_keeps_raw_offsets_without_relocations() {
        let constants = [DebugConstant {
            name: "FIRST".to_string(),
            file: "first.asm".to_string(),
            line: 1,
            value: 1,
        }];
        let sections = debug_sections(&[], &constants, Path::new("C:/proj"), false);
        assert!(section(&sections, ".debug_info").relocs.is_empty());
        // Raw offsets stay baked in (single-CU companions need no rebasing).
        let info = &section(&sections, ".debug_info").data;
        assert_eq!(&info[12..16], &0u32.to_le_bytes()); // producer = 0
        assert_eq!(u32::from_le_bytes(info[6..10].try_into().unwrap()), 0); // abbrev_offset
    }
}