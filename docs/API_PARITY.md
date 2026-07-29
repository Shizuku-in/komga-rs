# API compatibility surface

The canonical machine-readable inventory is [`api-parity.json`](api-parity.json). It is pinned to
the Kotlin baseline at commit `4ef86df` and currently contains 207 registered route templates. A
template with multiple HTTP methods is one manifest entry.

The inventory covers the core protocols used by Komga clients:

| Area | Kotlin source | Route templates | Contract status |
| --- | --- | ---: | --- |
| REST and browser authentication | 21 REST controllers, CommonBookController, OAuth callback | 134 | Implemented or explicitly Partial by domain |
| OPDS 1.2 | OpdsController, OpdsCommonController | 19 | Implemented |
| OPDS 2 | Opds2Controller, OpdsCommonController, CommonBookController | 33 | Implemented |
| Kobo | KoboController | 10 | Implemented |
| KOReader | KoreaderSyncController | 4 | Implemented |
| SSE | SseController | 1 | Implemented |
| Supported management | Spring Boot Actuator equivalents | 6 | Implemented |

Each manifest group records its Kotlin source, access class, domain behavior status, paths, and
methods. Protocol-level media types are also recorded. Detailed status and media semantics remain
covered by the HTTP integration tests for each domain; the manifest is the path-and-method source of
truth.

## Automated guard

The server integration suite parses the pinned Kotlin `komga/docs/openapi.json` and requires every one
of its 165 REST operations to appear in the manifest after normalizing path-parameter names. It then
probes every non-Kobo route at both `/` and `/komga`: a `TRACE` request must expose exactly the declared
methods in the `Allow` response header. Every template is also called without credentials to lock its
public, protected, conditionally public, OPDS Basic, Kobo-key, or KOReader-key boundary. DTOs and
successful side effects are covered by the domain HTTP scenarios rather than synthetic manifest requests.
Kobo methods are verified through those functional scenarios because its intentional `ANY` Store-proxy
fallback consumes unmatched methods.

## Search contract

Book and Series `fullTextSearch` and Collection/ReadList `search` use the independent Tantivy v2
index. Supported observable syntax includes default-AND terms, quoted phrases, named fields, dynamic
author roles, inclusive ranges, explicit boolean operators, grouping and negation, punctuation,
hyphenated text, and CJK. Invalid syntax and fields which do not exist in an entity return no hits,
matching the Kotlin API behavior.

SQLite visibility filters are applied before pagination. Relevance is used when requested; an
explicit business sort overrides relevance. The index is rebuilt from SQLite when empty or
incompatible. Reading or writing the Kotlin Lucene directory is not part of the compatibility target.

## Explicit exclusions

| Surface | Decision |
| --- | --- |
| Other `/actuator/*` endpoints | JVM and Spring management internals are not a compatibility target. |
| `/v3/api-docs` and `/swagger-ui/*` | Springdoc and Swagger UI are not a compatibility target. |
| WebUI assets and SPA fallback | Tested separately as browser packaging rather than API routes. |
| Spring Boot configuration files | Replace with CLI arguments, `KOMGA_*`, database settings, and `oauth2.json`. |

The supported management surface is limited to `/actuator/health`, `/actuator/info`,
`/actuator/logfile`, `/actuator/metrics`, `/actuator/metrics/{metric}`, and `/actuator/shutdown`.
Exclusions must be documented here and in the JSON manifest; they must not be represented by
placeholder handlers.
