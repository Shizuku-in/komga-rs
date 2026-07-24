# Migration status

The reference baseline is Komga commit `4ef86df`. Porting is organized as vertical slices so every completed stage can start, persist data, and pass HTTP-level tests.

| Area | Status | Notes |
| --- | --- | --- |
| Process/configuration | Partial | Default config directory, main/task database files, address, coordinated background-service shutdown, and tracing are implemented. Spring YAML compatibility is pending. |
| SQLite | Partial | User, library, settings, task queue, Series/Book, media pages/files, book thumbnails, metadata relations, and read-progress schema are compatible with current Flyway databases. The complete schema and all historical migration paths are pending. |
| Claim and users | Implemented | Claim, current user, CRUD, roles, library sharing fields, age/label restrictions, and password changes. |
| Authentication | Partial | Basic, in-memory sessions, remember-me cookie, header sessions, logout, and REST API keys are implemented. OAuth2 and authentication activity are pending. |
| Libraries and scanner | Partial | Library CRUD, filesystem browsing, scan workers, startup/periodic scheduling, Series/Book discovery, change reconciliation, soft deletion, and automatic CBZ/CBR/PDF/EPUB analysis are implemented. Sidecars, metadata providers, and trash execution are pending. |
| Series/books/read progress | Partial | Skeleton persistence, natural ordering, modern list/detail APIs, Spring pagination, basic recursive filtering, sibling navigation, access restrictions, existing progress hydration, CBZ/CBR/PDF and fixed-layout EPUB pages, EPUB resources/positions, and generated book thumbnails are implemented. Metadata mutations, progress writes, WebPub manifests, deprecated/specialized lists, thumbnail management, and downloads are pending. |
| Collections/read lists | Pending | CRUD, matching, thumbnails, generated archives, and progress aggregation. |
| Media | Partial | ZIP/CBZ, RAR4/RAR5/CBR (including solid archives), PDF, and EPUB analysis are implemented without external runtime commands. PDF supports rendered and raw pages; EPUB supports OPF/NAV/NCX discovery, resources, Readium positions, native KEPUB detection, fixed-layout pages, and package covers. WebPub manifests, EPUB metadata import, uncommon image codecs, hashing, and duplicate pages are pending. |
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

- Scan workers analyze ZIP/CBZ, RAR/CBR, PDF, and EPUB files and generate book covers. Encrypted and multi-volume RAR archives, password-protected PDFs, and DRM-protected or obfuscated EPUB resources remain unsupported. Page hashes, duplicate detection, EPUB metadata providers, WebPub manifests, and KEPUB conversion are not implemented.
- Series/Book queries do not yet support Lucene `fullTextSearch`, collection/read-list/poster/media-profile conditions, deprecated GET lists, alphabetical groups, or latest/new/updated feeds. Unsupported request bodies and sort fields return `400 Bad Request`.
- `EmptyTrash` and unknown Kotlin tasks remain queued and are not claimed by Rust workers.
- Only empty libraries can be deleted. Non-empty library deletion returns `409 Conflict` until the full series/book cascade is ported.
- The OAuth provider endpoint intentionally returns an empty list until OAuth authentication is implemented.
- `SERVER_SETTINGS` is read only for remember-me duration and task-pool size; the server-settings API is not implemented.

## Next slice

The next dependency-ordered slice is WebPub publication support:

1. add Kotlin-compatible WebPub metadata, link, contributor, and publication DTOs at the server boundary;
2. generate profile-specific manifests for Divina, PDF, and EPUB media;
3. expose the generic and profile-specific manifest routes with current visibility and cache rules;
4. reuse EPUB resources, navigation, landmarks, page lists, and positions without moving transport DTOs into `komga-core`;
5. port HTTP contract tests for media types, links, metadata, restrictions, and conditional requests.

Media processing remains isolated in `komga-media`: archive, PDF, EPUB, and image implementations depend on the
lightweight models and profile classification in `komga-core`. The storage layer reads and writes Kotlin's
`MediaExtensionEpub` class name and gzip-compressed JSON representation without format dependencies.

The current ZIP, RAR, PDF, and EPUB paths use linked Rust dependencies and do not require `7z`, `unzip`, `unrar`, Poppler, PDFium, `kepubify`, or other host commands. RAR support uses `rars` with stable default features; encrypted and multi-volume archives retain Kotlin's unsupported status and error codes. PDF support uses Hayro for analysis, rendering, and single-page extraction and requires Rust 1.96.1 or newer; password-protected PDFs remain unsupported. EPUB uses `roxmltree` for package/navigation XML and `scraper` for tolerant XHTML inspection; ordinary EPUB positions follow the Readium 1024-byte rule, while only native KEPUB files receive Kobo span mappings. The image pipeline decodes JPEG, PNG, GIF, WebP, BMP, TIFF, and ICO for dimensions and conversion; other detected image types can be served unchanged but cannot yet be converted into generated JPEG thumbnails.
