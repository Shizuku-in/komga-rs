[![Rust CI](https://github.com/Shizuku-in/komga-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/Shizuku-in/komga-rs/actions/workflows/ci.yml)
[![GitHub release](https://img.shields.io/github/v/release/Shizuku-in/komga-rs?color=blue)](https://github.com/Shizuku-in/komga-rs/releases)
[![License](https://img.shields.io/github/license/Shizuku-in/komga-rs)](LICENSE)

# ![app icon](./.github/readme-images/app-icon.png) Komga-rs

Komga-rs is an unofficial Rust port of [Komga](https://github.com/gotson/komga), a media server for comics, manga, BDs, magazines, and eBooks.

The server includes the Komga Web UI and targets the behavior and SQLite schema of the pinned Kotlin baseline. See the [migration status](docs/MIGRATION.md) for the exact compatibility boundary.

## Features

- Responsive Web UI for desktop, tablet, and mobile
- Library scanning, metadata import and editing, collections, and read lists
- CBZ/CBR, PDF, fixed-layout EPUB, and reflowable EPUB reading
- Multiple users with library, age, and sharing-label restrictions
- Reading progress, downloads, book import, and duplicate management
- REST API, OAuth2/OIDC, OPDS 1.2 and OPDS 2
- Kobo and KOReader synchronization
- Persistent background tasks, search, metrics, and event streaming

## Installation

### Docker Compose

Create writable configuration and library directories, then start the published image:

```bash
mkdir -p config books
export KOMGA_IMAGE=ghcr.io/shizuku-in/komga-rs:latest
docker compose pull
docker compose up -d --no-build
```

Open <http://localhost:25600>. By default, Compose mounts `./config` at `/config` and `./books` at `/data`. Set durable absolute paths in `.env` for a server deployment:

```dotenv
KOMGA_CONFIG_HOST_DIR=/srv/komga/config
KOMGA_LIBRARY_DIR=/srv/books
KOMGA_HTTP_PORT=25600
```

The mounted directories must be accessible to UID/GID 1000. The library mount is writable because import, conversion, repair, source deletion, and duplicate-page removal can modify files.

### From Source

Rust 1.96.1 or newer and Node.js 22.11 ~ 24 are required. Cargo builds and embeds the Web UI automatically.

```bash
cargo run --release -p komga-server -- --config-dir ./config
```

Run `cargo run -p komga-server -- --help` for configuration options. Settings can be supplied through command-line arguments, `KOMGA_*` environment variables, database-backed server settings, and an optional `<config-dir>/oauth2.json`.

CORS is disabled by default. To permit a separate browser origin, set an exact comma-separated allowlist with `--cors-allowed-origins` or `KOMGA_CORS_ALLOWED_ORIGINS`, for example `https://reader.example,http://localhost:5173`.

Forwarding headers are ignored unless the immediate TCP peer is trusted. When Komga-rs runs behind a reverse proxy, set `--trusted-proxies` or `KOMGA_TRUSTED_PROXIES` to the proxy IP or CIDR network, for example `172.18.0.0/16,127.0.0.1/32`. Configure the narrowest network that contains the proxy; do not trust every Internet address. OAuth deployments should also set `publicBaseUrl` in `oauth2.json`.

## Migrating From Komga

Compatibility is scoped to the pinned Kotlin baseline and is not a universal migration path for every historical Komga database. Before switching:

- upgrade the Kotlin installation to a documented supported schema;
- back up `database.sqlite`, `tasks.sqlite`, and the library;
- translate Spring Boot configuration to the supported CLI or `KOMGA_*` settings;
- stop Kotlin before opening the databases with komga-rs.

Never run Kotlin Komga and komga-rs against the same SQLite files at the same time. The Rust search index is independent and will be rebuilt from SQLite. Read [MIGRATION.md](docs/MIGRATION.md) before migrating an existing installation.

## Development

Initialize the pinned Kotlin behavioral reference after cloning:

```bash
git submodule update --init komga
```

Start a development server:

```bash
cargo run -p komga-server -- --config-dir /tmp/komga-rs
```

Run the Rust checks:

```bash
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Run the Web UI checks:

```bash
cd webui
npm ci
npm run lint
npm run test:unit -- --runInBand
npm run build
```

Browser tests additionally require Playwright Chromium:

```bash
cd webui
npx playwright install chromium
npm run test:e2e
```

Production code lives in the root Rust workspace and `webui/`; the `komga/` submodule is the behavioral reference. See [AGENTS.md](AGENTS.md) for repository conventions.

## Documentation

- [Migration status and limitations](docs/MIGRATION.md)
- [API compatibility surface](docs/API_PARITY.md)
- [Machine-readable API manifest](docs/api-parity.json)
- [Upstream Komga documentation](https://komga.org)

## License

[MIT](LICENSE)
