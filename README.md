# fuckport

`fuckport` is a small command-line tool for stopping processes by PID, process name, or listening port.

## Features

- Kill by PID: `fuckport 1234`
- Kill by process name: `fuckport chrome`
- Kill by port: `fuckport :3000`
- Kill multiple targets in one run
- Interactive picker when no target is provided
- Graceful shutdown first, with automatic fallback to force kill

## Install

```bash
cargo install --path .
```

Or build locally:

```bash
cargo build --release
```

## Usage

```bash
fuckport [OPTIONS] [TARGET]...
```

Targets:

- `1234` treats the value as a PID
- `:8080` treats the value as a port
- Any other value is matched against process name and command line

Options:

- `-f, --force`: force kill immediately
- `-c, --case-sensitive`: enable case-sensitive name matching
- `-i, --interactive`: open the interactive selector
- `-s, --silent`: suppress success output
- `-v, --verbose`: show more detail in interactive mode
- `--force-after-timeout <N>`: wait time before escalating to force kill
- `--wait-for-exit <N>`: maximum wait before reporting failure

## Examples

```bash
fuckport :5173
fuckport node vite
fuckport 4242 -f
fuckport --interactive
```

Library-style examples:

```bash
cargo run --example parse_targets
cargo run --example list_processes
```

## Test

```bash
cargo test
```
