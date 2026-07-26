# Repository Guidelines

## Project Structure & Module Organization

This Rust 2024 workspace is an incremental port of Komga. The local `komga/` Kotlin checkout is the behavioral reference; implement production changes in the root workspace.

- `crates/komga-core`: framework-independent domain models and policies.
- `crates/komga-media`: archive, PDF, EPUB, metadata, and image processing.
- `crates/komga-search`: Tantivy indexing and multilingual search.
- `crates/komga-storage`: SQLite repositories and migrations.
- `crates/komga-server`: Axum routes, authentication, configuration, and workers.
- `crates/komga-server/tests`: HTTP and runtime integration tests.
- `crates/komga-storage/tests/fixtures`: Kotlin-compatible database fixtures.
- `webui`: maintained Vue 2 Web UI source imported from the pinned Komga baseline.
- `docs/MIGRATION.md`: implemented compatibility and known gaps.

## Build, Test, and Development Commands

Rust 1.96.1 or newer is required.

```bash
cargo build --workspace
cargo run -p komga-server -- --config-dir /tmp/komga-rs
cargo test --workspace --all-targets
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Use `cargo test -p komga-storage <test_name>` for focused iteration. Run the full format, Clippy, and test commands before submitting changes.

## Coding Style & Naming Conventions

Follow `rustfmt.toml` and standard Rust conventions: four-space indentation, `snake_case` modules/functions/tests, `UpperCamelCase` types and traits, and `SCREAMING_SNAKE_CASE` constants. Keep domain policy in `komga-core`; keep SQL in storage and HTTP concerns in server modules. Prefer typed parsing and structured SQL builders over ad hoc string handling. Public APIs require concise rustdoc, including meaningful error behavior.

## Testing Guidelines

Use built-in `#[test]` for synchronous units and `#[tokio::test]` for async storage/server behavior. Place unit tests beside their implementation and API scenarios under `crates/komga-server/tests/api_cases/`. Name tests as behavioral statements, for example `book_file_download_enforces_role_library_age_and_label_restrictions`. Compatibility work should include Kotlin fixture or HTTP contract coverage; no fixed coverage percentage is required.

## Commit & Pull Request Guidelines

History follows Conventional Commits with optional scopes: `feat(content): ...`, `fix(storage): ...`, `refactor: ...`. Keep commits focused and independently passing. Pull requests should describe behavior and compatibility impact, list verification commands, link relevant issues, and update `README.md` or `docs/MIGRATION.md` when support changes. Include screenshots only for user-visible UI changes.

## Security & Configuration

Never commit credentials, API keys, databases, or generated search indexes. Do not run Kotlin and Rust servers against the same SQLite files concurrently, and back up existing databases before compatibility testing.
