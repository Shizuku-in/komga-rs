# komga-rs

An incremental Rust port of the [Komga](https://github.com/gotson/komga) backend. The Kotlin source checked out in `komga/` is the behavioral reference; the Rust implementation is a separate workspace at the repository root.

> The core headless backend is usable, but this is not yet a drop-in replacement for a complete Komga server. Users and permissions, library scanning, content browsing and reading, progress, collections and read lists, search, media delivery, metadata, and persistent background tasks are implemented. See [the migration matrix](docs/MIGRATION.md) for exact coverage.

## Port status

The migration matrix currently tracks 13 functional areas: 2 are implemented or core-complete, 8 are partially implemented, and 3 are pending.

| Status | Areas |
| --- | --- |
| Implemented / core-complete | Claim and users; collections and read lists |
| Partial | Process and configuration; SQLite; authentication; libraries and scanner; Series, Books, and progress; media; search; operations |
| Pending | OPDS; device sync; Web UI packaging |

Recent content work added modern and deprecated Series/Book queries, legacy regex search, duplicate-book discovery, book-file hashing, configurable ZIP edge-page hashing, PageHash administration, and safe duplicate-page removal from CBZ files. OPDS, device synchronization, and content-domain event parity remain later work.

## Run

Rust 1.96.1 or newer is required.

```bash
cargo run --release -p komga-server
```

The defaults match Komga:

- listen on `0.0.0.0:25600`;
- keep state in `$HOME/.komga`;
- use `$HOME/.komga/database.sqlite`.
- use `$HOME/.komga/tasks.sqlite` for the persistent task queue.
- hash the first and last 3 pages of eligible ZIP books when a library enables page hashing.

Configuration can be supplied through arguments or environment variables:

```bash
cargo run -p komga-server -- \
  --config-dir /srv/komga \
  --page-hashing 3 \
  --host 127.0.0.1 \
  --port 25600

KOMGA_DATABASE_FILE=/srv/komga/database.sqlite \
KOMGA_TASKS_DATABASE_FILE=/srv/komga/tasks.sqlite \
KOMGA_SEARCH_DIRECTORY=/srv/komga/search KOMGA_PAGE_HASHING=3 KOMGA_PORT=25601 \
  cargo run -p komga-server
```

Do not run the Kotlin and Rust servers against the same SQLite file at the same time. Back up an existing database before the first Rust start.
Migration `0011_page_hashing` deliberately clears existing `PAGE_HASH` rules and thumbnails and resets JPEG page hashes because Rust uses a versioned RGB24 pixel hash instead of Java ImageIO's encoder-dependent bytes.

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
- `POST /api/v1/series/list`
- `POST /api/v1/series/list/alphabetical-groups`
- `GET /api/v1/series` and `GET /api/v1/series/alphabetical-groups`
- `GET /api/v1/series/{latest|new|updated}`
- `GET /api/v1/series/{id}`
- `POST /api/v1/books/list`
- `GET /api/v1/books`
- `GET /api/v1/books/{latest|ondeck}`
- `GET /api/v1/books/{id}`
- `GET /api/v1/books/{id}/previous`
- `GET /api/v1/books/{id}/next`
- `POST /api/v1/books/{id}/analyze`
- `GET /api/v1/books/{id}/pages`
- `GET /api/v1/books/{id}/pages/{number}`
- `GET /api/v1/books/{id}/pages/{number}/raw`
- `GET /api/v1/books/{id}/pages/{number}/thumbnail`
- `GET /api/v1/books/{id}/thumbnail`
- `GET /api/v1/books/{id}/positions`
- `GET /api/v1/books/{id}/resource/{resource}`
- `GET /api/v1/books/{id}/manifest`
- `GET /api/v1/books/{id}/manifest/{divina|pdf|epub}`
- `GET|PUT /api/v1/page-hashes`
- `GET /api/v1/page-hashes/unknown`
- `GET /api/v1/page-hashes/{hash}`
- `GET /api/v1/page-hashes/{hash}/thumbnail`
- `GET /api/v1/page-hashes/unknown/{hash}/thumbnail`
- `POST /api/v1/page-hashes/{hash}/{delete-all|delete-match}`
- `POST /api/v1/readlists/match/comicrack`
- `GET /api/v1/readlists/{id}/file`
- `POST /api/v1/filesystem`
- `GET /api/v1/client-settings/global/list`
- `PATCH|DELETE /api/v1/client-settings/global`
- `GET /api/v1/client-settings/user/list`
- `PATCH|DELETE /api/v1/client-settings/user`
- `GET /api/v1/oauth2/providers`
- `GET /api/v1|v2/authors` and `GET /api/v1/authors/{names|roles}`
- `GET /api/v1/{genres|sharing-labels|languages|publishers|age-ratings}`
- `GET /api/v1/tags`, `/api/v1/tags/{book|series}`, and `/api/v1/series/release-dates`
- `DELETE /api/v1/tasks`
- `GET /sse/v1/events`

Authentication is compatible with the current Komga contract: HTTP Basic, `KOMGA-SESSION`, `X-Auth-Token`, and SHA-512-backed `X-API-Key`. Existing Spring BCrypt `$2a$` password hashes are accepted.

`ScanLibrary` tasks are executed in the background. They discover configured CBX/PDF/EPUB files and persist Kotlin-compatible Series, Book, Media, and initial metadata rows. CBZ, CBR, PDF, and EPUB contents are analyzed and receive generated book thumbnails; fixed-layout EPUB pages, publication resources, navigation, reading positions, and Divina/PDF/EPUB WebPub manifests are available through the REST API. Root-level `ComicInfo.xml`, EPUB OPF, and Mylar `series.json` metadata are imported incrementally after analysis and through the book, series, and library metadata refresh endpoints. Provider updates preserve locked fields, and ComicInfo collection/read-list relationships remain incremental. `EmptyTrash` permanently removes soft-deleted database content, honors automatic cleanup after scans, and never deletes source files. Library deletion cascades through database-owned content and sidecars while also preserving source files. Other unported task types remain queued and unmodified.

Libraries with `hashPages` enabled enqueue Kotlin-compatible discovery and page-hashing tasks at startup, after scans, and when the setting is enabled. Only READY ZIP/CBZ media participates. The first and last configured number of pages are hashed; JPEG pages use a deterministic dimensions-plus-RGB24 representation, while other page types retain raw-byte XXH3-128 hashing. Administrators can classify hashes as `DELETE_AUTO`, `DELETE_MANUAL`, or `IGNORE`, inspect matching pages and previews, and queue all or individual matches for deletion. ZIP removal raw-copies surviving entries in archive order, validates the complete temporary archive and current source snapshot, and then performs a same-directory atomic replacement. It does not create a source backup, so back up real libraries before enabling deletion rules.

The modern Series and Book list endpoints support Spring-style pagination, the complete structured condition trees, ranked multilingual `fullTextSearch`, alphabetical groups, fixed discovery feeds, and On Deck selection. Deprecated GET lists and the legacy alphabetical-groups route translate their query parameters into the same condition trees and search candidates; Series `search_regex` supports case-insensitive Rust regexes over `title` and `title_sort`. Referential filter values preserve Kotlin's scope precedence and data-source differences. Collection and read-list `search` uses the same in-process Tantivy index. Library sharing, age, sharing labels, member restrictions, and structured conditions remain enforced by SQLite before pagination. The Rust index uses `<config-dir>/search/v2`, remains independent of Kotlin's Lucene directory, and queues a priority-8 rebuild task at startup when the file index is empty or incompatible.

## Develop

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

The workspace is split by responsibility:

- `komga-core`: framework-free domain types and policy;
- `komga-media`: filesystem media analysis, page extraction, and image processing;
- `komga-storage`: SQLite schema compatibility and repositories;
- `komga-server`: Axum routes, authentication, configuration, and process lifecycle.
