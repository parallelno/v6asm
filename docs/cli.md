# CLI Usage

```
v6asm <source.asm> [options]
v6asm --init <name>
```

## Arguments

| Argument | Description |
|----------|-------------|
| `<source>` | Assembly source file (`.asm`) to compile |
| `-i`, `--init <name>` | Scaffold a new `.asm` file with a starter template |
| `-o`, `--output <path>` | Output path (default: `<source>.rom`, or `<source>.o` in object mode) |
| `-f`, `--format <format>` | Output format: `rom` (default) or `obj` (relocatable ELF object) |
| `-c`, `--cpu <cpu>` | Target CPU: `i8080` (default) or `z80` |
| `-a`, `--rom-align <n>` | ROM size alignment in bytes (default: `1`; ignored in object mode) |
| `-I`, `--include-dir <path>` | Extra directory to search for `.include` files; repeatable |
| `-D`, `--define <NAME[=VALUE]>` | Pre-define a symbol; `VALUE` is decimal or `0x`-prefixed hex (default `1`); repeatable |
| `-q`, `--quiet` | Suppress `.print` output |
| `-V`, `--verbose` | Extra diagnostics |
| `-l`, `--lst` | Generate a listing file (`.lst`) alongside the ROM |
| `-g`, `--debug` | Emit DWARF v4 source metadata; writes a companion `.elf` in ROM mode |
| `--debug-elf <path>` | Explicit companion ELF path for ROM debug output |
| `-v`, `--version` | Print the build version string |
| `-h`, `--help` | Print help with the program header |

## Help And Version

`-v` / `--version` prints the build version in the form `YYYY.MM.DD-<git-hash>`.

`-h` / `--help` prints the normal clap-generated help preceded by:

```text
Intel 8080/Z80 assembler, version <version>
(c) Aleksandr Fedotovskikh <mailforfriend@gmail.com>
```

## Console Output

Diagnostics on the console are colorized: errors and carets in red, warnings
in yellow, source locations and notes in cyan, summaries (`ROM:`/`OBJ:`,
`Compilation completed`) in green, and verbose details dimmed. When stderr is
not a terminal (pipes, files, CI) the colors are stripped automatically, and
the standard `NO_COLOR` variable disables them explicitly.

## Examples

```bash
v6asm main.asm                        # compile, output main.rom
v6asm -i main                         # create main.asm from template
v6asm main.asm -o out/program.rom     # custom output path
v6asm main.asm -c z80 -l              # Z80 mode + listing
v6asm -I libs -I shared main.asm      # search extra include directories
v6asm main.asm -D DEBUG=1 -D VERSION=0x10  # pre-define symbols
v6asm main.asm -D FEATURE             # pre-define FLAG=1 (boolean)
v6asm main.asm -f obj                 # emit relocatable ELF object main.o
v6asm -g -f obj main.asm -o main.o    # emit an object with DWARF v4 metadata
v6asm -g main.asm -o main.rom         # emit main.rom and sibling main.elf
v6asm -g main.asm --debug-elf out.elf # choose the ROM companion ELF path
v6asm -v                              # print version
```

## Output Artifacts

- `<name>.rom` — Vector 06c executable loaded by the emulator.
- `<name>.o` — relocatable ELF32 object (object mode, `-f obj`) for linking with `ld.lld`. See [Object Output](object-output.md).
- `<name>.elf` — debug companion for a ROM invocation with `-g`, unless `--debug-elf` selects another path.
- `<name>.lst` — optional listing file (enabled with `--lst`) showing addresses, emitted bytes, and source lines. See [Listing File Format](listing.md) for details.

See [Debug Metadata](debug-metadata.md) for object and direct-ROM workflows,
emitted DWARF sections, source attribution, and current limitations.
