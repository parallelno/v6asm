use crate::diagnostics::{AsmError, AsmResult};
use super::{EncodedInstruction, ParsedOperand, Register, RegisterPair, Condition, i8080};

/// Encode a Z80-compatibility instruction (maps to i8080 opcodes)
pub fn encode(mnemonic: &str, operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    let upper = mnemonic.to_uppercase();

    // Z80-specific mnemonics must be checked BEFORE the i8080 fallback because some
    // share names with i8080 instructions that have different semantics.
    // For example, i8080 "JP nn" = "Jump if Positive" (conditional, 0xF2),
    // but Z80 "JP nn" = unconditional jump (0xC3).
    match upper.as_str() {
        "LD"   => return encode_ld(operands),
        "ADD"  => return encode_z80_add(operands),
        "ADC"  => return encode_z80_adc(operands),
        "SUB"  => return encode_z80_sub(operands),
        "SBC"  => return encode_z80_sbc(operands),
        "AND"  => return encode_z80_alu_reg(operands, 0xA0, 0xE6),
        "XOR"  => return encode_z80_alu_reg(operands, 0xA8, 0xEE),
        "OR"   => return encode_z80_alu_reg(operands, 0xB0, 0xF6),
        "CP"   => return encode_z80_alu_reg(operands, 0xB8, 0xFE),
        "INC"  => return encode_z80_inc_dec(operands, true),
        "DEC"  => return encode_z80_inc_dec(operands, false),
        "JP"   => return encode_z80_jp(operands),
        "CALL" => return encode_z80_call(operands),
        "RET"  => return encode_z80_ret(operands),
        "EX"   => return encode_z80_ex(operands),
        "HALT" => return i8080::encode("HLT", &[]),
        "RLCA" => return i8080::encode("RLC", &[]),
        "RRCA" => return i8080::encode("RRC", &[]),
        "RLA"  => return i8080::encode("RAL", &[]),
        "RRA"  => return i8080::encode("RAR", &[]),
        "CPL"  => return i8080::encode("CMA", &[]),
        "SCF"  => return i8080::encode("STC", &[]),
        "CCF"  => return i8080::encode("CMC", &[]),
        _ => {}
    }

    // Fall back to direct i8080 mnemonics (MOV, MVI, LXI, PUSH, POP, etc. still work in Z80 mode)
    if let Ok(enc) = i8080::encode(mnemonic, operands) {
        return Ok(enc);
    }

    Err(AsmError::new(format!("Unknown Z80 instruction: {}", mnemonic)))
}

fn reg_code(op: &ParsedOperand) -> AsmResult<u8> {
    match op {
        ParsedOperand::Reg(r) => Ok(r.code()),
        ParsedOperand::Memory => Ok(6),
        _ => Err(AsmError::new("Expected register or (HL)")),
    }
}

