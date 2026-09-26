# OG Note

Local-first notes + audio recording app for Linux, Windows, macOS, iOS, and Android — Tauri 2 + Svelte.

Forked from [OG-Suite](https://github.com/OmegaGiven/OG-suite)'s notes app: same raw / markdown / rich
editor modes, same markdown-first storage, packaged as a standalone app instead of a Suite module.
Backend counterpart is [og-vault](https://github.com/OmegaGiven/OG-vault).

## Structure

```
apps/note/          Tauri + Svelte app (desktop + mobile targets in src-tauri/gen/)
packages/            @og-suite/{contracts,crdt,runtime,sync,ui} — carried over from OG-Suite
```

## Develop

```sh
npm install
npm run dev              # vite dev server
npm run tauri dev         # native window
```

## Build

```sh
npm run build             # frontend only
npm run tauri build       # native bundle for the current platform
```

Native builds for Linux/Windows/macOS/Android run automatically in CI — see
[`.github/workflows/build.yml`](.github/workflows/build.yml).
