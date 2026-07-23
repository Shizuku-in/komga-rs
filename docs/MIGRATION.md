# Migration status

The reference baseline is Komga commit `4ef86df`. Porting is organized as vertical slices so every completed stage can start, persist data, and pass HTTP-level tests.

| Area | Status | Notes |
| --- | --- | --- |
| Process/configuration | Partial | Default config directory, database file, address, graceful shutdown, and tracing are implemented. Spring YAML compatibility is pending. |
| SQLite | Partial | The user-related current schema is compatible with existing Flyway databases. The complete schema and all historical migration paths are pending. |
| Claim and users | Implemented | Claim, current user, CRUD, roles, library sharing fields, age/label restrictions, and password changes. |
| Authentication | Partial | Basic, in-memory sessions, remember-me cookie, header sessions, logout, and REST API keys are implemented. OAuth2 and authentication activity are pending. |
| Libraries and scanner | Pending | Library CRUD, filesystem browsing, scanning, analysis, trash, sidecars, and metadata providers. |
| Series/books/read progress | Pending | Search, sorting, pagination, metadata, thumbnails, pages, downloads, and progress. |
| Collections/read lists | Pending | CRUD, matching, thumbnails, generated archives, and progress aggregation. |
| Media | Pending | ZIP/RAR/PDF/EPUB detection and extraction, image conversion, hashing, and duplicate pages. |
| Search | Pending | Lucene-equivalent multilingual indexing and lifecycle. |
| OPDS | Pending | OPDS 1 XML and OPDS 2 JSON catalogs. |
| Device sync | Pending | Kobo, KOReader, sync points, and KEPUB conversion. |
| Operations | Pending | Task queue, schedulers, SSE, metrics, announcements, settings, fonts, and release checks. |
| Web UI packaging | Pending | Static asset build/embedding and SPA fallback. |

## Compatibility rules

1. Preserve public paths, status codes, JSON field names, headers, and media types before optimizing internals.
2. Read the current Kotlin schema directly. Rust migrations must be additive and safe when Flyway tables already exist.
3. Keep domain policy independent from Axum and SQLx so Kotlin unit cases can be ported without HTTP or database setup.
4. Add HTTP contract tests for each controller before marking a row implemented.
5. Never require an irreversible database conversion until full feature parity is available.

## Next slice

The next dependency-ordered slice is library management:

1. snapshot the final `LIBRARY` and server/client settings schema;
2. port `Library`, scan options, repository queries, and content restriction filtering;
3. implement library CRUD and filesystem validation;
4. introduce the persistent task queue and scanner boundary;
5. add compatibility tests against a Kotlin-created database fixture.
