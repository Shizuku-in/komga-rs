# Migration status

The reference baseline is Komga commit `4ef86df`. Porting is organized as vertical slices so every completed stage can start, persist data, and pass HTTP-level tests.

| Area | Status | Notes |
| --- | --- | --- |
| Process/configuration | Partial | Default config directory, main/task database files, address, Kobo sync page limit, optional kepubify path and Store proxy flag, coordinated background-service shutdown, and tracing are implemented. Spring YAML and database-backed dynamic Kobo settings are pending. |
| SQLite | Partial | User, library, settings, task queue, Series/Book, media pages/files, book/series thumbnails, metadata relations, read progress, SyncPoint snapshots, and historical events are compatible with current Flyway databases. The complete schema and all historical migration paths are pending. |
| Claim and users | Implemented | Claim, current user, CRUD, roles, library sharing fields, age/label restrictions, and password changes. |
| Authentication | Partial | Basic, in-memory sessions, remember-me cookie, header sessions, logout, and REST API keys are implemented. OAuth2 and authentication activity are pending. |
| Libraries and scanner | Partial | Library CRUD with database cascade deletion, filesystem browsing, scan workers, startup/periodic scheduling, Series/Book discovery, change reconciliation, soft deletion, EmptyTrash/automatic cleanup, automatic CBZ/CBR/PDF/EPUB analysis, ComicInfo/EPUB metadata import, and Mylar `series.json` sidecars are implemented. Artwork sidecars and source-file deletion tasks are pending. |
| Series/books/read progress | Partial | Skeleton persistence, natural ordering, modern and deprecated list/detail APIs, complete structured filtering, legacy Series regex search, modern and legacy alphabetical groups, latest/new/updated and On Deck feeds, Referential filter lookups, Spring pagination, sibling navigation, access restrictions, progress hydration, per-book and series-level writes, REST Readium progression, Mihon/Tachiyomi series synchronization, administrator book/series metadata mutations, duplicate Book and Page discovery, book/series thumbnail management and regeneration, CBZ/CBR/PDF and fixed-layout EPUB pages, EPUB resources/positions, WebPub manifests, and streaming original-file downloads are implemented. |
| Collections/read lists | Core complete | CRUD, visible membership, reverse lookup, navigation, Mihon progress, ComicRack matching, streamed ZIP downloads, uploads, dynamic collage posters, and scanner-driven ComicInfo relationship import. |
| Media | Partial | ZIP/CBZ, RAR4/RAR5/CBR (including solid archives), PDF, and EPUB analysis are implemented. PDF supports rendered and raw pages; EPUB supports OPF/NAV/NCX discovery, resources, Readium positions, native KEPUB detection, optional kepubify-backed KoboSpan enrichment, fixed-layout pages, and package covers. Divina, PDF, and EPUB WebPub manifests are available. Kotlin-compatible XXH3-128 and KOReader partial-MD5 book hashing, configurable ZIP edge-page hashing, PageHash administration, automatic/manual duplicate-page discovery, and validated atomic ZIP page removal are implemented. Uncommon image codecs and page removal from RAR/PDF/EPUB remain pending. |
| Search | Partial | Tantivy schema v2 covers ranked Book/Series `fullTextSearch` and Collection/ReadList `search`, with SQLite visibility filtering, explicit post-commit synchronization, and streamed asynchronous rebuilds. Kotlin Lucene index interoperability and the full Classic QueryParser grammar remain pending; search deliberately does not depend on best-effort SSE delivery. |
| OPDS | Implemented | OPDS 1.2 Atom/OpenSearch catalogs with PSE acquisition links and OPDS 2 catalogs, authentication, publications, manifests, resources, browse facets, grouped search, and progression aliases. |
| Device sync | Partial | KOReader API-key authentication, partial-MD5 document matching, and Divina/PDF/EPUB reading-progress synchronization are implemented. Kobo path-key authentication, SyncPoint library and On Deck synchronization, bidirectional progress, initialization, metadata, original and converted KEPUB delivery, JPEG covers, and optional Kobo Store merging/proxying are implemented. A dedicated Kobo port and database/REST dynamic settings remain pending. |
| Operations | Partial | Persistent task storage, atomic worker ownership, startup recovery, scan/media/file-hash/KOReader-hash/page-hash/duplicate-page-removal/thumbnail/metadata-refresh/metadata-aggregation workers, library scheduling, task-status and content-domain SSE, historical-event administration, task clearing, client settings, and selected server settings are implemented. Metrics, announcements, full server settings, fonts, and release checks are pending. |
| Web UI packaging | Pending | Static asset build/embedding and SPA fallback. |