fn encode_ld(operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    if operands.len() < 2 {
        return Err(AsmError::new("LD requires two operands"));
    }
    match (&operands[0], &operands[1]) {
        // LD r, r' / LD r, (HL) / LD (HL), r
        (ParsedOperand::Reg(_) | ParsedOperand::Memory, ParsedOperand::Reg(_) | ParsedOperand::Memory) => {
            i8080::encode("MOV", operands)
        }
        // LD r, n (immediate)
        (ParsedOperand::Reg(_) | ParsedOperand::Memory, ParsedOperand::Imm8 | ParsedOperand::Imm16) => {
            i8080::encode("MVI", operands)
        }
        // LD rp, nn (16-bit immediate)
        (ParsedOperand::RegPair(_), ParsedOperand::Imm16) => {
            i8080::encode("LXI", operands)
        }
        // LD HL, (nn) — LHLD
        (ParsedOperand::RegPair(RegisterPair::HL), ParsedOperand::Mem16) => {
            i8080::encode("LHLD", operands)
        }
        // LD A, (BC) / LD A, (DE)
        (ParsedOperand::Reg(Register::A), ParsedOperand::RegPair(RegisterPair::BC)) => {
            i8080::encode("LDAX", &[ParsedOperand::RegPair(RegisterPair::BC)])
        }
        (ParsedOperand::Reg(Register::A), ParsedOperand::RegPair(RegisterPair::DE)) => {
            i8080::encode("LDAX", &[ParsedOperand::RegPair(RegisterPair::DE)])
        }
        // LD (BC), A / LD (DE), A
        (ParsedOperand::RegPair(RegisterPair::BC), ParsedOperand::Reg(Register::A)) => {
            i8080::encode("STAX", &[ParsedOperand::RegPair(RegisterPair::BC)])
        }
        (ParsedOperand::RegPair(RegisterPair::DE), ParsedOperand::Reg(Register::A)) => {
            i8080::encode("STAX", &[ParsedOperand::RegPair(RegisterPair::DE)])
        }
        // LD A, (nn) — LDA
        (ParsedOperand::Reg(Register::A), ParsedOperand::Mem16) => {
            i8080::encode("LDA", operands)
        }
        // LD (nn), A — STA
        (ParsedOperand::Mem16, ParsedOperand::Reg(Register::A)) => {
            i8080::encode("STA", operands)
        }
        // LD SP, HL — SPHL
        (ParsedOperand::RegPair(RegisterPair::SP), ParsedOperand::RegPair(RegisterPair::HL)) => {
            i8080::encode("SPHL", &[])
        }
        _ => Err(AsmError::new("Invalid operand combination for LD")),
    }
}

fn encode_z80_add(operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    if operands.is_empty() {
        return Err(AsmError::new("ADD requires operands"));
    }
    match &operands[0] {
        // ADD A, r / ADD A, (HL) — skip the A and use second operand
        ParsedOperand::Reg(Register::A) if operands.len() >= 2 => {
            i8080::encode("ADD", &operands[1..])
        }
        // ADD HL, rp — DAD
        ParsedOperand::RegPair(RegisterPair::HL) if operands.len() >= 2 => {
            i8080::encode("DAD", &operands[1..])
        }
        // ADD r (implied A as destination)
        _ => i8080::encode("ADD", operands),
    }
}

fn encode_z80_adc(operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    if operands.is_empty() {
        return Err(AsmError::new("ADC requires operands"));
    }
    match &operands[0] {
        ParsedOperand::Reg(Register::A) if operands.len() >= 2 => {
            i8080::encode("ADC", &operands[1..])
        }
        _ => i8080::encode("ADC", operands),
    }
}

fn encode_z80_sub(operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    if operands.is_empty() {
        return Err(AsmError::new("SUB requires operands"));
    }
    match &operands[0] {
        ParsedOperand::Reg(Register::A) if operands.len() >= 2 => {
            i8080::encode("SUB", &operands[1..])
        }
        _ => i8080::encode("SUB", operands),
    }
}

fn encode_z80_sbc(operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    if operands.is_empty() {
        return Err(AsmError::new("SBC requires operands"));
    }
    match &operands[0] {
        ParsedOperand::Reg(Register::A) if operands.len() >= 2 => {
            i8080::encode("SBB", &operands[1..])
        }
        _ => i8080::encode("SBB", operands),
    }
}

fn encode_z80_alu_reg(operands: &[ParsedOperand], reg_base: u8, imm_opcode: u8) -> AsmResult<EncodedInstruction> {
    if operands.is_empty() {
        return Err(AsmError::new("ALU instruction requires an operand"));
    }
    match &operands[0] {
        ParsedOperand::Imm8 | ParsedOperand::Imm16 => {
            Ok(EncodedInstruction {
                bytes: vec![imm_opcode, 0],
                size: 2,
                has_imm8: true,
                has_imm16: false,
                prefix_operands: 0,
            })
        }
        _ => {
            let src = reg_code(&operands[0])?;
            Ok(EncodedInstruction {
                bytes: vec![reg_base | src],
                size: 1,
                has_imm8: false,
                has_imm16: false,
                prefix_operands: 0,
            })
        }
    }
}

