import { svelte } from '@sveltejs/vite-plugin-svelte'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vite'

// npm sets this for any script it runs (npm run dev/build, and
// beforeBuildCommand in tauri.conf.json goes through "npm run build" too),
// so this always matches package.json's version with no file read needed
// — a plain readFileSync here breaks svelte-check's own Vite config loader.
const appVersion = process.env.npm_package_version ?? '0.0.0-dev'

const alias = [
  { find: '@og-suite/ui/ActionBar', replacement: fileURLToPath(new URL('../../packages/ui/src/ActionBar.svelte', import.meta.url)) },
  { find: '@og-suite/ui/Icon', replacement: fileURLToPath(new URL('../../packages/ui/src/Icon.svelte', import.meta.url)) },
  { find: '@og-suite/ui/MobileSuiteTopBar', replacement: fileURLToPath(new URL('../../packages/ui/src/MobileSuiteTopBar.svelte', import.meta.url)) },
  { find: '@og-suite/contracts', replacement: fileURLToPath(new URL('../../packages/contracts/src/index.ts', import.meta.url)) },
  { find: '@og-suite/crdt', replacement: fileURLToPath(new URL('../../packages/crdt/src/index.ts', import.meta.url)) },
  { find: '@og-suite/runtime', replacement: fileURLToPath(new URL('../../packages/runtime/src/index.ts', import.meta.url)) },
  { find: '@og-suite/sync', replacement: fileURLToPath(new URL('../../packages/sync/src/index.ts', import.meta.url)) },
  { find: '@og-suite/ui', replacement: fileURLToPath(new URL('../../packages/ui/src/index.ts', import.meta.url)) },
  { find: '@og-toolkit/theme/ThemeModeToggle', replacement: fileURLToPath(new URL('../../packages/theme/src/ThemeModeToggle.svelte', import.meta.url)) },
  { find: '@og-toolkit/theme', replacement: fileURLToPath(new URL('../../packages/theme/src/index.ts', import.meta.url)) },
]

export default defineConfig({
  base: process.env.OG_NOTES_BASE ?? '/',
  plugins: [svelte()],
  resolve: { alias },
  define: {
    // Single source of truth for the version shown in Settings — bump
    // package.json's version and this follows automatically, no separate
    // env var to remember to set.
    'import.meta.env.VITE_OG_APP_VERSION': JSON.stringify(appVersion),
    // Unlocks "Continue without signing in" (local-only mode) outside of
    // Tauri too — normally gated to the desktop/mobile app only, since the
    // go-server web UI build wants to require a real sign-in. The
    // underlying local-only runtime already works in a plain browser (see
    // runtime.ts's createLocalCache) — only the UI gate needed a way to
    // opt in, for the GitHub Pages demo build (OG_NOTES_DEMO=1).
    'import.meta.env.VITE_OG_NOTES_DEMO_MODE': JSON.stringify(process.env.OG_NOTES_DEMO === '1'),
  },
})