## Compatibility rules

1. Preserve public paths, status codes, JSON field names, headers, and media types before optimizing internals.
2. Read the current Kotlin schema directly. Rust migrations must be additive and safe when Flyway tables already exist.
3. Keep domain policy independent from Axum and SQLx so Kotlin unit cases can be ported without HTTP or database setup.
4. Add HTTP contract tests for each controller before marking a row implemented.
5. Do not perform irreversible database conversions without an explicitly accepted and documented compatibility reset; `0011_page_hashing` is the current exception and requires a backup.

## Current limitations

- Scan workers analyze ZIP/CBZ, RAR/CBR, PDF, and EPUB files, hash book files for standard and KOReader matching, hash ZIP edge pages, discover classified duplicate pages, import EPUB metadata, and generate book covers. Encrypted and multi-volume RAR archives, password-protected PDFs, DRM-protected or obfuscated EPUB resources, and RAR/PDF/EPUB page hashes or page removal remain unsupported. KEPUB conversion requires an external `kepubify` executable.
- Series/Book queries support ranked multilingual `fullTextSearch`, the complete modern condition trees, deprecated GET parameter translation, legacy Series `search_regex`, discovery feeds, modern and legacy alphabetical groups, On Deck, and Referential filter values. Unsupported modern request bodies and sort fields return `400 Bad Request`; deprecated queries retain Kotlin's lenient unknown-parameter and unknown-sort behavior. Legacy regexes use Rust regex syntax, which intentionally does not support Java look-around or backreferences.
- Unknown Kotlin tasks remain queued and are not claimed by Rust workers.
- READY ZIP books can hash their configured first and last pages through Kotlin-compatible `FindBooksWithMissingPageHash` and `HashBookPages` tasks. JPEG hashes use Rust's versioned RGB24 representation and intentionally differ from Java ImageIO hashes; migration `0011` clears existing PageHash rules, thumbnails, and JPEG page hashes before regeneration.
- PageHash administration and the Kotlin-compatible `FindDuplicatePagesToDelete` and `RemoveHashedPages` tasks are implemented for local READY ZIP/CBZ books. Removal rejects stale snapshots, duplicate entry names, and deleting every page; raw-copies all survivors and validates the temporary ZIP before atomic replacement. Each removed page writes `DuplicatePageDeleted` to the historical-event tables in the same transaction, and the changed Book is emitted through content-domain SSE. No source backup is created.
- `GET /api/v1/history` reads Kotlin and Rust historical events with administrator-only Spring pagination. Rust currently produces `DuplicatePageDeleted`; the other Kotlin event types depend on still-unported file deletion, conversion, and import workflows.
- `/sse/v1/events` emits Kotlin-compatible names and JSON for library, series, book, collection, read-list, progress, thumbnail, and session changes. Progress and session events are user-scoped, `BookImported` is administrator-scoped, lagging subscribers skip old best-effort notifications, and search recovery remains explicit.
- OPDS 1.2 and OPDS 2 use proxy-aware absolute links and enforce library sharing, age, and sharing-label visibility before returning catalogs or reading resources. OPDS 2 authentication exposes only the Basic flow until OAuth authentication is implemented.
- KOReader synchronization requires a Library with `hashKoreader` enabled and a user with the `KOREADER_SYNC` role. Clients place a Komga API key in `X-Auth-User`; user creation remains disabled. Document-hash lookup applies the same library, age, and sharing-label visibility as REST before progress is returned or changed.
- Kobo synchronization requires a user with `KOBO_SYNC` and places the plaintext API key in the `/kobo/{apiKey}` path. File delivery additionally requires `FILE_DOWNLOAD`. Only visible, non-deleted READY EPUB books and the virtual `KOMGA-ONDECK` tag are snapshotted. Interrupted snapshots are retained until synchronization resumes or `DELETE /api/v1/syncpoints/me` resets them. Configured `kepubify` output is temporary and cached for five idle minutes; existing books are not automatically reanalyzed. Store proxying is disabled by default, targets only `https://storeapi.kobo.com`, and forwards official authorization/device headers but never the local path key.
- The OAuth provider endpoint intentionally returns an empty list until OAuth authentication is implemented.
- `SERVER_SETTINGS` is read only for remember-me duration and task-pool size; the server-settings API is not implemented.

