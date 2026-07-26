# Migration status

The `komga/` Git submodule pins the behavioral reference baseline at Komga commit `4ef86df`. The root `webui/` directory is the maintained Web UI source imported from that baseline. Porting is organized as vertical slices so every completed stage can start, persist data, and pass HTTP-level tests.

| Area | Status | Notes |
| --- | --- | --- |
| Process/configuration | Partial | Default config directory, main/task database files, search/font directories, address/context path, Kobo sync page limit, coordinated background-service shutdown, tracing, and Kotlin-compatible database/REST server settings are implemented. Spring YAML remains pending. |
| SQLite | Partial | User, library, settings, task queue, Series/Book, media pages/files, book/series thumbnails, metadata relations, read progress, SyncPoint snapshots, announcement read state, and historical events are compatible with current Flyway databases. The complete schema and all historical migration paths are pending. |
| Claim and users | Implemented | Claim, current user, CRUD, roles, library sharing fields, age/label restrictions, and password changes. |
| Authentication | Partial | Basic, in-memory sessions, remember-me cookie, header sessions, logout, and REST API keys are implemented. OAuth2 and authentication activity are pending. |
| Libraries and scanner | Partial | Library CRUD with database cascade deletion, filesystem browsing, scan workers, startup/periodic scheduling, Series/Book discovery, change reconciliation, soft deletion, EmptyTrash/automatic cleanup, Kotlin-compatible Book/Series source-file deletion tasks, RAR/CBR-to-CBZ conversion and source-extension repair scheduling, automatic CBZ/CBR/PDF/EPUB analysis, ComicInfo/EPUB metadata import, Mylar `series.json`, and Book/Series artwork sidecars are implemented. |
| Series/books/read progress | Partial | Skeleton persistence, natural ordering, modern and deprecated list/detail APIs, complete structured filtering, legacy Series regex search, modern and legacy alphabetical groups, latest/new/updated and On Deck feeds, Referential filter lookups, Spring pagination, sibling navigation, access restrictions, progress hydration, per-book and series-level writes, REST Readium progression, Mihon/Tachiyomi series synchronization, administrator metadata mutation, source-file deletion, batch Book import and upgrade, duplicate Book and Page discovery, thumbnail management and regeneration, CBZ/CBR/PDF and fixed-layout EPUB pages, EPUB resources/positions, WebPub manifests, and streaming original-file downloads are implemented. |
| Collections/read lists | Core complete | CRUD, visible membership, reverse lookup, navigation, Mihon progress, ComicRack matching, streamed ZIP downloads, uploads, dynamic collage posters, and scanner-driven ComicInfo relationship import. |
| Media | Partial | ZIP/CBZ, RAR4/RAR5/CBR (including solid archives), PDF, and EPUB analysis are implemented. Ready RAR4/RAR5 books can be streamed into validated CBZ replacements, and analyzed CBR/CBZ/PDF/EPUB source extensions can be repaired. PDF supports rendered and raw pages; EPUB supports OPF/NAV/NCX discovery, resources, Readium positions, native KEPUB detection, in-process KEPUB conversion and KoboSpan enrichment, fixed-layout pages, and package covers. Divina, PDF, and EPUB WebPub manifests are available. Kotlin-compatible XXH3-128 and KOReader partial-MD5 book hashing, configurable ZIP edge-page hashing, PageHash administration, automatic/manual duplicate-page discovery, and validated atomic ZIP page removal are implemented. Uncommon image codecs and page removal from RAR/PDF/EPUB remain pending. |
| Search | Partial | Tantivy schema v2 covers ranked Book/Series `fullTextSearch` and Collection/ReadList `search`, with SQLite visibility filtering, explicit post-commit synchronization, and streamed asynchronous rebuilds. Kotlin Lucene index interoperability and the full Classic QueryParser grammar remain pending; search deliberately does not depend on best-effort SSE delivery. |
| OPDS | Implemented | OPDS 1.2 Atom/OpenSearch catalogs with PSE acquisition links and OPDS 2 catalogs, authentication, publications, manifests, resources, browse facets, grouped search, and progression aliases. |
| Device sync | Implemented | KOReader API-key authentication, partial-MD5 document matching, and Divina/PDF/EPUB reading-progress synchronization are implemented. Kobo path-key authentication, SyncPoint library and On Deck synchronization, bidirectional progress, initialization, metadata, original and converted KEPUB delivery, JPEG covers, optional Store merging/proxying, and public-port URL correction are implemented. |
| Operations | Implemented | Persistent task storage, atomic worker ownership, startup recovery, dynamically resizable task processing, scan/media/file-hash/KOReader-hash/page-hash/duplicate-page-removal/source-deletion/source-import/RAR-conversion/extension-repair/local-artwork/thumbnail/metadata-refresh/metadata-aggregation workers, library scheduling, task-status and content-domain SSE, historical-event administration, task clearing, client and server settings, application metrics, announcements, release checks, and bundled/local reader fonts are implemented. |
| Web UI packaging | Pending | The source is maintained in root `webui/`; static asset build/embedding and SPA fallback are pending. |

