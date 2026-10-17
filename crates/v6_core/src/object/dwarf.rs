//! Minimal DWARF v4 metadata for relocatable V6C objects and direct-ROM companions.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::assembler::DebugLineRow;
use crate::preprocessor::{ExpansionKind, ExpansionSite};

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

/// A `DebugLineRow` with its line-table attribution resolved.
#[derive(Debug, Clone)]
struct AttributedRow {
    section: Option<usize>,
    offset_or_address: u32,
    file: String,
    line_num: usize,
    column: u32,
    is_stmt: bool,
}

/// One contiguous run of rows emitted by the same macro expansion, used as the
/// address range of a `DW_TAG_inlined_subroutine` DIE.
#[derive(Debug, Clone)]
struct InlineGroup {
    section: Option<usize>,
    start: u32,
    end: u32,
    /// Expansion chain (definition and invocation site per level) shared by
    /// every row of the run.
    chain: Vec<ExpansionSite>,
}

impl InlineGroup {
    fn byte_span(&self) -> u16 {
        (self.end - self.start).min(u16::MAX as u32) as u16
    }
}

/// An abstract `DW_TAG_subprogram` DIE for a macro definition.
#[derive(Debug, Clone)]
struct AbstractMacro {
    name_offset: u32,
    file_index: u32,
    line: usize,
}

/// A concrete inline instance ready for DIE emission.
#[derive(Debug, Clone)]
struct InlineGroupInfo {
    section: Option<usize>,
    start: u32,
    byte_span: u16,
    /// One entry per macro level of the expansion chain, outermost first.
    sites: Vec<SiteRef>,
}

/// A single nesting level of an inline instance.
#[derive(Debug, Clone)]
struct SiteRef {
    /// Index into the abstract macro DIE list.
    macro_index: usize,
    invocation_file: u32,
    invocation_line: usize,
    /// One-based column of the invocation within its physical line.
    invocation_column: usize,
}

/// The innermost macro level of an expansion chain.
fn macro_invocation_site(chain: &[ExpansionSite]) -> Option<&ExpansionSite> {
    chain
        .iter()
        .rev()
        .find(|site| site.kind == ExpansionKind::Macro)
}

