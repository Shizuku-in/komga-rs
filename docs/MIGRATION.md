# Migration status

The reference baseline is Komga commit `4ef86df`. Porting is organized as vertical slices so every completed stage can start, persist data, and pass HTTP-level tests.

| Area | Status | Notes |
| --- | --- | --- |
| Process/configuration | Partial | Default config directory, main/task database files, address, coordinated background-service shutdown, and tracing are implemented. Spring YAML compatibility is pending. |
| SQLite | Partial | User, library, settings, task queue, and Series/Book skeleton schema is compatible with current Flyway databases. The complete schema and all historical migration paths are pending. |
| Claim and users | Implemented | Claim, current user, CRUD, roles, library sharing fields, age/label restrictions, and password changes. |
| Authentication | Partial | Basic, in-memory sessions, remember-me cookie, header sessions, logout, and REST API keys are implemented. OAuth2 and authentication activity are pending. |
| Libraries and scanner | Partial | Library CRUD, filesystem browsing, scan workers, startup/periodic scheduling, Series/Book discovery, change reconciliation, and soft deletion are implemented. Media analysis, sidecars, metadata providers, and trash execution are pending. |
| Series/books/read progress | Partial | Series/Book skeleton persistence, natural book ordering, and initial metadata/media rows are implemented. REST queries, filtering, thumbnails, pages, downloads, and progress are pending. |
| Collections/read lists | Pending | CRUD, matching, thumbnails, generated archives, and progress aggregation. |
| Media | Pending | ZIP/RAR/PDF/EPUB detection and extraction, image conversion, hashing, and duplicate pages. |
| Search | Pending | Lucene-equivalent multilingual indexing and lifecycle. |
| OPDS | Pending | OPDS 1 XML and OPDS 2 JSON catalogs. |
| Device sync | Pending | Kobo, KOReader, sync points, and KEPUB conversion. |
| Operations | Partial | Persistent task storage, atomic worker ownership, startup recovery, scan workers, library scheduling, task-status SSE, task clearing, client settings, and selected server settings are implemented. Metrics, announcements, full server settings, fonts, and release checks are pending. |
| Web UI packaging | Pending | Static asset build/embedding and SPA fallback. |

## Compatibility rules

1. Preserve public paths, status codes, JSON field names, headers, and media types before optimizing internals.
2. Read the current Kotlin schema directly. Rust migrations must be additive and safe when Flyway tables already exist.
3. Keep domain policy independent from Axum and SQLx so Kotlin unit cases can be ported without HTTP or database setup.
4. Add HTTP contract tests for each controller before marking a row implemented.
5. Never require an irreversible database conversion until full feature parity is available.

## Current limitations

- Scan workers discover files and persist content skeletons, but do not inspect archive/PDF/EPUB contents or generate pages, hashes, metadata, or thumbnails.
- `EmptyTrash` and unknown Kotlin tasks remain queued and are not claimed by Rust workers.
- Only empty libraries can be deleted. Non-empty library deletion returns `409 Conflict` until the full series/book cascade is ported.
- The OAuth provider endpoint intentionally returns an empty list until OAuth authentication is implemented.
- `SERVER_SETTINGS` is read only for remember-me duration and task-pool size; the server-settings API is not implemented.

## Next slice

The next dependency-ordered slice is content queries and media analysis:

1. implement Kotlin-compatible Series and Book list/get DTOs, pagination, filtering, and access restrictions;
2. port CBX/ZIP/RAR, PDF, and EPUB media detection and page discovery;
3. add AnalyzeBook task emission/execution and Media status transitions;
4. expose page and thumbnail retrieval after analysis is available;
5. extend Kotlin fixtures and HTTP contracts with analyzed content.
