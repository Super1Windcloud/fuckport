# fuckport

[![crates.io](https://img.shields.io/crates/v/fuckport.svg)](https://crates.io/crates/fuckport)
[![docs.rs](https://img.shields.io/docsrs/fuckport)](https://docs.rs/fuckport)
[![license](https://img.shields.io/crates/l/fuckport.svg)](LICENSE)

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
cargo install fuckport
```

For local development:

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

Kill a process by PID:

```bash
fuckport 1337
```

Kill a process by name:

```bash
fuckport safari
```

Kill whatever is listening on a port:

```bash
fuckport :8080
```

Kill multiple targets in one command:

```bash
fuckport 1337 safari :8080
```

Run without arguments to open the interactive interface:

```bash
fuckport
```

To kill a port, prefix it with a colon, for example `:8080`.

The interactive interface can be closed with `Esc` without killing anything.

Process name matching is case-insensitive by default. Queries containing uppercase letters use smart-case matching.

Library-style examples:

```bash
cargo run --example parse_targets
cargo run --example list_processes
```

## Test

```bash
cargo test
```

## Release

```bash
cargo package
cargo publish
```
