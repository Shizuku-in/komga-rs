# komga-rs

An incremental Rust port of the [Komga](https://github.com/gotson/komga) backend. The Kotlin source pinned as the `komga/` Git submodule is the behavioral and Web UI reference; the Rust implementation is a separate workspace at the repository root.

> The core headless backend is usable, but this is not yet a drop-in replacement for a complete Komga server. Users and permissions, library scanning, content browsing and reading, progress, collections and read lists, search, media delivery, metadata, and persistent background tasks are implemented. See [the migration matrix](docs/MIGRATION.md) for exact coverage.

## Port status

The migration matrix currently tracks 13 functional areas: 5 are implemented or core-complete, 7 are partially implemented, and 1 is pending.

| Status | Areas |
| --- | --- |
| Implemented / core-complete | Claim and users; collections and read lists; OPDS; device sync; operations |
| Partial | Process and configuration; SQLite; authentication; libraries and scanner; Series, Books, and progress; media; search |
| Pending | Web UI packaging |

Recent content work added modern and deprecated Series/Book queries, legacy regex search, duplicate-book discovery, book-file hashing, configurable ZIP edge-page hashing, PageHash administration, safe duplicate-page removal from CBZ files with historical auditing, RAR/CBR-to-CBZ conversion, source-extension repair, authenticated OPDS 1.2 and OPDS 2 catalogs, Kotlin-compatible content-domain SSE, KOReader and Kobo synchronization, application metrics, announcements, release checks, and bundled and local reader fonts.

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
- load additional reader fonts from `$HOME/.komga/fonts`, alongside bundled OpenDyslexic files.
- hash the first and last 3 pages of eligible ZIP books when a library enables page hashing.
- return at most 100 source books or tags in each Kobo synchronization page.
- enable in-process KEPUB conversion, with the external fallback and Kobo Store proxy disabled until configured.

Configuration can be supplied through arguments or environment variables:

```bash
cargo run -p komga-server -- \
  --config-dir /srv/komga \
  --fonts-directory /srv/komga/fonts \
  --page-hashing 3 \
  --kobo-sync-item-limit 100 \
  --kobo-kepubify-path /usr/local/bin/kepubify \
  --kobo-proxy \
  --host 127.0.0.1 \
  --port 25600 \
  --context-path /komga

KOMGA_DATABASE_FILE=/srv/komga/database.sqlite \
KOMGA_TASKS_DATABASE_FILE=/srv/komga/tasks.sqlite \
KOMGA_SEARCH_DIRECTORY=/srv/komga/search \
KOMGA_FONTS_DIRECTORY=/srv/komga/fonts KOMGA_PAGE_HASHING=3 \
KOMGA_KOBO_SYNC_ITEM_LIMIT=100 \
KOMGA_KOBO_KEPUBIFY_PATH=/usr/local/bin/kepubify KOMGA_KOBO_PROXY=true \
KOMGA_PORT=25601 KOMGA_CONTEXT_PATH=/komga \
  cargo run -p komga-server
```

`--kobo-kepubify-path` and `KOMGA_KOBO_KEPUBIFY_PATH` are optional compatibility fallbacks. The
in-process converter remains the primary implementation when either setting is present.

Do not run the Kotlin and Rust servers against the same SQLite file at the same time. Back up an existing database before the first Rust start.
Migration `0011_page_hashing` deliberately clears existing `PAGE_HASH` rules and thumbnails and resets JPEG page hashes because Rust uses a versioned RGB24 pixel hash instead of Java ImageIO's encoder-dependent bytes.

## Implemented API

- `GET /actuator/health`
- `GET /actuator/metrics`
- `GET /actuator/metrics/{metric}`
- `GET|POST /api/v1/claim`
- `GET /api/v2/users/me`
- `PATCH /api/v2/users/me/password`
- `GET|POST /api/v2/users`
- `PATCH|DELETE /api/v2/users/{id}`
- `PATCH /api/v2/users/{id}/password`
- `GET|POST /api/v2/users/me/api-keys`
- `DELETE /api/v2/users/me/api-keys/{id}`
- `DELETE /api/v1/syncpoints/me`
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
- `DELETE /api/v1/series/{id}/file`
- `POST /api/v1/books/list`
- `POST /api/v1/books/import`
- `GET /api/v1/books`
- `GET /api/v1/books/{latest|ondeck}`
- `GET /api/v1/books/{id}`
- `GET|DELETE /api/v1/books/{id}/file`
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
- `GET /api/v1/history`
- `POST /koreader/users/create`
- `GET /koreader/users/auth`
- `GET /koreader/syncs/progress/{bookHash}`
- `PUT /koreader/syncs/progress`
- `GET /kobo/{apiKey}/ping`
- `GET /kobo/{apiKey}/v1/initialization`
- `POST /kobo/{apiKey}/v1/auth/device`
- `GET /kobo/{apiKey}/v1/library/sync`
- `GET /kobo/{apiKey}/v1/library/{bookId}/metadata`
- `GET|PUT /kobo/{apiKey}/v1/library/{bookId}/state`
- `GET /kobo/{apiKey}/v1/books/{bookId}/file/epub`
- `GET /kobo/{apiKey}/v1/books/{thumbnailId}/thumbnail/.../image.jpg`
- `GET|PUT|POST|DELETE|PATCH /kobo/{apiKey}/{*path}` when Store proxying is enabled
- `POST /api/v1/readlists/match/comicrack`
- `GET /api/v1/readlists/{id}/file`
- `POST /api/v1/filesystem`
- `GET /api/v1/client-settings/global/list`
- `PATCH|DELETE /api/v1/client-settings/global`
- `GET /api/v1/client-settings/user/list`
- `PATCH|DELETE /api/v1/client-settings/user`
- `GET /api/v1/oauth2/providers`
- `GET|PUT /api/v1/announcements`
- `GET /api/v1/releases`
- `GET /api/v1/fonts/families`
- `GET /api/v1/fonts/resource/{family}/css`
- `GET /api/v1/fonts/resource/{family}/{file}`
- `GET /opds/v1.2/{catalog|search|ondeck|keep-reading|series|books/latest|libraries|collections|readlists|publishers}` and their detail/resource routes
- `GET /opds/v2/auth`
- `GET /opds/v2/{catalog|libraries|search|collections/{id}|readlists/{id}|series/{id}}`
- `GET /opds/v2/libraries[/{id}]/{keep-reading|on-deck|books/latest|series/latest|browse|collections|readlists}`
- `GET /opds/v2/books/{id}/{manifest|manifest/{divina|pdf|epub}|thumbnail|pages/{number}|pages/{number}/raw|file|resource/{resource}|progression}`
- `PUT /opds/v2/books/{id}/progression`
- `GET /api/v1|v2/authors` and `GET /api/v1/authors/{names|roles}`
- `GET /api/v1/{genres|sharing-labels|languages|publishers|age-ratings}`
- `GET /api/v1/tags`, `/api/v1/tags/{book|series}`, and `/api/v1/series/release-dates`
- `DELETE /api/v1/tasks`
- `GET|PATCH /api/v1/settings`
- `GET /sse/v1/events`

Authentication is compatible with the current Komga contract: HTTP Basic, `KOMGA-SESSION`, `X-Auth-Token`, and SHA-512-backed `X-API-Key`. Existing Spring BCrypt `$2a$` password hashes are accepted. KOReader routes exclusively accept a plaintext API key in `X-Auth-User` and require the `KOREADER_SYNC` role. Kobo routes exclusively accept a plaintext API key in the `/kobo/{apiKey}` path and require `KOBO_SYNC`; original-file delivery also requires `FILE_DOWNLOAD`. Kobo keys are redacted from request tracing. OPDS 2 also exposes the public authentication document and returns its media type, Basic challenge, and discovery link on authentication failures.

Administrators can retrieve and patch Kotlin-compatible server settings through `/api/v1/settings`. Empty-container cleanup, thumbnail size, task-pool size, remember-me policy, Kobo Store proxying/public URL port, and external `kepubify` fallback selection update live after a committed PATCH. `serverPort` and `serverContextPath` are persisted as database overrides and take effect on the next process start; the response reports configuration, database, and current effective values separately. Database values override CLI/environment values, and clearing nullable overrides restores the configured fallback.

Administrators can inspect nine Komga application metrics through the actuator endpoints. Content gauges query SQLite on each request; task execution and failure measurements are process-local and reset after restart. Announcement and release endpoints read the fixed Komga feed and GitHub release API, cache successful upstream responses for one idle hour, and never cache failures. Announcement read state is persisted per user. These outbound requests are read-only but disclose the server's network address and request metadata to Komga's website or GitHub.

OpenDyslexic WOFF and WOFF2 files are bundled with the server. Additional font families are discovered from immediate subdirectories of the configured fonts directory at startup; a local family with the same name replaces its bundled counterpart. Font catalogs require authentication, while CSS and font resources are public so the EPUB reader can load them without forwarding credentials. Symlinked directories and files, unsupported extensions, nested files, and paths outside the configured root are rejected.

`ScanLibrary` tasks are executed in the background. They discover configured CBX/PDF/EPUB files and persist Kotlin-compatible Series, Book, Media, and initial metadata rows. CBZ, CBR, PDF, and EPUB contents are analyzed and receive generated book thumbnails; fixed-layout EPUB pages, publication resources, navigation, reading positions, and Divina/PDF/EPUB WebPub manifests are available through the REST API. Root-level `ComicInfo.xml`, EPUB OPF, Mylar `series.json`, adjacent Book artwork, and Series artwork sidecars are imported incrementally. Provider updates preserve locked fields, and ComicInfo collection/read-list relationships remain incremental. Libraries with `convertToCbz` enqueue persistent RAR4/RAR5 conversion tasks after scans and when the setting is enabled; `repairExtensions` likewise queues repairs for analyzed CBR/CBZ/PDF/EPUB files. `EmptyTrash` permanently removes soft-deleted database content, honors automatic cleanup after scans, and never deletes source files. Library deletion cascades through database-owned content and sidecars while also preserving source files. Administrators can enqueue deduplicated Kotlin-compatible Book/Series source deletion and batch Book import tasks. Imports support COPY, MOVE, and HARDLINK with copy fallback, destination renaming, one-shot upgrades, sidecars, relation transfer, `BookImported`/upgrade deletion history, analysis follow-up, SSE, and search synchronization. Other unported task types remain queued and unmodified.

Source-file deletion is permanent: no backup, trash copy, or recovery archive is created. The worker deletes only database-registered Book files and `SIDECAR` thumbnails, never unknown directory contents. Before the first unlink it canonicalizes the Library root and every involved existing path; a non-local URL or any real path outside that root rejects the complete task. The Library root directory itself is always preserved, including when its last root-level Book is deleted. Back up real libraries before using these endpoints.

Book imports accept only non-symlink regular source files outside every configured Library. Target directories, upgrade files, and existing sidecars are canonicalized inside the destination Library before any change. The worker stages targets beside their final paths and keeps temporary backups until the main-database transaction commits; validation and database failures restore MOVE sources and replaced upgrade files on a best-effort basis. A committed MOVE is permanent and no long-term backup or trash copy is retained.

RAR/CBR conversion streams only analyzed pages and ancillary files into a validated CBZ, then records `BookFileDeleted` and `BookConverted` in the same database transaction that updates the Book and Media rows. Conversion and extension repair reject non-local, symlinked, stale, out-of-Library, or conflicting paths before replacing the source. A same-directory backup or repair marker supports restart recovery and transaction rollback, but is removed after commit; the original RAR/CBR is then permanently gone. Extension repair preserves `.epub` files whose analyzer classifies them as ordinary ZIP archives instead of renaming them to `.cbz`.

Libraries with `hashKoreader` enabled enqueue Kotlin-compatible `HashBookKoreader` tasks after scans, when the setting is enabled, and after ZIP page removal. The worker computes KOReader's partial MD5 against a source snapshot and conditionally persists it to `FILE_HASH_KOREADER`. KOReader clients can then authenticate, pull, and push Divina/PDF page progress or EPUB resource progress. Hash resolution applies the user's library, age, and sharing-label restrictions before reporting collisions.

Kobo synchronization captures Kotlin-compatible SyncPoint snapshots of each user's visible READY EPUB books and virtual On Deck tag. First and incremental synchronization are transactionally paged with Komga, Calibre Web, and official dotted sync-token compatibility. Metadata, reading state, EPUB/native KEPUB files, and JPEG covers are delivered through path-authenticated routes. Ordinary reflowable EPUBs always advertise and stream an in-process converted KEPUB from a five-minute idle cache; a configured `kepubify` executable is used only after a compatible content-conversion failure, never to bypass archive safety or resource limits. Native KEPUB and fixed-layout EPUB delivery remains unchanged. Analysis records KoboSpan positions for new books, while startup submits low-priority analysis for eligible historical EPUBs which have no KoboSpan positions. Converted files remain process-local temporary files and never replace Library sources. Enabling `KOMGA_KOBO_PROXY` or the corresponding database setting forwards only the official authorization/content headers and `x-kobo-*` protocol headers to the fixed Kobo Store endpoint, merges official sync pages after local pages, and redirects unknown covers to Kobo's CDN. The `koboPort` server setting corrects device-visible absolute URLs when no forwarded headers are present; it does not create another listener. Store proxying may disclose device and Kobo account tokens to Kobo; it never forwards the local path API key. Unknown routes still return an empty JSON object while proxying is disabled.

Libraries with `hashPages` enabled enqueue Kotlin-compatible discovery and page-hashing tasks at startup, after scans, and when the setting is enabled. Only READY ZIP/CBZ media participates. The first and last configured number of pages are hashed; JPEG pages use a deterministic dimensions-plus-RGB24 representation, while other page types retain raw-byte XXH3-128 hashing. Administrators can classify hashes as `DELETE_AUTO`, `DELETE_MANUAL`, or `IGNORE`, inspect matching pages and previews, and queue all or individual matches for deletion. ZIP removal raw-copies surviving entries in archive order, validates the complete temporary archive and current source snapshot, and then performs a same-directory atomic replacement. Each removed page creates a `DuplicatePageDeleted` historical event in the same database transaction. It does not create a source backup, so back up real libraries before enabling deletion rules.

The authenticated SSE endpoint combines the administrator task-count heartbeat with Kotlin event names and payloads for durable library, series, book, collection, read-list, progress, thumbnail, and session changes. Progress and session events are restricted to the affected user; import results are administrator-only. Search synchronization remains explicit and recoverable rather than depending on best-effort SSE delivery.

The modern Series and Book list endpoints support Spring-style pagination, the complete structured condition trees, ranked multilingual `fullTextSearch`, alphabetical groups, fixed discovery feeds, and On Deck selection. Deprecated GET lists and the legacy alphabetical-groups route translate their query parameters into the same condition trees and search candidates; Series `search_regex` supports case-insensitive Rust regexes over `title` and `title_sort`. Referential filter values preserve Kotlin's scope precedence and data-source differences. Collection and read-list `search` uses the same in-process Tantivy index. Library sharing, age, sharing labels, member restrictions, and structured conditions remain enforced by SQLite before pagination. The Rust index uses `<config-dir>/search/v2`, remains independent of Kotlin's Lucene directory, and queues a priority-8 rebuild task at startup when the file index is empty or incompatible.

OPDS 1.2 serves Atom navigation and acquisition feeds, OpenSearch discovery, PSE page streaming, thumbnails, and original files. OPDS 2 serves recommended and paged library feeds, browse facets, grouped search, publications, profile manifests, reading resources, and Readium progression. Both generations enforce the same library and content restrictions as REST, generate proxy-aware absolute links, and reuse the existing media and search implementations.

## Develop

Initialize the pinned Kotlin and Web UI reference after cloning:

```bash
git submodule update --init komga
```

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
