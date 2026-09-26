import { mount } from 'svelte'
import { applyTokens, defaultTokens, loadThemeMode, resolveTheme } from '@og-toolkit/theme'
import StandaloneNotesApp from './StandaloneNotesApp.svelte'
import './styles.css'

// EXPERIMENTAL, see styles.css's .og-no-backdrop-blur rule: testing
// whether the app's pervasive backdrop-filter: blur() usage is what
// triggers the AdrenoVK "Shader compilation failed" white-screen bug on
// Android, as an alternative to disabling hardware acceleration
// entirely (see AndroidManifest.xml). Remove this block (and the CSS
// rule) once resolved either way.
if (typeof navigator !== 'undefined' && /Android/i.test(navigator.userAgent)) {
  document.documentElement.classList.add('og-no-backdrop-blur')
}

// Apply the resolved theme (system or independent, see @og-toolkit/theme)
// before mounting, so the app never flashes its bundled default colors
// first. resolveTheme can end up waiting on a Tauri fs plugin IPC call
// (readSystemTheme -> @tauri-apps/plugin-fs's exists()) that, on Android,
// has been observed to occasionally never resolve or reject on a cold
// WebView — permanently blocking mount() behind a promise that never
// settles, i.e. the app never gets past a blank screen no matter how long
// you wait. Racing against a timeout means a stuck theme lookup degrades
// to "briefly wrong colors, then mounts" instead of "app never opens."
const themeResolutionTimeoutMs = 1500
const timeout = new Promise<typeof defaultTokens>((resolve) => {
  setTimeout(() => resolve(defaultTokens), themeResolutionTimeoutMs)
})

Promise.race([resolveTheme(loadThemeMode()), timeout])
  .catch(() => defaultTokens)
  .then((tokens) => applyTokens(tokens))
  .finally(() => {
    mount(StandaloneNotesApp, {
      target: document.getElementById('app')!,
    })
  })