## Compatibility rules

1. Preserve public paths, status codes, JSON field names, headers, and media types before optimizing internals.
2. Read the current Kotlin schema directly. Rust migrations must be additive and safe when Flyway tables already exist.
3. Keep domain policy independent from Axum and SQLx so Kotlin unit cases can be ported without HTTP or database setup.
4. Add HTTP contract tests for each controller before marking a row implemented.
5. Do not perform irreversible database conversions without an explicitly accepted and documented compatibility reset; `0011_page_hashing` is the current exception and requires a backup.

## Current limitations

- Scan workers analyze ZIP/CBZ, RAR/CBR, PDF, and EPUB files, hash book files for standard and KOReader matching, hash ZIP edge pages, discover classified duplicate pages, import EPUB metadata, generate book covers, and schedule enabled RAR-to-CBZ conversion or source-extension repair. Conversion supports ready local RAR4/RAR5 books, including solid archives; encrypted and multi-volume RAR archives, password-protected PDFs, DRM-protected or obfuscated EPUB resources, and RAR/PDF/EPUB page hashes or page removal remain unsupported. KEPUB conversion runs in process and does not require an external executable.
- Series/Book queries support ranked multilingual `fullTextSearch`, the complete modern condition trees, deprecated GET parameter translation, legacy Series `search_regex`, discovery feeds, modern and legacy alphabetical groups, On Deck, and Referential filter values. Unsupported modern request bodies and sort fields return `400 Bad Request`; deprecated queries retain Kotlin's lenient unknown-parameter and unknown-sort behavior. Legacy regexes use Rust regex syntax, which intentionally does not support Java look-around or backreferences.
- Unknown Kotlin tasks remain queued and are not claimed by Rust workers.
- READY ZIP books can hash their configured first and last pages through Kotlin-compatible `FindBooksWithMissingPageHash` and `HashBookPages` tasks. JPEG hashes use Rust's versioned RGB24 representation and intentionally differ from Java ImageIO hashes; migration `0011` clears existing PageHash rules, thumbnails, and JPEG page hashes before regeneration.
- PageHash administration and the Kotlin-compatible `FindDuplicatePagesToDelete` and `RemoveHashedPages` tasks are implemented for local READY ZIP/CBZ books. Removal rejects stale snapshots, duplicate entry names, and deleting every page; raw-copies all survivors and validates the temporary ZIP before atomic replacement. Each removed page writes `DuplicatePageDeleted` to the historical-event tables in the same transaction, and the changed Book is emitted through content-domain SSE. No source backup is created.
- `GET /api/v1/history` reads Kotlin and Rust historical events with administrator-only Spring pagination. Rust currently produces `DuplicatePageDeleted`, `BookFileDeleted`, `SeriesFolderDeleted`, `BookImported`, and `BookConverted`.
- Kotlin-compatible `FindBooksToConvert`, `ConvertBook`, and `RepairExtension` tasks are persistent and deduplicated. Conversion validates the complete CBZ and source snapshot before a reversible same-directory replacement, atomically updates Book/Media/history rows, restores the RAR/CBR when persistence fails, publishes content SSE, and synchronizes search. A committed conversion permanently removes the original source. All source, target, temporary, backup, and marker paths must resolve inside the Library and must not be symlinks; conflicting targets are preserved and reject the task. Extension repair intentionally leaves a `.epub` classified as ordinary ZIP unchanged.
- Administrator `DELETE /api/v1/books/{id}/file` and `DELETE /api/v1/series/{id}/file` enqueue priority-8 `DeleteBook`/`DeleteSeries` tasks and return `202` even for unknown IDs. Deletion is permanent and limited to registered source files and `SIDECAR` thumbnails. Existing paths are canonicalized before any unlink; non-local or out-of-Library paths reject the whole task, unknown directory entries are preserved, and the Library root is never removed.
- Administrator `POST /api/v1/books/import` accepts Kotlin-compatible batches and enqueues priority-8, Series-grouped `ImportBook` tasks. COPY, MOVE, HARDLINK-with-copy-fallback, destination names, one-shot upgrades, Book artwork sidecars, user thumbnails, metadata, media, progress, and read-list transfer are implemented. Sources must be non-symlink regular files outside every Library; targets are staged and database failures restore replaced or moved files on a best-effort basis. A supplied but missing or cross-Series `upgradeBookId` is rejected instead of reproducing Kotlin's inconsistent pseudo-upgrade behavior.
- `/sse/v1/events` emits Kotlin-compatible names and JSON for library, series, book, collection, read-list, progress, thumbnail, and session changes. Progress and session events are user-scoped, `BookImported` is administrator-scoped, lagging subscribers skip old best-effort notifications, and search recovery remains explicit.
- OPDS 1.2 and OPDS 2 use proxy-aware absolute links and enforce library sharing, age, and sharing-label visibility before returning catalogs or reading resources. OPDS 2 authentication exposes only the Basic flow until OAuth authentication is implemented.
- KOReader synchronization requires a Library with `hashKoreader` enabled and a user with the `KOREADER_SYNC` role. Clients place a Komga API key in `X-Auth-User`; user creation remains disabled. Document-hash lookup applies the same library, age, and sharing-label visibility as REST before progress is returned or changed.
- Kobo synchronization requires a user with `KOBO_SYNC` and places the plaintext API key in the `/kobo/{apiKey}` path. File delivery additionally requires `FILE_DOWNLOAD`. Only visible, non-deleted READY EPUB books and the virtual `KOMGA-ONDECK` tag are snapshotted. Interrupted snapshots are retained until synchronization resumes or `DELETE /api/v1/syncpoints/me` resets them. In-process KEPUB output is temporary and cached for five idle minutes; eligible historical EPUBs missing KoboSpan positions receive a low-priority analysis task at startup. Store proxying is disabled by default, targets only `https://storeapi.kobo.com`, and forwards official authorization/device headers but never the local path key.
- The OAuth provider endpoint intentionally returns an empty list until OAuth authentication is implemented.
- `GET/PATCH /api/v1/settings` reads and atomically updates Kotlin-compatible `SERVER_SETTINGS`. Task concurrency, future remember-me sessions, remember-key rotation, cleanup policy, thumbnail size, Kobo proxy/public port, and external kepubify fallback selection update live. Main port and context-path overrides intentionally take effect after restart.
- `/actuator/metrics` exposes the nine Komga application metrics rather than a general JVM/Micrometer registry. Content gauges are live SQLite snapshots; task execution and failure values are process-local and reset after restart. Announcement and release routes are administrator-only, use fixed Komga and GitHub upstreams, and cache only successful responses for one idle hour. Announcement read state is persisted per user. The font family catalog is built at startup, local families override bundled names, and symlinked or out-of-root files are rejected; font CSS and resources remain anonymous for reader compatibility.