/// Re-attribute macro-expanded rows to their macro body lines and collect the
/// contiguous expansion runs used for the inline DIEs.
///
/// Attribution policy (the "Option A" contract between the line table and the
/// `.debug_info` DIEs):
///
/// - Every row of a macro expansion maps to the macro body line it was
///   expanded from (`origin`), so stepping inside an inline frame shows the
///   macro definition.
/// - The first row of an expansion additionally keeps a statement row at the
///   invocation site, so a breakpoint on the macro call line still resolves.
///   For a nested call that site is the parent macro's body line, which is
///   exactly the invocation location recorded in the expansion chain.
///
/// Rows are grouped in emission order: a run continues while rows share the
/// expansion chain, the section, and contiguous addresses. Loop iterations
/// append their own expansion site, so each unrolled iteration becomes its own
/// inline instance.
fn attribute_rows(rows: &[DebugLineRow]) -> (Vec<AttributedRow>, Vec<InlineGroup>) {
    let mut attributed: Vec<AttributedRow> = Vec::new();
    let mut groups: Vec<InlineGroup> = Vec::new();
    let mut entered: HashSet<String> = HashSet::new();
    let mut open_group: Option<usize> = None;

    for row in rows {
        if let Some(context) = &row.macro_context {
            let chain = &row.expansion;
            if chain.iter().any(|site| site.kind == ExpansionKind::Macro) {
                let _group_index = match open_group {
                    Some(index)
                        if groups[index].chain == *chain
                            && groups[index].section == row.section
                            && groups[index].end == row.offset_or_address =>
                    {
                        groups[index].end = row.offset_or_address + row.byte_len;
                        index
                    }
                    _ => {
                        groups.push(InlineGroup {
                            section: row.section,
                            start: row.offset_or_address,
                            end: row.offset_or_address + row.byte_len,
                            chain: chain.clone(),
                        });
                        open_group = Some(groups.len() - 1);
                        groups.len() - 1
                    }
                };
                let (origin_file, origin_line) = row
                    .origin
                    .clone()
                    .unwrap_or_else(|| (row.file.clone(), row.line_num));
                if entered.insert(context.clone()) {
                    // Entry row: keep a statement row at the invocation site so
                    // a breakpoint on the call line resolves to this code. Its
                    // column is the invocation's own position, not the body
                    // statement's.
                    let invocation = macro_invocation_site(chain);
                    let (invocation_file, invocation_line, invocation_column) = invocation
                        .map(|site| {
                            (
                                site.invocation_file.clone(),
                                site.invocation_line,
                                site.invocation_column as u32,
                            )
                        })
                        .unwrap_or_else(|| (row.file.clone(), row.line_num, row.column));
                    attributed.push(AttributedRow {
                        section: row.section,
                        offset_or_address: row.offset_or_address,
                        file: invocation_file,
                        line_num: invocation_line,
                        column: invocation_column,
                        is_stmt: row.is_stmt,
                    });
                }
                attributed.push(AttributedRow {
                    section: row.section,
                    offset_or_address: row.offset_or_address,
                    file: origin_file,
                    line_num: origin_line,
                    column: row.column,
                    is_stmt: row.is_stmt,
                });
                continue;
            }
        }
        open_group = None;
        attributed.push(AttributedRow {
            section: row.section,
            offset_or_address: row.offset_or_address,
            file: row.file.clone(),
            line_num: row.line_num,
            column: row.column,
            is_stmt: row.is_stmt,
        });
    }

    (attributed, groups)
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
/// `DW_AT_stmt_list` line-table offset, the CU header's `abbrev_offset`, and
/// each inline DIE's `DW_AT_low_pc` — is emitted as a relocation against the
/// corresponding debug section symbol or code section. `ld.lld` then rebases
/// those references while concatenating the inputs' debug sections, so each
/// compilation unit's strings, line program, and abbreviation table stay valid
/// in the merged output. When `relocate` is false (direct-ROM debug
/// companions), raw single-object offsets and absolute addresses are kept and
/// no relocations are emitted.
///
/// Macro provenance: rows expanded from a macro body are modeled the way C
/// compilers model inlining. Each invoked macro definition gets an abstract
/// `DW_TAG_subprogram` DIE and each contiguous expansion run gets a nested
/// `DW_TAG_inlined_subroutine` DIE tree with `DW_AT_call_file`/`call_line`,
/// while the line table maps the expanded instructions to the macro body
/// lines (keeping an invocation-site entry row per expansion).
pub fn debug_sections(
    rows: &[DebugLineRow],
    constants: &[DebugConstant],
    compilation_dir: &Path,
    relocate: bool,
) -> Vec<ExtraSection> {
    let (attributed_rows, groups) = attribute_rows(rows);

    let mut files: Vec<String> = attributed_rows
        .iter()
        .map(|row| row.file.clone())
        .collect();
    files.extend(constants.iter().map(|constant| constant.file.clone()));
    for group in &groups {
        for site in group
            .chain
            .iter()
            .filter(|site| site.kind == ExpansionKind::Macro && site.name.is_some())
        {
            files.push(site.definition_file.clone());
            files.push(site.invocation_file.clone());
        }
    }
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

    // Abstract `DW_TAG_subprogram` DIEs: one per macro definition that was
    // actually expanded, keyed by (name, definition file, definition line) so
    // same-named macros in different files stay distinct. The sorted map keeps
    // the DIE order deterministic.
    let mut macro_defs: BTreeMap<(String, String, usize), ()> = BTreeMap::new();
    for group in &groups {
        for site in group
            .chain
            .iter()
            .filter(|site| site.kind == ExpansionKind::Macro && site.name.is_some())
        {
            macro_defs.insert((
                site.name.clone().unwrap(),
                site.definition_file.clone(),
                site.definition_line,
            ), ());
        }
    }
    let macro_list: Vec<(String, String, usize)> = macro_defs.into_keys().collect();
    let macro_indices: BTreeMap<(String, String, usize), usize> = macro_list
        .iter()
        .enumerate()
        .map(|(index, key)| (key.clone(), index))
        .collect();

    let (debug_str, producer_offset, name_offset, comp_dir_offset, constant_name_offsets, macro_name_offsets) =
        debug_strings(&unit_name, &comp_dir, constants, &macro_list);
    let abstract_macros: Vec<AbstractMacro> = macro_list
        .iter()
        .zip(&macro_name_offsets)
        .map(|((_, file, line), name_offset)| AbstractMacro {
            name_offset: *name_offset,
            file_index: file_indices[file],
            line: *line,
        })
        .collect();

    // Concrete inline instances: per contiguous run, one nested
    // `DW_TAG_inlined_subroutine` DIE per macro level of the expansion chain.
    let inline_groups: Vec<InlineGroupInfo> = groups
        .iter()
        .map(|group| InlineGroupInfo {
            section: group.section,
            start: group.start,
            byte_span: group.byte_span(),
            sites: group
                .chain
                .iter()
                .filter(|site| site.kind == ExpansionKind::Macro && site.name.is_some())
                .map(|site| SiteRef {
                    macro_index: macro_indices[&(
                        site.name.clone().unwrap(),
                        site.definition_file.clone(),
                        site.definition_line,
                    )],
                    invocation_file: file_indices[&site.invocation_file],
                    invocation_line: site.invocation_line,
                    invocation_column: site.invocation_column,
                })
                .collect(),
        })
        .collect();

    let debug_info_output = debug_info(
        CuStrings {
            producer: producer_offset,
            name: name_offset,
            comp_dir: comp_dir_offset,
        },
        constants,
        &file_indices,
        &constant_name_offsets,
        &abstract_macros,
        &inline_groups,
    );
    // The DWARF references below point into *this object's* debug sections.
    // In object mode they are rebased by the linker through relocations
    // against the debug section symbols; in direct-ROM companion mode the
    // single CU's raw offsets are already final, so no relocations are needed.
    let mut debug_info_relocs = Vec::new();
    if relocate {
        for &(field_offset, string_offset) in &debug_info_output.strp_fields {
            debug_info_relocs.push(Reloc {
                offset: field_offset,
                kind: RelocKind::Abs32,
                target: RelocTarget::ExtraSection(DEBUG_STR),
                addend: string_offset as i64,
            });
        }
        // DW_AT_stmt_list: offset into this object's .debug_line.
        debug_info_relocs.push(Reloc {
            offset: debug_info_output.stmt_list_field,
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
        // Inline DIE `DW_AT_low_pc` fields: section-relative code addresses,
        // rebased by LLD exactly like the line-table row addresses.
        debug_info_relocs.extend(debug_info_output.pc_relocs);
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
            data: debug_info_output.data,
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
        debug_line(
            &attributed_rows,
            &directories,
            &directory_indices,
            &files,
            &file_indices,
        ),
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
    macros: &[(String, String, usize)],
) -> (Vec<u8>, u32, u32, u32, Vec<u32>, Vec<u32>) {
    let mut data = Vec::new();
    let producer_offset = push_string(&mut data, "v6asm");
    let name_offset = push_string(&mut data, unit_name);
    let comp_dir_offset = push_string(&mut data, comp_dir);
    let constant_name_offsets = constants
        .iter()
        .map(|constant| push_string(&mut data, &constant.name))
        .collect();
    let macro_name_offsets = macros
        .iter()
        .map(|(name, _, _)| push_string(&mut data, name))
        .collect();
    (
        data,
        producer_offset,
        name_offset,
        comp_dir_offset,
        constant_name_offsets,
        macro_name_offsets,
    )
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
    write_uleb(&mut data, 3); // abbreviation code
    write_uleb(&mut data, 0x2e); // DW_TAG_subprogram (abstract macro definition)
    data.push(0); // DW_CHILDREN_no
    write_uleb(&mut data, 0x03); // DW_AT_name
    write_uleb(&mut data, 0x0e); // DW_FORM_strp
    write_uleb(&mut data, 0x3a); // DW_AT_decl_file
    write_uleb(&mut data, 0x0f); // DW_FORM_udata
    write_uleb(&mut data, 0x3b); // DW_AT_decl_line
    write_uleb(&mut data, 0x0f); // DW_FORM_udata
    data.extend_from_slice(&[0, 0]); // attribute/form terminator
    write_uleb(&mut data, 4); // abbreviation code
    write_uleb(&mut data, 0x1d); // DW_TAG_inlined_subroutine
    data.push(1); // DW_CHILDREN_yes (nested macro invocations)
    write_uleb(&mut data, 0x31); // DW_AT_abstract_origin
    write_uleb(&mut data, 0x13); // DW_FORM_ref4
    write_uleb(&mut data, 0x11); // DW_AT_low_pc
    write_uleb(&mut data, 0x01); // DW_FORM_addr
    write_uleb(&mut data, 0x12); // DW_AT_high_pc
    write_uleb(&mut data, 0x05); // DW_FORM_data2 (offset from low_pc)
    write_uleb(&mut data, 0x58); // DW_AT_call_file
    write_uleb(&mut data, 0x0f); // DW_FORM_udata
    write_uleb(&mut data, 0x59); // DW_AT_call_line
    write_uleb(&mut data, 0x0f); // DW_FORM_udata
    write_uleb(&mut data, 0x5a); // DW_AT_call_column
    write_uleb(&mut data, 0x0f); // DW_FORM_udata
    data.extend_from_slice(&[0, 0]); // attribute/form terminator
    data.push(0); // abbreviation table terminator
    data
}

/// Offsets of the three compilation-unit strings inside `.debug_str`.
struct CuStrings {
    producer: u32,
    name: u32,
    comp_dir: u32,
}

/// The `.debug_info` byte stream plus the fields that need relocations.
struct DebugInfoOutput {
    data: Vec<u8>,
    /// `(field offset, .debug_str offset)` for every `DW_FORM_strp` field.
    strp_fields: Vec<(u32, u32)>,
    /// Offset of the CU's `DW_AT_stmt_list` field.
    stmt_list_field: u32,
    /// Relocations for inline DIE `DW_AT_low_pc` fields.
    pc_relocs: Vec<Reloc>,
}

fn debug_info(
    cu_strings: CuStrings,
    constants: &[DebugConstant],
    file_indices: &BTreeMap<String, u32>,
    constant_name_offsets: &[u32],
    abstract_macros: &[AbstractMacro],
    inline_groups: &[InlineGroupInfo],
) -> DebugInfoOutput {
    let mut data = vec![0; 4];
    data.extend_from_slice(&4u16.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.push(2); // V6C address size
    write_uleb(&mut data, 1);
    let mut strp_fields = Vec::new();
    strp_fields.push((data.len() as u32, cu_strings.producer));
    data.extend_from_slice(&cu_strings.producer.to_le_bytes());
    data.extend_from_slice(&0x8001u16.to_le_bytes()); // DW_LANG_Mips_Assembler
    strp_fields.push((data.len() as u32, cu_strings.name));
    data.extend_from_slice(&cu_strings.name.to_le_bytes());
    strp_fields.push((data.len() as u32, cu_strings.comp_dir));
    data.extend_from_slice(&cu_strings.comp_dir.to_le_bytes());
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

    // Abstract `DW_TAG_subprogram` DIEs, one per invoked macro definition.
    // They are emitted before the inline DIEs so their CU-relative offsets are
    // known when `DW_AT_abstract_origin` (`DW_FORM_ref4`) references them.
    let mut abstract_die_offsets = Vec::with_capacity(abstract_macros.len());
    for macro_def in abstract_macros {
        abstract_die_offsets.push(data.len() as u32);
        write_uleb(&mut data, 3);
        strp_fields.push((data.len() as u32, macro_def.name_offset));
        data.extend_from_slice(&macro_def.name_offset.to_le_bytes());
        write_uleb(&mut data, macro_def.file_index as u64);
        write_uleb(&mut data, macro_def.line as u64);
    }

    // `DW_TAG_inlined_subroutine` DIE trees, one per contiguous expansion run,
    // nested along the macro invocation chain. Every level covers the run's
    // address range; the innermost level is the macro whose body produced the
    // rows, the outer levels record the surrounding invocation chain.
    let mut pc_relocs = Vec::new();
    for group in inline_groups {
        let mut levels = 0usize;
        for site in &group.sites {
            levels += 1;
            write_uleb(&mut data, 4);
            data.extend_from_slice(&abstract_die_offsets[site.macro_index].to_le_bytes());
            let low_pc_offset = data.len() as u32;
            data.extend_from_slice(
                &(group.section.map_or(group.start as u16, |_| 0)).to_le_bytes(),
            );
            if let Some(section) = group.section {
                pc_relocs.push(Reloc {
                    offset: low_pc_offset,
                    kind: RelocKind::Abs16,
                    target: RelocTarget::Section(section),
                    addend: group.start as i64,
                });
            }
            data.extend_from_slice(&group.byte_span.to_le_bytes());
            write_uleb(&mut data, site.invocation_file as u64);
            write_uleb(&mut data, site.invocation_line as u64);
            write_uleb(&mut data, site.invocation_column as u64);
        }
        // One children terminator per open inline DIE (innermost first).
        data.resize(data.len() + levels, 0);
    }

    data.push(0); // End of compilation-unit children.
    write_initial_length(&mut data);
    DebugInfoOutput {
        data,
        strp_fields,
        stmt_list_field,
        pc_relocs,
    }
}

fn debug_line(
    rows: &[AttributedRow],
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
    // Sorted by address; the stable sort keeps emission order, which matters
    // when an invocation row and its macro body row share the entry address
    // (the invocation row is constructed first, matching compiler output).
    let mut statement_rows: Vec<&AttributedRow> = rows.iter().filter(|row| row.is_stmt).collect();
    statement_rows.sort_by_key(|row| (row.section, row.offset_or_address));
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
    use std::collections::BTreeMap;
    use std::path::Path;

    use crate::assembler::{DebugLineRow, EmissionKind};
    use crate::preprocessor::{ExpansionKind, ExpansionSite};

    use super::{attribute_rows, debug_sections, write_sleb, write_uleb, DebugConstant, ExtraSection};
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
            origin: None,
        }
    }

    fn section<'a>(sections: &'a [ExtraSection], name: &str) -> &'a ExtraSection {
        sections.iter().find(|section| section.name == name).unwrap()
    }

    fn initial_length(data: &[u8]) -> usize {
        u32::from_le_bytes(data[..4].try_into().unwrap()) as usize
    }

    // ---- macro provenance helpers and minimal DWARF parsers (test-only) ----

    fn macro_row(
        section: Option<usize>,
        offset: u32,
        byte_len: u32,
        file: &str,
        line: usize,
        origin: (&str, usize),
        context: &str,
        chain: Vec<ExpansionSite>,
    ) -> DebugLineRow {
        DebugLineRow {
            section,
            offset_or_address: offset,
            byte_len,
            file: file.to_string(),
            line_num: line,
            column: 0,
            is_stmt: true,
            kind: EmissionKind::Instruction,
            macro_context: Some(context.to_string()),
            expansion: chain,
            origin: Some((origin.0.to_string(), origin.1)),
        }
    }

    fn macro_site(
        name: &str,
        def_file: &str,
        def_line: usize,
        inv_file: &str,
        inv_line: usize,
    ) -> ExpansionSite {
        macro_site_with_column(name, def_file, def_line, inv_file, inv_line, 1)
    }

    fn macro_site_with_column(
        name: &str,
        def_file: &str,
        def_line: usize,
        inv_file: &str,
        inv_line: usize,
        inv_column: usize,
    ) -> ExpansionSite {
        ExpansionSite {
            kind: ExpansionKind::Macro,
            name: Some(name.to_string()),
            definition_file: def_file.to_string(),
            definition_line: def_line,
            invocation_file: inv_file.to_string(),
            invocation_line: inv_line,
            invocation_column: inv_column,
            iteration: None,
        }
    }

    fn uleb_at(data: &[u8], pos: usize) -> (u64, usize) {
        let mut value = 0u64;
        let mut shift = 0u32;
        let mut i = pos;
        loop {
            let byte = data[i];
            i += 1;
            value |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        (value, i)
    }

    fn sleb_at(data: &[u8], pos: usize) -> (i64, usize) {
        let mut value = 0i64;
        let mut shift = 0u32;
        let mut i = pos;
        loop {
            let byte = data[i];
            i += 1;
            value |= ((byte & 0x7f) as i64) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    value |= -1i64 << shift;
                }
                break;
            }
        }
        (value, i)
    }

    /// One decoded `.debug_line` statement row.
    #[derive(Debug, PartialEq)]
    struct LineRow {
        address: u64,
        file: u32,
        line: i64,
        column: u64,
    }

    /// Decode a `.debug_line` section into its file table and row sequence.
    fn line_table(data: &[u8]) -> (Vec<String>, Vec<LineRow>) {
        let opcode_base = data[15] as usize;
        let line_base = data[13] as i8 as i64;
        let line_range = data[14] as i64;
        let mut pos = 16 + opcode_base - 1; // skip standard_opcode_lengths
        // include directories
        loop {
            let end = data[pos..].iter().position(|&b| b == 0).unwrap() + pos;
            if end == pos {
                pos += 1;
                break;
            }
            pos = end + 1;
        }
        // file names
        let mut files = Vec::new();
        loop {
            let end = data[pos..].iter().position(|&b| b == 0).unwrap() + pos;
            if end == pos {
                pos += 1;
                break;
            }
            files.push(String::from_utf8_lossy(&data[pos..end]).into_owned());
            pos = end + 1;
            for _ in 0..3 {
                let (_, next) = uleb_at(data, pos);
                pos = next;
            }
        }
        // line program
        let mut rows = Vec::new();
        let (mut address, mut file, mut line, mut column) = (0u64, 1u32, 1i64, 0u64);
        while pos < data.len() {
            let opcode = data[pos];
            pos += 1;
            match opcode {
                0 => {
                    let (length, next) = uleb_at(data, pos);
                    pos = next;
                    let end = pos + length as usize;
                    match data[pos] {
                        1 => break, // end_sequence terminates the sequence
                        2 => {
                            address = u16::from_le_bytes(
                                data[pos + 1..pos + 3].try_into().unwrap(),
                            ) as u64;
                        }
                        _ => {}
                    }
                    pos = end;
                }
                1 => rows.push(LineRow { address, file, line, column }), // copy
                2 => {
                    let (delta, next) = uleb_at(data, pos);
                    pos = next;
                    address += delta;
                }
                3 => {
                    let (delta, next) = sleb_at(data, pos);
                    pos = next;
                    line += delta;
                }
                4 => {
                    let (value, next) = uleb_at(data, pos);
                    pos = next;
                    file = value as u32;
                }
                5 => {
                    let (value, next) = uleb_at(data, pos);
                    pos = next;
                    column = value;
                }
                8 => {
                    // const_add_pc: advance like the largest special opcode.
                    address += (255 - opcode_base) as u64;
                }
                9 => {
                    address = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap()) as u64;
                    pos += 2;
                }
                opcode if opcode as usize >= opcode_base => {
                    let adjusted = (opcode - opcode_base as u8) as i64;
                    address += adjusted as u64;
                    line += line_base + adjusted * line_range;
                    rows.push(LineRow { address, file, line, column });
                }
                _ => {} // negate_stmt / set_basic_block / set_isa: no operands needed
            }
        }
        (files, rows)
    }

    /// One decoded `.debug_info` DIE.
    #[derive(Debug)]
    struct Die {
        offset: usize,
        tag: u8,
        /// (attribute, form, raw value bytes)
        attrs: Vec<(u8, u8, Vec<u8>)>,
    }

    fn attr(die: &Die, attribute: u8) -> &[u8] {
        &die.attrs.iter().find(|(at, _, _)| *at == attribute).unwrap().2
    }

    fn str_at(debug_str: &[u8], offset: u32) -> String {
        let start = offset as usize;
        let end = start + debug_str[start..].iter().position(|&b| b == 0).unwrap();
        String::from_utf8_lossy(&debug_str[start..end]).into_owned()
    }

    fn read_form(data: &[u8], pos: usize, form: u8, address_size: usize) -> (Vec<u8>, usize) {
        match form {
            0x01 => (data[pos..pos + address_size].to_vec(), pos + address_size), // addr
            0x05 => (data[pos..pos + 2].to_vec(), pos + 2),  // data2
            0x0e => (data[pos..pos + 4].to_vec(), pos + 4),  // strp
            0x12 => (data[pos..pos + 2].to_vec(), pos + 2),  // ref2
            0x13 => (data[pos..pos + 4].to_vec(), pos + 4),  // ref4
            0x17 => (data[pos..pos + 4].to_vec(), pos + 4),  // sec_offset
            0x0a => (data[pos..pos + 1].to_vec(), pos + 1),  // data1
            0x0f => {
                let (_, next) = uleb_at(data, pos);
                (data[pos..next].to_vec(), next)
            }
            0x0d => {
                let (_, next) = sleb_at(data, pos);
                (data[pos..next].to_vec(), next)
            }
            other => panic!("unsupported DWARF form {other:#x} in test parser"),
        }
    }

    /// Decode the `.debug_info` DIE stream (first compilation unit).
    fn parse_dies(info: &[u8], abbrev: &[u8]) -> Vec<Die> {
        // abbreviation declarations: code -> (tag, has_children, attribute/form pairs)
        let mut table: BTreeMap<u64, (u8, bool, Vec<(u8, u8)>)> = BTreeMap::new();
        let mut i = 0usize;
        while abbrev[i] != 0 {
            let (code, next) = uleb_at(abbrev, i);
            i = next;
            let tag = abbrev[i];
            i += 1;
            let has_children = abbrev[i] == 1;
            i += 1;
            let mut attrs = Vec::new();
            loop {
                if abbrev[i] == 0 && abbrev[i + 1] == 0 {
                    i += 2;
                    break;
                }
                attrs.push((abbrev[i], abbrev[i + 1]));
                i += 2;
            }
            table.insert(code, (tag, has_children, attrs));
        }
        let address_size = info[10] as usize;
        let mut pos = 11usize; // past the CU header
        let mut dies = Vec::new();
        let mut open = 0usize;
        while pos < info.len() {
            let offset = pos;
            let (code, next) = uleb_at(info, pos);
            pos = next;
            if code == 0 {
                if open == 0 {
                    break; // end of compilation-unit children
                }
                open -= 1;
                continue;
            }
            let (tag, has_children, attrs) = table[&code].clone();
            let mut values = Vec::new();
            for (attribute, form) in attrs {
                let (bytes, next) = read_form(info, pos, form, address_size);
                values.push((attribute, form, bytes));
                pos = next;
            }
            if has_children {
                open += 1;
            }
            dies.push(Die { offset, tag, attrs: values });
        }
        dies
    }

    #[test]
    fn macro_inline_die_stream_matches_declared_abbreviation_forms() {
        // A nested invocation chain exercises two abstract DIEs and a
        // two-level inline tree in one `.debug_info` contribution.
        let chain = vec![
            macro_site("A", "m.asm", 1, "main.asm", 10),
            macro_site("B", "b.asm", 5, "m.asm", 7),
        ];
        let rows = vec![macro_row(
            None,
            0x1000,
            2,
            "m.asm",
            2,
            ("b.asm", 6),
            "A_1",
            chain,
        )];
        let sections = debug_sections(&rows, &[], Path::new("C:/proj"), false);
        let abbrev = &section(&sections, ".debug_abbrev").data;
        let info = &section(&sections, ".debug_info").data;

        // Declaration side: the abbrev table must declare the inline DIE tag as
        // the spec constant `DW_TAG_inlined_subroutine` (0x1d) — never 0x28,
        // which is `DW_TAG_enumerator` and would make every inline frame
        // invisible to conformant DWARF consumers. Attribute, form, and tag
        // codes below 0x80 are single ULEB bytes, so the pairs appear
        // literally in `.debug_abbrev`.
        assert!(
            abbrev.windows(2).any(|w| w == [0x04, 0x1d]),
            "abbreviation code 4 must declare DW_TAG_inlined_subroutine (0x1d)"
        );
        assert!(
            !abbrev.windows(2).any(|w| w == [0x04, 0x28]),
            "abbreviation code 4 must not declare DW_TAG_enumerator (0x28)"
        );

        // Declaration side: the abbrev table must declare `DW_AT_abstract_origin`
        // (0x31) with `DW_FORM_ref4` (0x13) — the DIE stream emits the reference
        // as a 4-byte CU-relative offset — and never as `ref2` (0x12). Attribute
        // and form codes below 0x80 are single ULEB bytes, so the pairs appear
        // literally in `.debug_abbrev`.
        assert!(
            abbrev.windows(2).any(|w| w == [0x31, 0x13]),
            "abbrev must declare DW_AT_abstract_origin as DW_FORM_ref4"
        );
        assert!(
            !abbrev.windows(2).any(|w| w == [0x31, 0x12]),
            "abbrev must not declare DW_AT_abstract_origin as DW_FORM_ref2"
        );

        // Stream side: walk the DIEs strictly by the declared forms. `parse_dies`
        // panics on an abbreviation code that is missing from the table, which is
        // exactly what a form/encoding mismatch makes a conformant reader hit.
        let dies = parse_dies(info, abbrev);
        let subprograms: Vec<&Die> = dies.iter().filter(|die| die.tag == 0x2e).collect();
        let inlines: Vec<&Die> = dies.iter().filter(|die| die.tag == 0x1d).collect();
        assert_eq!(subprograms.len(), 2, "one abstract subprogram per macro definition");
        assert_eq!(inlines.len(), 2, "one inline DIE per chain level");

        // Each inline level references its abstract DIE with a 4-byte ref4 value
        // that resolves to that DIE's CU-relative offset.
        for inline in &inlines {
            let (_, form, value) = inline
                .attrs
                .iter()
                .find(|(at, _, _)| *at == 0x31)
                .expect("inline DIE carries DW_AT_abstract_origin");
            assert_eq!(*form, 0x13, "abstract_origin must use DW_FORM_ref4");
            assert_eq!(value.len(), 4, "abstract_origin must be encoded in 4 bytes");
            let target = u32::from_le_bytes(value[..].try_into().unwrap()) as usize;
            assert!(
                subprograms.iter().any(|die| die.offset == target),
                "abstract_origin {target:#x} must resolve to an abstract subprogram DIE"
            );
        }
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

    #[test]
    fn attribute_rows_split_groups_on_gaps_and_new_chains() {
        let chain_a = vec![macro_site("A", "m.asm", 1, "main.asm", 10)];
        let chain_b = vec![macro_site("B", "m.asm", 5, "main.asm", 11)];
        let rows = vec![
            macro_row(Some(0), 0, 1, "main.asm", 10, ("m.asm", 2), "A_1", chain_a.clone()),
            macro_row(Some(0), 1, 1, "main.asm", 10, ("m.asm", 3), "A_1", chain_a.clone()),
            macro_row(Some(0), 2, 1, "main.asm", 11, ("m.asm", 6), "B_2", chain_b),
            macro_row(Some(0), 3, 1, "main.asm", 10, ("m.asm", 4), "A_1", chain_a),
        ];

        let (attributed, groups) = attribute_rows(&rows);

        // Entry rows keep the invocation site; every row maps to its body line.
        assert_eq!(attributed.len(), 6);
        assert_eq!((attributed[0].file.as_str(), attributed[0].line_num), ("main.asm", 10));
        assert_eq!((attributed[1].file.as_str(), attributed[1].line_num), ("m.asm", 2));
        assert_eq!((attributed[2].file.as_str(), attributed[2].line_num), ("m.asm", 3));
        assert_eq!((attributed[3].file.as_str(), attributed[3].line_num), ("main.asm", 11));
        assert_eq!((attributed[4].file.as_str(), attributed[4].line_num), ("m.asm", 6));
        assert_eq!((attributed[5].file.as_str(), attributed[5].line_num), ("m.asm", 4));

        // A's contiguous run [0,2), the nested B run [2,3), and A resumed [3,4).
        assert_eq!(groups.len(), 3);
        assert_eq!((groups[0].start, groups[0].end), (0, 2));
        assert_eq!((groups[1].start, groups[1].end), (2, 3));
        assert_eq!((groups[2].start, groups[2].end), (3, 4));
        assert_eq!(groups[1].chain.len(), 1);
        assert_eq!(groups[1].chain[0].name.as_deref(), Some("B"));
    }

    #[test]
    fn attribute_rows_split_loop_iterations_into_separate_inline_instances() {
        let mut chain_first = vec![macro_site("A", "m.asm", 1, "main.asm", 10)];
        chain_first.push(ExpansionSite {
            kind: ExpansionKind::Loop,
            name: None,
            definition_file: "main.asm".to_string(),
            definition_line: 9,
            invocation_file: "main.asm".to_string(),
            invocation_line: 9,
            invocation_column: 1,
            iteration: Some(0),
        });
        let mut chain_second = chain_first.clone();
        chain_second[1].iteration = Some(1);
        let rows = vec![
            macro_row(Some(0), 0, 1, "main.asm", 10, ("m.asm", 2), "A_1", chain_first),
            macro_row(Some(0), 1, 1, "main.asm", 10, ("m.asm", 2), "A_1", chain_second),
        ];

        let (_, groups) = attribute_rows(&rows);

        // Contiguous addresses, but each iteration is its own inline instance.
        assert_eq!(groups.len(), 2);
        assert_eq!((groups[0].start, groups[0].end), (0, 1));
        assert_eq!((groups[1].start, groups[1].end), (1, 2));
    }

    #[test]
    fn macro_rows_map_body_lines_and_keep_invocation_entry_row() {
        let chain = vec![macro_site_with_column("PAUSE", "engine/macros.asm", 299, "main.asm", 42, 11)];
        let rows = vec![
            macro_row(Some(0), 0, 2, "main.asm", 42, ("engine/macros.asm", 304), "PAUSE_1", chain.clone()),
            macro_row(Some(0), 2, 1, "main.asm", 42, ("engine/macros.asm", 305), "PAUSE_1", chain),
        ];

        let sections = debug_sections(&rows, &[], Path::new("C:/proj"), true);
        let line = section(&sections, ".debug_line");
        let (files, line_rows) = line_table(&line.data);

        assert!(files.iter().any(|file| file == "macros.asm"));
        assert!(files.iter().any(|file| file == "main.asm"));
        assert_eq!(line_rows.len(), 3);
        // Entry address 0: invocation row first, then the macro body row.
        // (The line-table file table stores basenames; in object mode every
        // SET_ADDRESS field is zero-baked and fixed up via relocations.)
        assert_eq!(line_rows[0].file as usize - 1, files.iter().position(|file| file == "main.asm").unwrap());
        assert_eq!(line_rows[0].line, 42);
        assert_eq!(line_rows[1].file as usize - 1, files.iter().position(|file| file == "macros.asm").unwrap());
        assert_eq!(line_rows[1].line, 304);
        assert_eq!(line_rows[2].line, 305);
        // The entry row points at the invocation's own column (11); the body
        // rows keep the body statement's column (0 in this fixture).
        assert_eq!(line_rows[0].column, 11);
        assert_eq!(line_rows[1].column, 0);
        assert_eq!(line_rows[2].column, 0);
        let addends: Vec<i64> = line.relocs.iter().map(|reloc| reloc.addend).collect();
        assert_eq!(addends, [0, 0, 2]);
    }

    #[test]
    fn macro_expansion_emits_abstract_subprogram_and_inline_dies() {
        let chain = vec![macro_site_with_column("PAUSE", "engine/macros.asm", 299, "main.asm", 42, 11)];
        let rows = vec![macro_row(
            Some(0),
            4,
            2,
            "main.asm",
            42,
            ("engine/macros.asm", 304),
            "PAUSE_1",
            chain,
        )];

        let sections = debug_sections(&rows, &[], Path::new("C:/proj"), true);
        let info = section(&sections, ".debug_info");
        let dies = parse_dies(&info.data, &section(&sections, ".debug_abbrev").data);
        let str_data = &section(&sections, ".debug_str").data;
        let (files, _) = line_table(&section(&sections, ".debug_line").data);

        let subprograms: Vec<&Die> = dies.iter().filter(|die| die.tag == 0x2e).collect();
        assert_eq!(subprograms.len(), 1);
        let name_offset = u32::from_le_bytes(attr(subprograms[0], 0x03).try_into().unwrap());
        assert_eq!(str_at(str_data, name_offset), "PAUSE");
        let macros_index =
            files.iter().position(|file| file == "macros.asm").unwrap() as u32 + 1;
        assert_eq!(uleb_at(attr(subprograms[0], 0x3a), 0).0, macros_index as u64);
        assert_eq!(uleb_at(attr(subprograms[0], 0x3b), 0).0, 299);

        let inlines: Vec<&Die> = dies.iter().filter(|die| die.tag == 0x1d).collect();
        assert_eq!(inlines.len(), 1);
        let origin_ref = u32::from_le_bytes(attr(inlines[0], 0x31).try_into().unwrap());
        assert_eq!(origin_ref as usize, subprograms[0].offset);
        // low_pc is a zero-baked section-relative field; high_pc is the run span.
        assert_eq!(attr(inlines[0], 0x11), &[0, 0]);
        assert_eq!(attr(inlines[0], 0x12), &[2, 0]);
        let main_index = files.iter().position(|file| file == "main.asm").unwrap() as u32 + 1;
        assert_eq!(uleb_at(attr(inlines[0], 0x58), 0).0, main_index as u64);
        assert_eq!(uleb_at(attr(inlines[0], 0x59), 0).0, 42);
        assert_eq!(uleb_at(attr(inlines[0], 0x5a), 0).0, 11);

        // The low_pc field carries a section-relative code relocation.
        let reloc = info.relocs.iter().find(|reloc| reloc.kind == RelocKind::Abs16).unwrap();
        assert_eq!(reloc.target, RelocTarget::Section(0));
        assert_eq!(reloc.addend, 4);
        assert_eq!(&info.data[reloc.offset as usize..reloc.offset as usize + 2], &[0, 0]);
    }

    #[test]
    fn nested_macro_expansions_nest_inline_dies() {
        let outer = macro_site_with_column("OUTER", "lib/macros.asm", 10, "main.asm", 7, 5);
        let inner = macro_site_with_column("INNER", "lib/macros.asm", 2, "lib/macros.asm", 12, 13);
        let rows = vec![macro_row(
            Some(0),
            0,
            1,
            "main.asm",
            7,
            ("lib/macros.asm", 3),
            "INNER_2",
            vec![outer, inner],
        )];

        let sections = debug_sections(&rows, &[], Path::new("C:/proj"), true);
        let dies = parse_dies(
            &section(&sections, ".debug_info").data,
            &section(&sections, ".debug_abbrev").data,
        );
        let str_data = &section(&sections, ".debug_str").data;
        let (files, _) = line_table(&section(&sections, ".debug_line").data);

        let subprograms: Vec<&Die> = dies.iter().filter(|die| die.tag == 0x2e).collect();
        assert_eq!(subprograms.len(), 2);
        let mut definitions: Vec<(String, u64)> = subprograms
            .iter()
            .map(|die| {
                (
                    str_at(str_data, u32::from_le_bytes(attr(die, 0x03).try_into().unwrap())),
                    uleb_at(attr(die, 0x3b), 0).0,
                )
            })
            .collect();
        definitions.sort();
        assert_eq!(definitions, vec![("INNER".to_string(), 2), ("OUTER".to_string(), 10)]);

        let inlines: Vec<&Die> = dies.iter().filter(|die| die.tag == 0x1d).collect();
        assert_eq!(inlines.len(), 2);
        let find_abstract = |name: &str| {
            subprograms
                .iter()
                .find(|die| {
                    str_at(str_data, u32::from_le_bytes(attr(die, 0x03).try_into().unwrap())) == name
                })
                .unwrap()
                .offset
        };
        // The outer inline DIE precedes the inner one, which it nests.
        assert_eq!(
            u32::from_le_bytes(attr(inlines[0], 0x31).try_into().unwrap()) as usize,
            find_abstract("OUTER")
        );
        assert_eq!(
            u32::from_le_bytes(attr(inlines[1], 0x31).try_into().unwrap()) as usize,
            find_abstract("INNER")
        );
        // Outer call site: main.asm:7. Inner call site: the outer body line 12.
        let main_index = files.iter().position(|file| file == "main.asm").unwrap() as u32 + 1;
        assert_eq!(uleb_at(attr(inlines[0], 0x58), 0).0, main_index as u64);
        assert_eq!(uleb_at(attr(inlines[0], 0x59), 0).0, 7);
        assert_eq!(uleb_at(attr(inlines[0], 0x5a), 0).0, 5);
        let macros_index =
            files.iter().position(|file| file == "macros.asm").unwrap() as u32 + 1;
        assert_eq!(uleb_at(attr(inlines[1], 0x58), 0).0, macros_index as u64);
        assert_eq!(uleb_at(attr(inlines[1], 0x59), 0).0, 12);
        assert_eq!(uleb_at(attr(inlines[1], 0x5a), 0).0, 13);
        // Both levels cover the same single-instruction run.
        assert_eq!(attr(inlines[0], 0x12), &[1, 0]);
        assert_eq!(attr(inlines[1], 0x12), &[1, 0]);
    }

    #[test]
    fn rom_companion_bakes_absolute_inline_addresses_without_relocations() {
        let chain = vec![macro_site("PAUSE", "engine/macros.asm", 299, "main.asm", 42)];
        let rows = vec![macro_row(
            None,
            0x1234,
            2,
            "main.asm",
            42,
            ("engine/macros.asm", 304),
            "PAUSE_1",
            chain,
        )];

        let sections = debug_sections(&rows, &[], Path::new("C:/proj"), false);
        let info = section(&sections, ".debug_info");
        assert!(info.relocs.is_empty());
        let dies = parse_dies(&info.data, &section(&sections, ".debug_abbrev").data);
        let inlines: Vec<&Die> = dies.iter().filter(|die| die.tag == 0x1d).collect();
        assert_eq!(inlines.len(), 1);
        assert_eq!(attr(inlines[0], 0x11), &[0x34, 0x12]);
        assert_eq!(attr(inlines[0], 0x12), &[2, 0]);
    }
}