fn encode_z80_inc_dec(operands: &[ParsedOperand], is_inc: bool) -> AsmResult<EncodedInstruction> {
    if operands.is_empty() {
        return Err(AsmError::new("INC/DEC requires an operand"));
    }
    match &operands[0] {
        ParsedOperand::RegPair(_) => {
            if is_inc {
                i8080::encode("INX", operands)
            } else {
                i8080::encode("DCX", operands)
            }
        }
        _ => {
            if is_inc {
                i8080::encode("INR", operands)
            } else {
                i8080::encode("DCR", operands)
            }
        }
    }
}

fn encode_z80_jp(operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    if operands.is_empty() {
        return i8080::encode("JMP", &[]);
    }
    // JP (HL) — PCHL
    if operands.len() == 1 {
        if let ParsedOperand::Memory = &operands[0] {
            return i8080::encode("PCHL", &[]);
        }
    }
    // JP cc, nn — conditional jump
    if let ParsedOperand::Condition(cc) = &operands[0] {
        let opcode = match cc {
            Condition::NZ => 0xC2,
            Condition::Z  => 0xCA,
            Condition::NC => 0xD2,
            Condition::C  => 0xDA,
            Condition::PO => 0xE2,
            Condition::PE => 0xEA,
            Condition::P  => 0xF2,
            Condition::M  => 0xFA,
        };
        return Ok(EncodedInstruction {
            bytes: vec![opcode, 0, 0],
            size: 3,
            has_imm8: false,
            has_imm16: true,
            prefix_operands: 1,
        });
    }
    // JP nn — unconditional
    i8080::encode("JMP", &[])
}

fn encode_z80_call(operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    if operands.is_empty() {
        return i8080::encode("CALL", &[]);
    }
    if let ParsedOperand::Condition(cc) = &operands[0] {
        let opcode = match cc {
            Condition::NZ => 0xC4,
            Condition::Z  => 0xCC,
            Condition::NC => 0xD4,
            Condition::C  => 0xDC,
            Condition::PO => 0xE4,
            Condition::PE => 0xEC,
            Condition::P  => 0xF4,
            Condition::M  => 0xFC,
        };
        return Ok(EncodedInstruction {
            bytes: vec![opcode, 0, 0],
            size: 3,
            has_imm8: false,
            has_imm16: true,
            prefix_operands: 1,
        });
    }
    i8080::encode("CALL", &[])
}

fn encode_z80_ret(operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    if operands.is_empty() {
        return i8080::encode("RET", &[]);
    }
    if let ParsedOperand::Condition(cc) = &operands[0] {
        let opcode = match cc {
            Condition::NZ => 0xC0,
            Condition::Z  => 0xC8,
            Condition::NC => 0xD0,
            Condition::C  => 0xD8,
            Condition::PO => 0xE0,
            Condition::PE => 0xE8,
            Condition::P  => 0xF0,
            Condition::M  => 0xF8,
        };
        return Ok(EncodedInstruction {
            bytes: vec![opcode],
            size: 1,
            has_imm8: false,
            has_imm16: false,
            prefix_operands: 0,
        });
    }
    i8080::encode("RET", &[])
}

fn encode_z80_ex(operands: &[ParsedOperand]) -> AsmResult<EncodedInstruction> {
    // EX DE, HL → XCHG
    // EX (SP), HL → XTHL
    if operands.len() >= 2 {
        match (&operands[0], &operands[1]) {
            (ParsedOperand::RegPair(RegisterPair::DE), ParsedOperand::RegPair(RegisterPair::HL)) => {
                return i8080::encode("XCHG", &[]);
            }
            _ => {}
        }
    }
    // For EX (SP),HL we'd need special handling, use XTHL
    i8080::encode("XTHL", &[])
}
