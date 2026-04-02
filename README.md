# v6asm

> A two-pass Intel 8080 / Z80-compatible assembler for the **Vector-06c** home computer, written in Rust.

[![CI/CD](https://github.com/parallelno/v6asm/actions/workflows/ci.yml/badge.svg)](https://github.com/parallelno/v6asm/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

---

## Overview

`v6asm` is a command-line toolchain for developing software for the **Vector-06c** (Вектор-06Ц) — a Soviet-era Z80-compatible home computer. It assembles `.asm` source files into `.rom` binaries and can optionally build bootable **FDD disk images** ready to load in an emulator.

The workspace contains two standalone CLI tools:

| Tool | Purpose |
|------|---------|
| `v6asm` | Assembler — compiles `.asm` source into a `.rom` binary |
| `v6fdd` | FDD image utility — packs files into a `.fdd` disk image |

---

## Features

- **Intel 8080 instruction set** with optional **Z80 mnemonic compatibility** (`LD`, `JP`, `CALL`, port I/O forms, etc.)
- **Two-pass assembly** with full forward-reference resolution
- **Rich expression engine** — arithmetic, bitwise, logical, shift, comparison, unary `<`/`>` (low/high byte), operator precedence
- **Preprocessor directives**: `.include`, `.macro`/`.endmacro`, `.loop`/`.endloop`, `.if`/`.endif`, `.optional`/`.endoptional`, `.incbin`, `.filesize`, `.print`, `.error`, `.setting`
- **Local labels** (`@name`) with automatic scope management
- **Mutable variables** (`.var`) alongside immutable constants (`=` / `EQU`)
- **FDD image builder** — creates or patches 820 KB disk images from the built-in `rds308.fdd` template or a custom one
- **`--init` scaffolding** — generate a ready-to-build `.asm` file from a starter template
- Prebuilt binaries for **Linux**, **Windows**, and **macOS**

---

## Installation

### Download a prebuilt release

Grab the latest archive for your platform from the [Releases](https://github.com/parallelno/v6asm/releases) page and extract it. The archive contains:

```
v6asm       (v6asm.exe on Windows)
v6fdd       (v6fdd.exe on Windows)
docs/
README.md
```

Add the extracted directory to your `PATH` or copy the binaries to a directory that is already on it.

### Build from source

Prerequisites: [Rust toolchain](https://rustup.rs/) (stable).

```bash
git clone https://github.com/parallelno/v6asm.git
cd v6asm
cargo build --release
```

Binaries are written to `target/release/v6asm` and `target/release/v6fdd`.

---

## Quick Start

```bash
# Create a new .asm file from the starter template
v6asm --init main

# Assemble it
v6asm main.asm

# Custom output path
v6asm main.asm -o out/program.rom

# Z80 mode + listing
v6asm main.asm --cpu z80 --lst
```

After a successful build you will find:

- `main.rom` — the assembled binary
- `main.lst` — optional listing file (if `--lst` is passed)

---


## v6fdd — FDD Image Utility

```
USAGE:
    v6fdd -i <file> [-i <file>...] -o <output.fdd> [-t <template.fdd>]

OPTIONS:
    -t, --template   Template FDD image to start from (uses built-in rds308 if omitted)
    -i, --input      File to add to the disk (repeatable)
    -o, --output     Output FDD image path (required)
```

Example:

```bash
v6fdd -t rds308.fdd -i myprogram.rom -i extra.dat -o out/disk.fdd
```

---

## Assembly Language Reference

Full documentation is in [`docs/assembler.md`](docs/assembler.md). The highlights are below.

---

## Workspace Structure

```
Cargo.toml              ← workspace manifest
crates/
  v6_core/              ← shared library
    src/
      assembler.rs      ← two-pass assembler orchestrator
      diagnostics.rs    ← error types and source locations
      encoding.rs       ← character encoding helpers
      expr.rs           ← expression evaluator (Pratt parser)
      fdd/              ← FDD image read/write
      instructions/     ← Intel 8080 opcode table; Z80 compat mapping
      lexer.rs          ← tokenizer
      output.rs         ← ROM binary + listing emitter
      parser.rs         ← directive/instruction parser
      preprocessor.rs   ← macro expansion, .include, .loop, .if
      project.rs        ← CPU mode types
      symbols.rs        ← symbol table (labels, consts, macros)
  v6asm/                ← assembler CLI binary
    src/
      main.rs
      templates/
        main.asm        ← embedded template for --init
  v6fdd/                ← FDD utility CLI binary
    src/
      main.rs
docs/
  assembler.md          ← full assembler language specification
```

---

## Running Tests

```bash
cargo test --workspace
```

---

## License

This project is released under the [MIT License](LICENSE).
