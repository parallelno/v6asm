# v6asm

> A two-pass Intel 8080 / Z80 assembler for the **Vector-06c** home computer, written in Rust.

[![CI/CD](https://github.com/parallelno/v6asm/actions/workflows/ci.yml/badge.svg)](https://github.com/parallelno/v6asm/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

---

## Overview

`v6asm` is a command-line toolchain for the **Vector-06c** (Вектор-06Ц).
It assembles `.asm` source files into `.rom` binaries and can build bootable **FDD disk images** for an emulator.

The assembler runs two passes — the first collects symbols and sizes, the second
emits machine code with all forward references resolved. It supports the full
Intel 8080 instruction set and an optional Z80 mnemonic alternatives.
A rich preprocessor handles file includes, macros, conditional
assembly, loops, and optional code blocks. The toolchain can also emit a listing file
for inspection and a `.symbols.json` for editor/debugger integration.

| Tool | Purpose |
|------|---------|
| `v6asm` | Assembler — `.asm` → `.rom` binary |
| `v6fdd` | FDD utility — packs files into a `.fdd` disk image |

## Features

- **Intel 8080** instruction set with optional **Z80 mnemonic** compatibility
- **Two-pass assembly** with forward-reference resolution
- **Expressions** — arithmetic, bitwise, logical, shift, comparison, low/high byte operators
- **Preprocessor** — `.include`, `.macro`, `.loop`, `.if`, `.optional`, `.incbin`, and more
- **Local labels**, mutable variables (`.var`), and immutable constants (`=` / `EQU`)
- **Debug symbols** (`--symbols`) — `.symbols.json` for editor/debugger integration
- **FDD image builder** — `v6fdd` creates 820 KB disk images
- **`--init` scaffolding** — starter template for new projects
- Prebuilt binaries for **Linux**, **Windows**, and **macOS**

## Installation

### Prebuilt release

Download the latest archive from [Releases](https://github.com/parallelno/v6asm/releases), extract it, and add the directory to your `PATH`.

### Build from source

Requires the [Rust toolchain](https://rustup.rs/) (stable).

```bash
git clone https://github.com/parallelno/v6asm.git
cd v6asm
cargo build --release
```

## Quick Start

```bash
v6asm --init main              # scaffold a new project
v6asm main.asm                 # assemble → main.rom
v6asm main.asm --lst           # + listing file
v6asm main.asm --symbols       # + debug symbols
v6asm main.asm --cpu z80       # Z80 mnemonic mode
```

See [CLI Usage](docs/cli.md) for all options and output artifacts.

## Documentation

Full reference is in the [`docs/`](docs/README.md) folder:

- [CLI Usage](docs/cli.md) — arguments, options, output artifacts
- [Assembler Syntax](docs/syntax.md) — expressions, operators, literals, symbols
- [Directives](docs/directives.md) — `.org`, `.include`, `.if`, `.loop`, `.optional`, data emission, and more
- [Macros](docs/macros.md) — `.macro` / `.endmacro`, parameters, scoping
- [Listing Format](docs/listing.md) — `.lst` column layout and expansion behavior
- [Debug Symbols](docs/symbols.md) — `.symbols.json` schema, symbol types, and naming conventions

## Tests

```bash
cargo test --workspace
```

## License

[MIT](LICENSE)