## Next slice

The next compatibility slice can add file deletion/import/conversion workflows that produce the remaining historical event types. Page removal for non-ZIP formats, a dedicated Kobo listener, and database-backed dynamic server settings remain later work.

### Planned in-process KEPUB conversion

Replace the optional external `kepubify` process with a Rust implementation after the current Kobo synchronization
surface has stabilized. This is a compatibility port for Komga's conversion needs, not initially a port of the complete
`kepubify` command-line interface or its optional typography and text-replacement features.

The converter should live in `komga-media` and expose a reader/writer API with cooperative cancellation. It must preserve
the EPUB package structure and ZIP requirements, update the OPF where required, and transform reflowable spine documents
with Kobo styles, `book-columns`/`book-inner` wrappers, and deterministic `kobo.*` spans. The XHTML path must tolerate the
malformed HTML/XML commonly accepted by `kepubify` and emit compatible polyglot XHTML without needlessly rewriting valid
input. Native KEPUB and fixed-layout detection remains outside the converter and continues to use the existing media
analysis policy.

Implementation should proceed behind the existing KEPUB service boundary:

1. Add the conversion library and mandatory EPUB/OPF/XHTML transformations to `komga-media`.
2. Port or adapt upstream fixtures and run differential tests against the official `kepubify` output. Compare package
   semantics, transformed markup, and Kobo span identities rather than compressed ZIP bytes.
3. Switch server conversion and media-analysis enrichment to the in-process converter while retaining the external
   executable as a temporary fallback and comparison oracle. Remove `KOMGA_KOBO_KEPUBIFY_PATH` only after the compatibility
   corpus and Kobo device tests pass without the fallback.

The test corpus should cover EPUB2 and EPUB3, malformed XHTML, XML declarations and processing instructions, namespaces,
SVG and MathML, right-to-left and Unicode text, pre-existing Kobo spans, navigation variants, fixed-layout publications,
ZIP entry metadata, cancellation, and hostile archive limits. Since `kepubify` is MIT-licensed, any translated source or
adapted fixtures must retain its copyright and license notice.

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

The ZIP, RAR, PDF, and base EPUB paths use linked Rust dependencies and do not require `7z`, `unzip`, `unrar`, Poppler, or PDFium. RAR support uses `rars` with stable default features; encrypted and multi-volume archives retain Kotlin's unsupported status and error codes. PDF support uses Hayro for analysis, rendering, and single-page extraction and requires Rust 1.96.1 or newer; password-protected PDFs remain unsupported. EPUB uses `roxmltree` for package/navigation XML and `scraper` for tolerant XHTML inspection; ordinary EPUB positions follow the Readium 1024-byte rule and receive Kobo spans when the optional external `kepubify` conversion succeeds. The image pipeline decodes JPEG, PNG, GIF, WebP, BMP, TIFF, and ICO for dimensions and conversion; other detected image types can be served unchanged but cannot yet be converted into generated JPEG thumbnails.
