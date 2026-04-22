# Repository Guidelines

## Project Structure & Module Organization
This repository is a small Rust CLI crate with one binary and reusable library modules.

- `src/main.rs`: entrypoint for the `fuckport` CLI.
- `src/lib.rs`: exports shared modules used by the binary and examples.
- `src/cli.rs`, `src/input.rs`, `src/process.rs`, `src/killer.rs`, `src/interactive.rs`, `src/error.rs`: argument parsing, target parsing, process discovery, kill logic, TUI flow, and error handling.
- `tests/`: integration tests for end-to-end CLI behavior, for example [`tests/cli_help.rs`](tests/cli_help.rs).
- `examples/`: small runnable examples such as `cargo run --example parse_targets`.
- `target/`: build artifacts; do not commit changes from this directory.

## Build, Test, and Development Commands
- `cargo build`: compile the crate in debug mode.
- `cargo build --release`: produce an optimized binary.
- `cargo run -- :8080`: run the CLI locally against a sample target.
- `cargo test`: run unit and integration tests.
- `cargo test -- --nocapture`: run tests with printed output visible.
- `cargo run --example parse_targets`: exercise library behavior from `examples/`.
- `cargo fmt` and `cargo clippy --all-targets --all-features -D warnings`: format and lint before opening a PR.

## Coding Style & Naming Conventions
Follow standard Rust style with `rustfmt` defaults: 4-space indentation, trailing commas where helpful, and grouped `use` imports. Use `snake_case` for functions, modules, and files, `PascalCase` for structs/enums, and descriptive CLI flag names such as `force_after_timeout`. Keep modules focused; add new functionality to an existing file only when it matches that module’s responsibility.

## Testing Guidelines
Prefer unit tests near the implementation with `#[cfg(test)]` for pure logic, and place CLI or process-spawning checks in `tests/`. Name tests after observable behavior, such as `help_mentions_timeout_options` or `fuzzy_filter_matches_app_name_and_ports`. Run `cargo test` before every commit; add or update tests whenever parser behavior, matching logic, or interactive state handling changes.

## Commit & Pull Request Guidelines
Recent commits use short, imperative subjects like `Add sort modes and detail view for interactive interface`. Keep that style: start with a verb, describe one logical change, and avoid bundling unrelated edits. PRs should include a concise summary, linked issue if applicable, and terminal screenshots or notes when changing interactive UI behavior or help output.
