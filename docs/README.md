# v6asm Documentation

Intel 8080/Z80 assembler for Vector 06c.

## Table of Contents

1. [CLI Usage](cli.md) — command-line arguments, options, and output artifacts
2. [Assembler Syntax](syntax.md) — comments, expressions, operators, numeric literals, symbols, and precedence rules
3. [Directives](directives.md) — directives, data emission, constants, variables, and more
4. [Structs](structs.md) — user-defined composite data types, inheritance, pools, and references
5. [Macros](macros.md) — `.macro` / `.endmacro`, parameter defaults, label and constant scoping, nesting
6. [Listing File Format](listing.md) — `.lst` column layout, address/bytes/source columns, include and loop expansion behavior
7. [Object Output](object-output.md) — relocatable ELF objects (`-f obj`), sections, symbols, and relocations
8. [Debug Metadata](debug-metadata.md) — DWARF v4 object output, source attribution, symbols, and debugger workflow
9. [Scripts Folder](scripts/README.md) — batch helpers for local build and publish workflows
