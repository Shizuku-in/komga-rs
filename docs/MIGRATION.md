# Migration status

The reference baseline is Komga commit `4ef86df`. Porting is organized as vertical slices so every completed stage can start, persist data, and pass HTTP-level tests.

| Area | Status | Notes |
| --- | --- | --- |
| Process/configuration | Partial | Default config directory, main/task database files, address, coordinated background-service shutdown, and tracing are implemented. Spring YAML compatibility is pending. |
| SQLite | Partial | User, library, settings, task queue, Series/Book, media pages/files, book thumbnails, metadata relations, and read-progress schema are compatible with current Flyway databases. The complete schema and all historical migration paths are pending. |
| Claim and users | Implemented | Claim, current user, CRUD, roles, library sharing fields, age/label restrictions, and password changes. |
| Authentication | Partial | Basic, in-memory sessions, remember-me cookie, header sessions, logout, and REST API keys are implemented. OAuth2 and authentication activity are pending. |
| Libraries and scanner | Partial | Library CRUD, filesystem browsing, scan workers, startup/periodic scheduling, Series/Book discovery, change reconciliation, soft deletion, and automatic CBZ/CBR analysis are implemented. PDF/EPUB analysis, sidecars, metadata providers, and trash execution are pending. |
| Series/books/read progress | Partial | Skeleton persistence, natural ordering, modern list/detail APIs, Spring pagination, basic recursive filtering, sibling navigation, access restrictions, existing progress hydration, CBZ/CBR pages, and generated book thumbnails are implemented. Metadata mutations, progress writes, deprecated/specialized lists, thumbnail management, and downloads are pending. |
| Collections/read lists | Pending | CRUD, matching, thumbnails, generated archives, and progress aggregation. |
| Media | Partial | ZIP/CBZ and RAR4/RAR5/CBR detection, including solid RAR archives, page discovery, dimensions, extraction, JPEG/PNG conversion, and generated covers are implemented without external runtime commands. PDF/EPUB, uncommon image codecs, hashing, and duplicate pages are pending. |
| Search | Pending | Lucene-equivalent multilingual indexing and lifecycle. |
| OPDS | Pending | OPDS 1 XML and OPDS 2 JSON catalogs. |
| Device sync | Pending | Kobo, KOReader, sync points, and KEPUB conversion. |
| Operations | Partial | Persistent task storage, atomic worker ownership, startup recovery, scan/media/thumbnail workers, library scheduling, task-status SSE, task clearing, client settings, and selected server settings are implemented. Metrics, announcements, full server settings, fonts, and release checks are pending. |
| Web UI packaging | Pending | Static asset build/embedding and SPA fallback. |

## Compatibility rules

1. Preserve public paths, status codes, JSON field names, headers, and media types before optimizing internals.
2. Read the current Kotlin schema directly. Rust migrations must be additive and safe when Flyway tables already exist.
3. Keep domain policy independent from Axum and SQLx so Kotlin unit cases can be ported without HTTP or database setup.
4. Add HTTP contract tests for each controller before marking a row implemented.
5. Never require an irreversible database conversion until full feature parity is available.

## Current limitations

- Scan workers analyze ZIP/CBZ and RAR/CBR files and generate their pages and book covers. Encrypted and multi-volume RAR archives remain unsupported. PDF/EPUB analysis, page hashes, duplicate detection, and metadata providers are not implemented.
- Series/Book queries do not yet support Lucene `fullTextSearch`, collection/read-list/poster/media-profile conditions, deprecated GET lists, alphabetical groups, or latest/new/updated feeds. Unsupported request bodies and sort fields return `400 Bad Request`.
- `EmptyTrash` and unknown Kotlin tasks remain queued and are not claimed by Rust workers.
- Only empty libraries can be deleted. Non-empty library deletion returns `409 Conflict` until the full series/book cascade is ported.
- The OAuth provider endpoint intentionally returns an empty list until OAuth authentication is implemented.
- `SERVER_SETTINGS` is read only for remember-me duration and task-pool size; the server-settings API is not implemented.

## Next slice

The next dependency-ordered slice is PDF media support:

1. select a redistributable PDF renderer that can be packaged consistently across supported targets;
2. port page count, dimensions, rasterized page delivery, and raw single-page PDF extraction;
3. feed PDF pages through the existing media, thumbnail, and HTTP pipeline;
4. mark previously unsupported PDF media as `OUTDATED` so it is re-analyzed;
5. port the Kotlin PDF fixtures before proceeding to EPUB.

The current ZIP and RAR paths use linked Rust dependencies and do not require `7z`, `unzip`, `unrar`, Poppler, or other host commands. RAR support uses `rars` with stable default features; encrypted and multi-volume archives retain Kotlin's unsupported status and error codes. The image pipeline decodes JPEG, PNG, GIF, WebP, BMP, TIFF, and ICO for dimensions and conversion; other detected image types can be served unchanged but cannot yet be converted into generated JPEG thumbnails.
