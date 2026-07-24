# komga-rs

An incremental Rust port of the [Komga](https://github.com/gotson/komga) backend. The Kotlin source checked out in `komga/` is the behavioral reference; the Rust implementation is a separate workspace at the repository root.

> This is not yet a drop-in replacement for a complete Komga server. The implemented slices cover startup, SQLite, server claiming, sessions, API keys, users, library configuration, client settings, persistent task processing, and basic filesystem scanning. See [the migration matrix](docs/MIGRATION.md) for exact coverage.

## Run

Rust 1.88 or newer is required.

```bash
cargo run --release -p komga-server
```

The defaults match Komga:

- listen on `0.0.0.0:25600`;
- keep state in `$HOME/.komga`;
- use `$HOME/.komga/database.sqlite`.
- use `$HOME/.komga/tasks.sqlite` for the persistent task queue.

Configuration can be supplied through arguments or environment variables:

```bash
cargo run -p komga-server -- \
  --config-dir /srv/komga \
  --host 127.0.0.1 \
  --port 25600

KOMGA_DATABASE_FILE=/srv/komga/database.sqlite \
KOMGA_TASKS_DATABASE_FILE=/srv/komga/tasks.sqlite KOMGA_PORT=25601 \
  cargo run -p komga-server
```

Do not run the Kotlin and Rust servers against the same SQLite file at the same time. Back up an existing database before the first Rust start.

## Implemented API

- `GET /actuator/health`
- `GET|POST /api/v1/claim`
- `GET /api/v2/users/me`
- `PATCH /api/v2/users/me/password`
- `GET|POST /api/v2/users`
- `PATCH|DELETE /api/v2/users/{id}`
- `PATCH /api/v2/users/{id}/password`
- `GET|POST /api/v2/users/me/api-keys`
- `DELETE /api/v2/users/me/api-keys/{id}`
- `GET /api/v1/login/set-cookie`
- `GET|POST /api/logout`
- `GET|POST /api/v1/libraries`
- `GET|PUT|PATCH|DELETE /api/v1/libraries/{id}`
- `POST /api/v1/libraries/{id}/scan`
- `POST /api/v1/libraries/{id}/empty-trash`
- `POST /api/v1/filesystem`
- `GET /api/v1/client-settings/global/list`
- `PATCH|DELETE /api/v1/client-settings/global`
- `GET /api/v1/client-settings/user/list`
- `PATCH|DELETE /api/v1/client-settings/user`
- `GET /api/v1/oauth2/providers`
- `DELETE /api/v1/tasks`
- `GET /sse/v1/events`

Authentication is compatible with the current Komga contract: HTTP Basic, `KOMGA-SESSION`, `X-Auth-Token`, and SHA-512-backed `X-API-Key`. Existing Spring BCrypt `$2a$` password hashes are accepted.

`ScanLibrary` tasks are executed in the background. They discover configured CBX/PDF/EPUB files and persist Kotlin-compatible Series, Book, Media, and initial metadata rows. Archive contents, pages, metadata providers, and thumbnails are not analyzed yet. `EmptyTrash` and other unported task types remain queued and unmodified. Library deletion is currently limited to empty libraries; deleting a library that still contains series, books, or sidecars returns `409 Conflict`.

## Develop

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

The workspace is split by responsibility:

- `komga-core`: framework-free domain types and policy;
- `komga-storage`: SQLite schema compatibility and repositories;
- `komga-server`: Axum routes, authentication, configuration, and process lifecycle.
