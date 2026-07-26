# Komga Web UI

This directory contains the Web UI maintained by `komga-rs`. It was imported
from `komga/komga-webui` at Komga commit
`4ef86df96e183612de610dc08a26f851d1e66458`.

Production builds must use this root copy. The `komga/` submodule remains the
read-only behavioral reference and is not the source for packaged Web UI
assets.

Node.js 24 is the default toolchain. Install dependencies and run the explicit
quality gates with:

```bash
npm ci
npm run lint
npm run test:unit -- --runInBand
npm run build
```

Production builds transpile TypeScript without running the memory-intensive
Vue CLI type-checking plugin. Sass is compiled with Sass Embedded's shared
modern compiler; dependency versions are pinned by `package-lock.json`.

The default `komga-server` Cargo feature builds this directory and embeds the
result in the binary. Set `KOMGA_WEBUI_DIST_DIR` to reuse a production `dist`
directory, or build `komga-server` with `--no-default-features` for an API-only
binary. Relative prebuilt paths are resolved from the workspace root.
