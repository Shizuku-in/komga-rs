# komga-rs

An incremental Rust port of the [Komga](https://github.com/gotson/komga) backend. The Kotlin source pinned as the `komga/` Git submodule is the behavioral reference; the Rust implementation and its maintained Web UI source live in the repository root.

> The server is usable, but this is not yet a drop-in replacement for a complete Komga server. Users and permissions, library scanning, content browsing and reading, progress, collections and read lists, search, media delivery, metadata, the bundled Web UI, and persistent background tasks are implemented. See [the migration matrix](docs/MIGRATION.md) for exact coverage.

## Port status

The migration matrix currently tracks 14 functional areas; all 14 are implemented or core-complete for the pinned compatibility baseline.

| Status | Areas |
| --- | --- |
| Implemented / core-complete | Process and configuration; SQLite; Claim and users; authentication; libraries and scanner; collections and read lists; Series, Books, and progress; media; search; OPDS; device sync; operations; transient Book import analysis; Web UI packaging |

## Compatibility target

`komga-rs` targets fresh Rust installations and the behavior of the pinned Komga baseline at commit `4ef86df`. Database compatibility is limited to the parts of that baseline schema explicitly covered by the [migration matrix](docs/MIGRATION.md).

The project does not promise direct migration from every historical Kotlin Komga database or reproduce every Flyway migration path. Before switching an existing installation, back up its database and use Kotlin Komga to bring it to the supported baseline; databases from other versions are unsupported unless they are explicitly listed and tested.

SQLite compatibility is enforced against Kotlin-generated main and task database fixtures. Contract tests require all 51 supported application tables, compatible column affinities and primary keys, named indexes, and foreign-key relationships; they also exercise repository round trips across every persisted domain while preserving both Flyway and additive SQLx migration histories.

Media compatibility is enforced against the pinned Kotlin ZIP, RAR4/RAR5, 7z, PDF, fixed-layout EPUB, and reflowable EPUB fixtures. The contracts cover detected types, READY/ERROR/UNSUPPORTED states, error codes, natural page order, dimensions, entry sizes, resource filtering, navigation, and positions. Kotlin's complete Apache Tika MIME database, pixel-identical PDFBox/ImageIO rendering, non-default Spring media settings, and inputs that the pinned Kotlin baseline itself rejects are outside this compatibility target.

Recent content work added modern and deprecated Series/Book queries, the deprecated Series-scoped Book query, streaming whole-Series ZIP downloads, legacy regex search, duplicate-book discovery, book-file hashing, configurable ZIP/RAR/PDF and fixed-layout EPUB edge-page hashing, PageHash administration, recoverable duplicate-page removal from CBZ, PDF, and safe fixed-layout EPUB files plus RAR-to-CBZ selective replacement with historical auditing, RAR/CBR-to-CBZ conversion, source-extension repair, barcode ISBN and one-shot metadata import, authenticated OPDS 1.2 and OPDS 2 catalogs, Kotlin-compatible content-domain SSE, KOReader and Kobo synchronization, application metrics, announcements, release checks, and bundled and local reader fonts.

## Run

Rust 1.96.1 or newer and Node.js 22.11 through 24 are required for the default build. Cargo builds and embeds the root Web UI automatically.

```bash
cargo run --release -p komga-server
```

Packagers can reuse an existing production build instead of invoking Node. Relative paths are resolved from the workspace root:

```bash
cd webui && npm ci && npm run build && cd ..
KOMGA_WEBUI_DIST_DIR=webui/dist cargo build --release -p komga-server
```

For an API-only binary without embedded Web UI assets or a Node dependency, disable the default feature:

```bash
cargo build -p komga-server --no-default-features
```

HEIF/HEIC and AVIF page conversion is an optional native build because it links to `libheif`. The default build remains
pure Rust and serves detected HEIF/AVIF pages unchanged. On Ubuntu 24.04, install the library and HEVC/AV1 decoder plugins,
then enable `heif-native`:

```bash
sudo apt-get install pkg-config libheif-dev libheif-plugin-libde265 libheif-plugin-dav1d
cargo run --release -p komga-server --features heif-native
```

`libheif` 1.17 or newer is required. macOS packagers can use `brew install libheif`; Windows packagers can provide it
through vcpkg. At startup, a `heif-native` server logs whether HEVC and AV1 decoders are actually available. A missing
decoder leaves its media type available for original delivery but unavailable for dimensions, conversion, and generated
thumbnails.

The defaults match Komga:

- listen on `0.0.0.0:25600`;
- keep state in `$HOME/.komga`;
- use `$HOME/.komga/database.sqlite`.
- use `$HOME/.komga/tasks.sqlite` for the persistent task queue.
- load additional reader fonts from `$HOME/.komga/fonts`, alongside bundled OpenDyslexic files.
- hash the first and last 3 pages of eligible ZIP, RAR, PDF, and Divina-compatible fixed-layout EPUB books when a library enables page hashing.
- return at most 100 source books or tags in each Kobo synchronization page.
- enable in-process KEPUB conversion, with the external fallback and Kobo Store proxy disabled until configured.
- serve the embedded Web UI at `/`, including SPA history fallback and configured context paths.
- read optional OAuth2/OIDC providers from `$HOME/.komga/oauth2.json`.

Configuration can be supplied through arguments or environment variables:

```bash
cargo run -p komga-server -- \
  --config-dir /srv/komga \
  --fonts-directory /srv/komga/fonts \
  --oauth2-config-file /srv/komga/oauth2.json \
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
KOMGA_OAUTH2_CONFIG_FILE=/srv/komga/oauth2.json \
KOMGA_KOBO_SYNC_ITEM_LIMIT=100 \
KOMGA_KOBO_KEPUBIFY_PATH=/usr/local/bin/kepubify KOMGA_KOBO_PROXY=true \
KOMGA_PORT=25601 KOMGA_CONTEXT_PATH=/komga \
  cargo run -p komga-server
```

Spring Boot configuration files such as `application.yml`, `application.yaml`, and
`application.properties` are not read by `komga-rs`. They are not part of the compatibility target.
Translate settings used by an existing Kotlin installation to the documented command-line arguments,
`KOMGA_*` environment variables, database-backed server settings, and `oauth2.json` before switching
servers. In particular, do not assume that a database path, port, or context path in a retained Spring
configuration file will be applied by the Rust server.

`--kobo-kepubify-path` and `KOMGA_KOBO_KEPUBIFY_PATH` are optional compatibility fallbacks. The
in-process converter remains the primary implementation when either setting is present.

Do not run the Kotlin and Rust servers against the same SQLite file at the same time. Back up an existing database before the first Rust start. Historical Kotlin database migration is outside the compatibility target; only explicitly documented and tested schemas are supported.
Migration `0011_page_hashing` deliberately clears existing `PAGE_HASH` rules and thumbnails and resets JPEG page hashes because Rust uses a versioned RGB24 pixel hash instead of Java ImageIO's encoder-dependent bytes.

## API compatibility

The core API inventory contains 203 route templates across REST, OAuth, SSE, OPDS, KOReader,
Kobo, and the supported management endpoints. [`docs/API_PARITY.md`](docs/API_PARITY.md) describes
the compatibility boundary, and [`docs/api-parity.json`](docs/api-parity.json) is the canonical
path-and-method manifest enforced by the server integration tests.

Authentication is compatible with the current Komga contract: HTTP Basic, `KOMGA-SESSION`, `X-Auth-Token`, SHA-512-backed `X-API-Key`, OAuth2, and OpenID Connect. Existing Spring BCrypt `$2a$` password hashes are accepted. Password, remember-me, API-key, and OAuth attempts are recorded in Kotlin-compatible authentication activity; ordinary session reuse is not recorded, and entries older than one month are removed daily. KOReader routes exclusively accept a plaintext API key in `X-Auth-User` and require the `KOREADER_SYNC` role. Kobo routes exclusively accept a plaintext API key in the `/kobo/{apiKey}` path and require `KOBO_SYNC`; original-file delivery also requires `FILE_DOWNLOAD`. Kobo keys and OAuth callback parameters are redacted from request tracing. OPDS 2 also exposes the public authentication document and returns its media type, Basic challenge, and discovery link on authentication failures.

### OAuth2 and OpenID Connect

OAuth is disabled when the default `<config-dir>/oauth2.json` does not exist. Use `--oauth2-config-file` or `KOMGA_OAUTH2_CONFIG_FILE` to select another file; a missing explicitly selected file, invalid JSON, failed OIDC discovery, duplicate registration ID, unresolved environment secret, or unsafe endpoint prevents startup. Provider endpoints must use HTTPS, except that loopback HTTP is accepted for local development and tests.

```json
{
  "publicBaseUrl": "https://reader.example/komga",
  "accountCreation": false,
  "oidcEmailVerification": true,
  "providers": [
    {
      "type": "oidc",
      "registrationId": "authentik",
      "name": "Authentik",
      "clientId": "komga",
      "clientSecretEnv": "KOMGA_AUTHENTIK_SECRET",
      "clientAuthenticationMethod": "client_secret_basic",
      "issuerUri": "https://id.example/application/o/komga/",
      "scopes": ["openid", "profile", "email"]
    },
    {
      "type": "oauth2",
      "registrationId": "github",
      "name": "GitHub",
      "clientId": "client-id",
      "clientSecret": "secret",
      "clientAuthenticationMethod": "client_secret_basic",
      "authorizationUri": "https://github.com/login/oauth/authorize",
      "tokenUri": "https://github.com/login/oauth/access_token",
      "userInfoUri": "https://api.github.com/user",
      "emailAttribute": "email",
      "scopes": ["read:user", "user:email"]
    }
  ]
}
```

Register one callback per provider as `<public URL>/login/oauth2/code/<registrationId>`, for example `https://reader.example/komga/login/oauth2/code/authentik`. `publicBaseUrl` is optional; without it, Komga derives the client-visible URL from `Forwarded`, `X-Forwarded-*`, `Host`, and the configured context path. A reverse proxy for the example above can forward those values as follows:

```nginx
location /komga/ {
    proxy_pass http://127.0.0.1:25600;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Forwarded-Prefix /komga;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
}
```

Set at most one of `clientSecret` and `clientSecretEnv`. A provider without a secret must use `clientAuthenticationMethod: "none"`. Login uses Authorization Code with random state, S256 PKCE, and an OIDC nonce; access and refresh tokens are discarded after the callback. Existing users are matched by email without case sensitivity. Unknown email addresses receive `ERR_1025` unless `accountCreation` is enabled, in which case an ordinary non-administrator user is created.

Administrators can retrieve and patch Kotlin-compatible server settings through `/api/v1/settings`. Empty-container cleanup, thumbnail size, task-pool size, remember-me policy, Kobo Store proxying/public URL port, and external `kepubify` fallback selection update live after a committed PATCH. `serverPort` and `serverContextPath` are persisted as database overrides and take effect on the next process start; the response reports configuration, database, and current effective values separately. Database values override CLI/environment values, and clearing nullable overrides restores the configured fallback.

Administrators can inspect nine Komga application metrics through the actuator endpoints. Content gauges query SQLite on each request; task execution and failure measurements are process-local and reset after restart. Announcement and release endpoints read the fixed Komga feed and GitHub release API, cache successful upstream responses for one idle hour, and never cache failures. Announcement read state is persisted per user. These outbound requests are read-only but disclose the server's network address and request metadata to Komga's website or GitHub.

OpenDyslexic WOFF and WOFF2 files are bundled with the server. Additional font families are discovered from immediate subdirectories of the configured fonts directory at startup; a local family with the same name replaces its bundled counterpart. Font catalogs require authentication, while CSS and font resources are public so the EPUB reader can load them without forwarding credentials. Symlinked directories and files, unsupported extensions, nested files, and paths outside the configured root are rejected.

`ScanLibrary` tasks are executed in the background. They discover configured CBX/PDF/EPUB files and persist Kotlin-compatible Series, Book, Media, and initial metadata rows. CBZ, CBR, PDF, and EPUB contents are analyzed and receive generated book thumbnails; fixed-layout EPUB pages, publication resources, navigation, reading positions, and Divina/PDF/EPUB WebPub manifests are available through the REST API. Root-level `ComicInfo.xml`, EPUB OPF, Mylar `series.json`, adjacent Book artwork, and Series artwork sidecars are imported incrementally. Provider updates preserve locked fields, and ComicInfo collection/read-list relationships remain incremental. Libraries with `convertToCbz` enqueue persistent RAR4/RAR5 conversion tasks after scans and when the setting is enabled; `repairExtensions` likewise queues repairs for analyzed CBR/CBZ/PDF/EPUB files. `EmptyTrash` permanently removes soft-deleted database content, honors automatic cleanup after scans, and never deletes source files. Library deletion cascades through database-owned content and sidecars while also preserving source files. Administrators can enqueue deduplicated Kotlin-compatible Book/Series source deletion and batch Book import tasks. Imports support COPY, MOVE, and HARDLINK with copy fallback, destination renaming, one-shot upgrades, sidecars, relation transfer, `BookImported`/upgrade deletion history, analysis follow-up, SSE, and search synchronization. Other unported task types remain queued and unmodified.

Source-file deletion is permanent: no backup, trash copy, or recovery archive is created. The worker deletes only database-registered Book files and `SIDECAR` thumbnails, never unknown directory contents. Before the first unlink it canonicalizes the Library root and every involved existing path; a non-local URL or any real path outside that root rejects the complete task. The Library root directory itself is always preserved, including when its last root-level Book is deleted. Back up real libraries before using these endpoints.

Book imports accept only non-symlink regular source files outside every configured Library. Target directories, upgrade files, and existing sidecars are canonicalized inside the destination Library before any change. The worker stages targets beside their final paths and keeps temporary backups until the main-database transaction commits; validation and database failures restore MOVE sources and replaced upgrade files on a best-effort basis. A committed MOVE is permanent and no long-term backup or trash copy is retained.

RAR/CBR conversion streams only analyzed pages and ancillary files into a validated CBZ, then records `BookFileDeleted` and `BookConverted` in the same database transaction that updates the Book and Media rows. Conversion and extension repair reject non-local, symlinked, stale, out-of-Library, or conflicting paths before replacing the source. A same-directory backup or repair marker supports restart recovery and transaction rollback, but is removed after commit; the original RAR/CBR is then permanently gone. Extension repair preserves `.epub` files whose analyzer classifies them as ordinary ZIP archives instead of renaming them to `.cbz`.

Libraries with `hashKoreader` enabled enqueue Kotlin-compatible `HashBookKoreader` tasks after scans, when the setting is enabled, and after ZIP, RAR, PDF, or EPUB page removal. The worker computes KOReader's partial MD5 against a source snapshot and conditionally persists it to `FILE_HASH_KOREADER`. KOReader clients can then authenticate, pull, and push Divina/PDF page progress or EPUB resource progress. Hash resolution applies the user's library, age, and sharing-label restrictions before reporting collisions.

Kobo synchronization captures Kotlin-compatible SyncPoint snapshots of each user's visible READY EPUB books and virtual On Deck tag. First and incremental synchronization are transactionally paged with Komga, Calibre Web, and official dotted sync-token compatibility. Metadata, reading state, EPUB/native KEPUB files, and JPEG covers are delivered through path-authenticated routes. Ordinary reflowable EPUBs always advertise and stream an in-process converted KEPUB from a five-minute idle cache; a configured `kepubify` executable is used only after a compatible content-conversion failure, never to bypass archive safety or resource limits. Native KEPUB and fixed-layout EPUB delivery remains unchanged. Analysis records KoboSpan positions for new books, while startup submits low-priority analysis for eligible historical EPUBs which have no KoboSpan positions. Converted files remain process-local temporary files and never replace Library sources. Enabling `KOMGA_KOBO_PROXY` or the corresponding database setting forwards only the official authorization/content headers and `x-kobo-*` protocol headers to the fixed Kobo Store endpoint, merges official sync pages after local pages, and redirects unknown covers to Kobo's CDN. The `koboPort` server setting corrects device-visible absolute URLs when no forwarded headers are present; it does not create another listener. Store proxying may disclose device and Kobo account tokens to Kobo; it never forwards the local path API key. Unknown routes still return an empty JSON object while proxying is disabled.

Libraries with `hashPages` enabled enqueue Kotlin-compatible discovery and page-hashing tasks at startup, after scans, and when the setting is enabled. READY ZIP/CBZ, RAR4/RAR5, PDF, and Divina-compatible fixed-layout EPUB media participates. The first and last configured number of pages are hashed; JPEG image pages and rendered PDF pages use a deterministic dimensions-plus-RGB24 representation, while other archive and EPUB page types retain raw-byte XXH3-128 hashing. Administrators can classify hashes as `DELETE_AUTO`, `DELETE_MANUAL`, or `IGNORE` and inspect matching pages and JPEG previews. Automatic and manual deletion include Divina-compatible EPUB matches; reflowable and incomplete fixed-layout EPUB media remains non-removable, so mixed `delete-all` requests still reject without partial task submission. The fixed Kotlin baseline hashes only ZIP pages; RAR, PDF, and fixed-layout EPUB hashing and removal are Rust extensions that retain the existing PageHash endpoints and task payloads.

ZIP removal raw-copies surviving entries in archive order. PDF removal extracts retained pages with their content, resources, dimensions, rotation, and order, then verifies every retained page by rendering it before and after the rewrite. PDF page hashing parses each book once, and PDF rewriting uses one parsed Hayro session and shared render cache for each of the source and output documents without changing JPEG bytes or `RGB24_XXH3_128_V1` hashes. It does not preserve document-level bookmarks, attachments, forms, page labels, or catalog metadata.

Fixed-layout EPUB removal requires a one-to-one Divina page/spine mapping and a standards-compliant, non-encrypted ZIP container. The worker removes selected spine items and physically drops their manifest documents; it also drops an exclusive image when no retained XML resource, package reference, cover declaration, or stylesheet could still use it. Standard EPUB 3 NAV entries, EPUB 2 NCX entries, and OPF guide references that point at a removed document are deleted, with retained nested navigation entries promoted in their original order. References from ordinary retained resources, other OPF ID references, navigation with base-URL overrides, and structurally ambiguous navigation are rejected without changing the source or database. The API returns `202 Accepted` when it queues an eligible EPUB because this reference validation occurs on the background worker. Duplicate, case-ambiguous, unsafe, symlink, encrypted, oversized, stale, and all-page removals are rejected.

ZIP, PDF, and fixed-layout EPUB rewrites validate the complete temporary output and current source snapshot before writing `.komga-page-remove-{bookId}.json`, moving the source to `.komga-page-remove-{bookId}.bak`, and installing the output at the original path. The versioned journal records both database snapshots and file fingerprints so persistence failure restores the old source and an interrupted committed operation keeps the new source. Deep scans recover determinable artifacts even after the task record is gone, retry legacy artifacts after committing a fresh scan snapshot, and preserve unsafe or conflicting files for manual inspection. Source, parent, temporary, backup, and journal paths must remain non-symlink paths inside the Library.

RAR4/RAR5 removal streams every retained registered page and ancillary file into a validated sibling CBZ, then atomically changes the Book URL and Media type and records `BookFileDeleted`, `BookConverted`, and one `DuplicatePageDeleted` event per removed page. An explicit manual deletion or `DELETE_AUTO` rule authorizes this permanent conversion even when the Library has `convertToCbz` disabled. The worker reuses the `.komga-convert-{bookId}.tmp` and `.bak` recovery protocol, preserves and rejects a pre-existing CBZ target, and serializes removal with regular conversion for the same Book. The fixed Kotlin baseline removes pages only from ZIP archives; PDF, RAR, and fixed-layout EPUB removal are Rust extensions that keep the existing PageHash endpoints and task payloads unchanged.

The authenticated SSE endpoint combines the administrator task-count heartbeat with Kotlin event names and payloads for durable library, series, book, collection, read-list, progress, thumbnail, and session changes. Progress and session events are restricted to the affected user; import results are administrator-only. Search synchronization remains explicit and recoverable rather than depending on best-effort SSE delivery.

The modern Series and Book list endpoints support Spring-style pagination, the complete structured condition trees, ranked multilingual `fullTextSearch`, alphabetical groups, fixed discovery feeds, and On Deck selection. Deprecated GET lists and the legacy alphabetical-groups route translate their query parameters into the same condition trees and search candidates; Series `search_regex` supports case-insensitive Rust regexes over `title` and `title_sort`. Referential filter values preserve Kotlin's scope precedence and data-source differences. Collection and read-list `search` uses the same in-process Tantivy index. The query contract covers default-AND terms, phrases, fields and dynamic author roles, ranges, boolean grouping and negation, punctuation, CJK, and Kotlin-compatible empty results for invalid syntax. Library sharing, age, sharing labels, member restrictions, and structured conditions remain enforced by SQLite before pagination. The Rust index uses `<config-dir>/search/v2`, remains independent of Kotlin's Lucene directory, and queues a priority-8 rebuild task at startup when the file index is empty or incompatible; Lucene file-format interoperability is not a compatibility target.

OPDS 1.2 serves Atom navigation and acquisition feeds, OpenSearch discovery, PSE page streaming, thumbnails, and original files. OPDS 2 serves recommended and paged library feeds, browse facets, grouped search, publications, profile manifests, reading resources, and Readium progression. Both generations enforce the same library and content restrictions as REST, generate proxy-aware absolute links, and reuse the existing media and search implementations.

## Develop

Initialize the pinned Kotlin behavioral reference after cloning:

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
