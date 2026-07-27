# API compatibility surface

The canonical machine-readable inventory is [`api-parity.json`](api-parity.json). It is pinned to
the Kotlin baseline at commit `4ef86df` and currently contains 203 registered route templates. A
template with multiple HTTP methods is one manifest entry.

The inventory covers the core protocols used by Komga clients:

| Area | Kotlin source | Route templates | Contract status |
| --- | --- | ---: | --- |
| REST and browser authentication | 21 REST controllers, CommonBookController, OAuth callback | 133 | Implemented or explicitly Partial by domain |
| OPDS 1.2 | OpdsController, OpdsCommonController | 19 | Implemented |
| OPDS 2 | Opds2Controller, OpdsCommonController | 32 | Implemented |
| Kobo | KoboController | 10 | Implemented |
| KOReader | KoreaderSyncController | 4 | Implemented |
| SSE | SseController | 1 | Implemented |
| Supported management | Spring Boot Actuator equivalents | 4 | Implemented |

Each manifest group records its Kotlin source, access class, domain behavior status, paths, and
methods. Protocol-level media types are also recorded. Detailed status and media semantics remain
covered by the HTTP integration tests for each domain; the manifest is the path-and-method source of
truth.

## Automated guard

The server integration suite validates the manifest structure and probes every non-Kobo route at both
`/` and `/komga`. A `TRACE` request must expose every declared method in the `Allow` response header;
a missing path or method fails the test. Kobo is verified through its functional tests because its
intentional `ANY` Store-proxy fallback consumes unmatched methods.

## Explicit exclusions

| Surface | Decision |
| --- | --- |
| Other `/actuator/*` endpoints | JVM and Spring management internals are not a compatibility target. |
| `/v3/api-docs` and `/swagger-ui/*` | Springdoc and Swagger UI are not a compatibility target. |
| WebUI assets and SPA fallback | Tested separately as browser packaging rather than API routes. |
| Spring Boot configuration files | Replace with CLI arguments, `KOMGA_*`, database settings, and `oauth2.json`. |

The supported management surface is limited to `/actuator/health`, `/actuator/info`,
`/actuator/metrics`, and `/actuator/metrics/{metric}`. Exclusions must be documented here and in the
JSON manifest; they must not be represented by placeholder handlers.
