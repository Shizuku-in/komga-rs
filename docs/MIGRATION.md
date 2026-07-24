# Migration status

The reference baseline is Komga commit `4ef86df`. Porting is organized as vertical slices so every completed stage can start, persist data, and pass HTTP-level tests.

| Area | Status | Notes |
| --- | --- | --- |
| Process/configuration | Partial | Default config directory, main/task database files, address, graceful shutdown, and tracing are implemented. Spring YAML compatibility is pending. |
| SQLite | Partial | User, library, client/server setting, and task-queue schema is compatible with current Flyway databases. The complete schema and all historical migration paths are pending. |
| Claim and users | Implemented | Claim, current user, CRUD, roles, library sharing fields, age/label restrictions, and password changes. |
| Authentication | Partial | Basic, in-memory sessions, remember-me cookie, header sessions, logout, and REST API keys are implemented. OAuth2 and authentication activity are pending. |
| Libraries and scanner | Partial | Library model/CRUD, access filtering, filesystem browsing, and scan/empty-trash task enqueueing are implemented. Task execution, analysis, sidecars, and metadata providers are pending. |
| Series/books/read progress | Pending | Search, sorting, pagination, metadata, thumbnails, pages, downloads, and progress. |
| Collections/read lists | Pending | CRUD, matching, thumbnails, generated archives, and progress aggregation. |
| Media | Pending | ZIP/RAR/PDF/EPUB detection and extraction, image conversion, hashing, and duplicate pages. |
| Search | Pending | Lucene-equivalent multilingual indexing and lifecycle. |
| OPDS | Pending | OPDS 1 XML and OPDS 2 JSON catalogs. |
| Device sync | Pending | Kobo, KOReader, sync points, and KEPUB conversion. |
| Operations | Partial | Persistent task storage, queue ownership semantics, task clearing, client settings, OAuth provider discovery, and remember-me duration loading are implemented. Workers, schedulers, SSE, metrics, announcements, full server settings, fonts, and release checks are pending. |
| Web UI packaging | Pending | Static asset build/embedding and SPA fallback. |

## Compatibility rules

1. Preserve public paths, status codes, JSON field names, headers, and media types before optimizing internals.
2. Read the current Kotlin schema directly. Rust migrations must be additive and safe when Flyway tables already exist.
3. Keep domain policy independent from Axum and SQLx so Kotlin unit cases can be ported without HTTP or database setup.
4. Add HTTP contract tests for each controller before marking a row implemented.
5. Never require an irreversible database conversion until full feature parity is available.

## Current limitations

- Scan and empty-trash actions enqueue Kotlin-compatible task records but no worker consumes them yet.
- Only empty libraries can be deleted. Non-empty library deletion returns `409 Conflict` until the full series/book cascade is ported.
- The OAuth provider endpoint intentionally returns an empty list until OAuth authentication is implemented.
- `SERVER_SETTINGS` is read only for the remember-me duration; the server-settings API is not implemented.

## Next slice

The next dependency-ordered slice is task execution and scanning:

1. implement the task worker lifecycle, ownership recovery, cancellation, and retry behavior;
2. port filesystem discovery and library scan orchestration behind the existing queue boundary;
3. persist series and book skeletons needed by scan results;
4. add scheduler triggers and task-status SSE events;
5. extend Kotlin database fixtures with representative scanned library content.