## Next slice

The next compatibility slice can address page removal for one non-ZIP format or Web UI packaging. Series artwork sidecars, database-backed dynamic server settings, the Operations API, and in-process KEPUB conversion now close their respective scanner, configuration, administration, and Kobo delivery gaps. Kotlin's `koboPort` is an advertised-port correction for direct device URLs, not a dedicated listener.

### In-process KEPUB conversion

`komga-media` exposes a synchronous reader/writer converter with cooperative cancellation. Its default behavior targets
`kepubify` v4.0.4 compatibility, including Kobo CSS and wrappers, deterministic sentence and image spans, OPF cover and
dummy-title-page updates, filtered ancillary files, and a standards-compliant EPUB ZIP layout. The compatibility corpus
compares entry sets, OPF and DOM semantics, and span identities rather than compressed ZIP bytes. Optional typography,
custom CSS, find/replace, hyphenation, old-firmware fullscreen fixes, and the complete `kepubify` CLI are out of scope.

Ordinary reflowable EPUBs advertise KEPUB without configuration. Native KEPUB and fixed-layout publications retain their
existing delivery formats. A configured `kepubify` path remains visible through the existing settings contract and is used
only as a fallback for invalid or incompatible content after archive safety and size checks pass. Cancellation, I/O,
unsafe archives, and resource-limit failures cannot invoke the fallback. Both in-process and fallback output must reopen
as a valid EPUB, contain a correct mimetype and package, and preserve source content documents before entering the cache.

Conversion is limited to 65,535 ZIP entries, 16 MiB per transformed XML/XHTML document, 256 MiB across transformed
documents, and 4 GiB of declared uncompressed data. Duplicate, absolute, backslash, traversal, symlink, encrypted, and
case-ambiguous entries are rejected. A two-permit semaphore bounds conversion work, requests for the same source snapshot
share one five-minute idle cache entry, and generated files remain process-local temporary files. Startup queues priority-0
analysis only for READY reflowable non-native EPUBs whose nonempty positions all lack KoboSpan data. The translated code
retains kepubify's MIT attribution in the workspace `NOTICE` file.

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

The ZIP, RAR, PDF, and EPUB paths use linked Rust dependencies and do not require `7z`, `unzip`, `unrar`, Poppler, PDFium, or `kepubify`. RAR support uses `rars` with stable default features; encrypted and multi-volume archives retain Kotlin's unsupported status and error codes. PDF support uses Hayro for analysis, rendering, and single-page extraction and requires Rust 1.96.1 or newer; password-protected PDFs remain unsupported. EPUB uses `roxmltree` and `quick-xml` for package XML and `scraper`/`html5ever`/`ego-tree` for tolerant XHTML inspection and conversion; ordinary EPUB positions follow the Readium 1024-byte rule and receive Kobo spans from the in-process converter. The image pipeline decodes JPEG, PNG, GIF, WebP, BMP, TIFF, and ICO for dimensions and conversion; other detected image types can be served unchanged but cannot yet be converted into generated JPEG thumbnails.
