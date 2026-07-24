# Migration status

The reference baseline is Komga commit `4ef86df`. Porting is organized as vertical slices so every completed stage can start, persist data, and pass HTTP-level tests.

| Area | Status | Notes |
| --- | --- | --- |
| Process/configuration | Partial | Default config directory, main/task database files, address, coordinated background-service shutdown, and tracing are implemented. Spring YAML compatibility is pending. |
| SQLite | Partial | User, library, settings, task queue, Series/Book, media pages/files, book thumbnails, metadata relations, and read-progress schema are compatible with current Flyway databases. The complete schema and all historical migration paths are pending. |
| Claim and users | Implemented | Claim, current user, CRUD, roles, library sharing fields, age/label restrictions, and password changes. |
| Authentication | Partial | Basic, in-memory sessions, remember-me cookie, header sessions, logout, and REST API keys are implemented. OAuth2 and authentication activity are pending. |
| Libraries and scanner | Partial | Library CRUD, filesystem browsing, scan workers, startup/periodic scheduling, Series/Book discovery, change reconciliation, soft deletion, and automatic CBZ/CBR/PDF/EPUB analysis are implemented. Sidecars, metadata providers, and trash execution are pending. |
| Series/books/read progress | Partial | Skeleton persistence, natural ordering, modern list/detail APIs, Spring pagination, basic recursive filtering, sibling navigation, access restrictions, progress hydration and per-book writes, REST Readium progression, administrator book/series metadata mutations, CBZ/CBR/PDF and fixed-layout EPUB pages, EPUB resources/positions, WebPub manifests, generated book thumbnails, and streaming original-file downloads are implemented. Series/bulk progress operations, deprecated/specialized lists, and thumbnail management are pending. |
| Collections/read lists | Pending | CRUD, matching, thumbnails, generated archives, and progress aggregation. |
| Media | Partial | ZIP/CBZ, RAR4/RAR5/CBR (including solid archives), PDF, and EPUB analysis are implemented without external runtime commands. PDF supports rendered and raw pages; EPUB supports OPF/NAV/NCX discovery, resources, Readium positions, native KEPUB detection, fixed-layout pages, and package covers. Divina, PDF, and EPUB WebPub manifests are available. EPUB metadata import, uncommon image codecs, hashing, and duplicate pages are pending. |
| Search | Pending | Lucene-equivalent multilingual indexing and lifecycle. |
| OPDS | Pending | OPDS 1 XML and OPDS 2 JSON catalogs. |
| Device sync | Pending | Kobo, KOReader, sync points, and KEPUB conversion. |
| Operations | Partial | Persistent task storage, atomic worker ownership, startup recovery, scan/media/thumbnail/metadata-aggregation workers, library scheduling, task-status SSE, task clearing, client settings, and selected server settings are implemented. Metrics, announcements, full server settings, fonts, and release checks are pending. |
| Web UI packaging | Pending | Static asset build/embedding and SPA fallback. |

## Compatibility rules

1. Preserve public paths, status codes, JSON field names, headers, and media types before optimizing internals.
2. Read the current Kotlin schema directly. Rust migrations must be additive and safe when Flyway tables already exist.
3. Keep domain policy independent from Axum and SQLx so Kotlin unit cases can be ported without HTTP or database setup.
4. Add HTTP contract tests for each controller before marking a row implemented.
5. Never require an irreversible database conversion until full feature parity is available.

## Current limitations

- Scan workers analyze ZIP/CBZ, RAR/CBR, PDF, and EPUB files and generate book covers. Encrypted and multi-volume RAR archives, password-protected PDFs, and DRM-protected or obfuscated EPUB resources remain unsupported. Page hashes, duplicate detection, EPUB metadata providers, and KEPUB conversion are not implemented.
- Series/Book queries do not yet support Lucene `fullTextSearch`, collection/read-list/poster/media-profile conditions, deprecated GET lists, alphabetical groups, or latest/new/updated feeds. Unsupported request bodies and sort fields return `400 Bad Request`.
- `EmptyTrash` and unknown Kotlin tasks remain queued and are not claimed by Rust workers.
- Only empty libraries can be deleted. Non-empty library deletion returns `409 Conflict` until the full series/book cascade is ported.
- The OAuth provider endpoint intentionally returns an empty list until OAuth authentication is implemented.
- `SERVER_SETTINGS` is read only for remember-me duration and task-pool size; the server-settings API is not implemented.

## Next slice

The next dependency-ordered slice is series-level reading progress:

1. expose the series mark-read and mark-unread routes;
2. update every visible book in one storage transaction and rebuild the series progress counters;
3. port the Mihon/Tachiyomi series progress read and update contract on top of the same lifecycle;
4. preserve content restrictions, idempotency, and missing-series behavior;
5. add HTTP tests for complete, reset, partial synchronization, authorization, and rollback behavior.

Book and series metadata can be patched through the Kotlin-compatible administrator routes. The HTTP boundary preserves
each field's distinct omitted-versus-null behavior, while `komga-core` owns normalization and validation. Bulk book
updates validate before writing, ignore missing IDs, and enqueue one deduplicated `AggregateSeriesMetadata` task per
affected series. `komga-storage` atomically replaces metadata relations and aggregations using the existing schema.

Per-book reading progress now supports one-based page updates, completion, idempotent deletion, and REST Readium
progression retrieval and synchronization. Locators are validated against analyzed media, serialized as Kotlin-compatible
gzip JSON, and series progress counts are rebuilt in the same transaction. Equal or older progression timestamps return
`409 Conflict`; book visibility restrictions are applied before progress is read or changed.

Original book files are served from both the canonical REST path and its filename-suffix compatibility alias. The
response uses the analyzed media type and current filesystem length, preserves Spring-compatible UTF-8 attachment
filenames, and streams the source in 8 KiB chunks without buffering the complete file.

Media processing remains isolated in `komga-media`: archive, PDF, EPUB, and image implementations depend on the
lightweight models and profile classification in `komga-core`. The storage layer reads and writes Kotlin's
`MediaExtensionEpub` class name and gzip-compressed JSON representation without format dependencies.

The current ZIP, RAR, PDF, and EPUB paths use linked Rust dependencies and do not require `7z`, `unzip`, `unrar`, Poppler, PDFium, `kepubify`, or other host commands. RAR support uses `rars` with stable default features; encrypted and multi-volume archives retain Kotlin's unsupported status and error codes. PDF support uses Hayro for analysis, rendering, and single-page extraction and requires Rust 1.96.1 or newer; password-protected PDFs remain unsupported. EPUB uses `roxmltree` for package/navigation XML and `scraper` for tolerant XHTML inspection; ordinary EPUB positions follow the Readium 1024-byte rule, while only native KEPUB files receive Kobo span mappings. The image pipeline decodes JPEG, PNG, GIF, WebP, BMP, TIFF, and ICO for dimensions and conversion; other detected image types can be served unchanged but cannot yet be converted into generated JPEG thumbnails.
