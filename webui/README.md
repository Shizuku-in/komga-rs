# Komga Web UI

This directory contains the Web UI maintained by `komga-rs`. It was imported
from `komga/komga-webui` at Komga commit
`4ef86df96e183612de610dc08a26f851d1e66458`.

Production builds must use this root copy. The `komga/` submodule remains the
read-only behavioral reference and is not the source for packaged Web UI
assets.

The application uses Vue 3, Vuetify 3, Vite, and Vitest. Node.js 24 is the
default toolchain; Node.js 22.12 or newer is also supported. Install
dependencies and run the explicit quality gates with:

```bash
npm ci
npm run lint
npm run test:unit
npm run build
```

For live development, start komga-server on port 25600 and run `npm run dev`.
Vite serves the UI at <http://localhost:8081> and proxies backend requests to
`http://127.0.0.1:25600`. Set `VITE_KOMGA_PROXY_TARGET` to use another backend.

Production builds transpile TypeScript with Vite. Sass is compiled with Sass
Embedded's modern compiler; dependency versions are pinned by
`package-lock.json`.

The default `komga-server` Cargo feature builds this directory and embeds the
result in the binary. Set `KOMGA_WEBUI_DIST_DIR` to reuse a production `dist`
directory, or build `komga-server` with `--no-default-features` for an API-only
binary. Relative prebuilt paths are resolved from the workspace root.
