import type { DesignTokens } from '@og-suite/contracts'
import { applyTokens, defaultTokens, loadStoredTokens, saveStoredTokens } from '@og-suite/ui'

/**
 * og-theme.json v1 — the plain-JSON serialization of og-config's Rust
 * `Config` struct (bar_bg/sec_bg/accent/accent2/bar_text/corner_radius),
 * kept intentionally small: this is the interchange shape every
 * OG-toolkit app agrees on, not the full appearance system any one app
 * happens to have. Extra fields in the real file (gaps, waybar layout,
 * etc.) are ignored — we only read what's shared.
 */
export type OgTheme = {
  bar_bg: string
  sec_bg: string
  accent: string
  accent2: string
  bar_text: string
  corner_radius: number
  gradient_enabled?: boolean
}

export type ThemeMode = 'system' | 'independent'

const themeModeKey = 'og-toolkit:theme-mode'
const systemThemePath = '$HOME/.config/sway-power/config.json'

export function loadThemeMode(defaultMode: ThemeMode = 'independent'): ThemeMode {
  if (typeof localStorage === 'undefined') return defaultMode
  const stored = localStorage.getItem(themeModeKey)
  return stored === 'system' || stored === 'independent' ? stored : defaultMode
}

export function saveThemeMode(mode: ThemeMode) {
  if (typeof localStorage === 'undefined') return
  localStorage.setItem(themeModeKey, mode)
}

function isTauri(): boolean {
  return typeof window !== 'undefined' && ('__TAURI_INTERNALS__' in window || '__TAURI__' in window)
}

/**
 * Reads the sway toolkit's shared theme file (written by og-settings) if
 * this is a Tauri build running where that file exists — desktop Linux,
 * in practice. Returns null anywhere else (Windows/macOS/mobile, or a
 * browser build with no filesystem access at all) so callers can fall
 * back to an independent theme without treating that as an error.
 */
export async function readSystemTheme(): Promise<OgTheme | null> {
  if (!isTauri()) return null
  try {
    const { readTextFile, exists } = await import('@tauri-apps/plugin-fs')
    if (!(await exists(systemThemePath))) return null
    const raw = await readTextFile(systemThemePath)
    const parsed = JSON.parse(raw) as Partial<OgTheme>
    if (!parsed.bar_bg || !parsed.accent) return null
    return {
      bar_bg: parsed.bar_bg,
      sec_bg: parsed.sec_bg ?? parsed.bar_bg,
      accent: parsed.accent,
      accent2: parsed.accent2 ?? parsed.accent,
      bar_text: parsed.bar_text ?? '#e0e0e0',
      corner_radius: parsed.corner_radius ?? 0,
      gradient_enabled: parsed.gradient_enabled ?? false,
    }
  } catch {
    // File missing, unreadable, or not valid og-theme shape — treat the
    // same as "no system theme available."
    return null
  }
}

/**
 * Maps the small og-theme.json surface onto the richer DesignTokens shape
 * OG-Suite's appearance system already uses, layering just the shared
 * fields over a base (so unrelated tokens — density, fonts, shadows —
 * keep sane values instead of being reset).
 */
export function ogThemeToDesignTokens(theme: OgTheme, base: DesignTokens = defaultTokens): DesignTokens {
  return {
    ...base,
    colorBackground: theme.bar_bg,
    colorSection: theme.sec_bg,
    colorPanel: theme.sec_bg,
    colorAccent: theme.accent,
    colorAccentBorder: theme.accent2,
    colorText: theme.bar_text,
    radius: theme.corner_radius,
  }
}

/**
 * Resolves the tokens to actually render with, respecting the user's
 * follow-system/independent choice. Falls back to independent (stored,
 * or shipped defaults) whenever there's no system theme to read —
 * "follow system" is a preference, not a hard requirement to find one.
 */
export async function resolveTheme(mode: ThemeMode, storageKey?: string): Promise<DesignTokens> {
  if (mode === 'system') {
    const systemTheme = await readSystemTheme()
    if (systemTheme) return ogThemeToDesignTokens(systemTheme)
  }
  return loadStoredTokens(storageKey)
}

export { applyTokens, defaultTokens, loadStoredTokens, saveStoredTokens }
