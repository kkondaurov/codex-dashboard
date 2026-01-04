# Repository Guidelines

This repository hosts a Rust-based local TUI that tracks OpenAI / Codex usage. Treat it as a normal Cargo project.

## Project Structure & Module Organization

- Application code lives in `src/`. Prefer small focused modules (e.g. `src/ingest.rs`, `src/tui.rs`, `src/config.rs`, `src/storage.rs`).
- Tests live next to code (`mod tests` in the same file) or in `tests/` for integration tests.
- Configuration and example files (e.g. `codex-usage.toml`) belong at the repository root.

## Build, Test, and Development Commands

- `cargo build` – Compile the project.
- `cargo run` – Run the main binary (ingestor + TUI).
- `cargo test` – Run the full test suite.
- Use `RUST_LOG=debug cargo run` when debugging behavior.

## Coding Style & Naming Conventions

- Follow idiomatic Rust style: 4-space indentation, `snake_case` for functions and modules, `CamelCase` for types.
- Keep functions short and focused; prefer small structs over large blobs of state.
- Run `cargo fmt` before committing; do not hand-format differently from `rustfmt`.
- Use `cargo clippy` to catch common issues when making larger changes.

## Testing Guidelines

- Prefer fast, deterministic tests. Unit tests should not hit real OpenAI endpoints.
- For code that talks to the network, introduce traits and test with fakes/mocks.
- Name tests descriptively (`test_calculates_daily_costs`, not `test1`).
- Ensure `cargo test` passes before reporting back to the user.

## Your workflow
- Before implementing anything, propose a plan, and get a good-to-go from the user.
- Always consider test coverage of the planned, and add/update appropriate tests as part of the implementation.
- When you're done with the implementation, rerun the test suite, and iteratively fix the problem flagged by failed tests.
- Run `cargo check`, `cargo fmt`, and `cargo clippy` after finishing the implementation, and before reporting back to the user,
  and iteratively fix the issues identified by these commands; don't forget to rerun the test suite.

## Commit & Pull Request Guidelines

- Use clear, imperative commit messages: `Add daily aggregate storage`, `Fix TUI refresh bug`.
- Use multi-line commit messages with concise description of the changes. Keep the first line brief but clear.
- Commit messages should describe the change, mention user-visible behavior, and note any new config or migrations.
- Escalate the permissions to do the commit, never attempt to do create or delete git lock file.
- Only commit if and when explicitly instructed by the user.

## Security & Configuration Tips

- Keep pricing and other secrets in config files or environment variables, not hard-coded into logic.
