import {
  createBrowserLocalCache,
  createBrowserSyncQueue,
  createHttpApiClient,
  createNoopDocumentUpdateChannel,
  createNoopPresenceChannel,
  createOfflineApiClient,
  createRuntimeId,
  createWebSocketDocumentUpdates,
  createWebSocketPresence,
} from '@og-suite/runtime'
import { loadStoredTokens } from '@og-suite/ui'
import type { LocalCache, RuntimeServices } from '@og-suite/runtime'
import { createFileSystemLocalCache } from './fileStorage'

function loadClientId() {
  const clientId = localStorage.getItem('og-suite:client-id') ?? createRuntimeId('client')
  localStorage.setItem('og-suite:client-id', clientId)
  return clientId
}

function isTauri() {
  return typeof window !== 'undefined' && ('__TAURI_INTERNALS__' in window || '__TAURI__' in window)
}

/**
 * Sessions expire server-side after 12h. Without this, staying signed in
 * "between version bumps" only worked by luck — as soon as the access
 * token expired, every request started 401ing and the app had no way to
 * recover except a manual sign-in. Called by the API client whenever a
 * request 401s; on success it persists the new tokens so the refreshed
 * session survives the next app launch too.
 */
function createTokenRefresher(baseUrl: string) {
  return async () => {
    const refreshToken = localStorage.getItem('og-suite:auth:refresh-token')
    if (!refreshToken) return null
    try {
      const response = await fetch(`${baseUrl}/api/v1/auth/refresh`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ refreshToken }),
      })
      if (!response.ok) throw new Error(`refresh failed with ${response.status}`)
      const session = (await response.json()) as { accessToken: string; refreshToken: string; expiresAt: string }
      localStorage.setItem('og-suite:auth:access-token', session.accessToken)
      localStorage.setItem('og-suite:auth:refresh-token', session.refreshToken)
      localStorage.setItem('og-suite:auth:expires-at', session.expiresAt)
      return session.accessToken
    } catch {
      // Refresh token is gone/expired too — clear the dead session so the
      // UI falls back to the sign-in screen instead of looping on 401s.
      localStorage.removeItem('og-suite:auth:access-token')
      localStorage.removeItem('og-suite:auth:refresh-token')
      localStorage.removeItem('og-suite:auth:expires-at')
      return null
    }
  }
}

/**
 * Real files on disk (see fileStorage.ts) whenever we're a Tauri build
 * with filesystem access; browser localStorage otherwise (`vite dev`
 * without Tauri, or a future plain-web build) — same LocalCache
 * interface either way, so nothing else needs to know which one is
 * active.
 */
function createLocalCache(): LocalCache {
  return isTauri() ? createFileSystemLocalCache() : createBrowserLocalCache('og-suite:notes:workspace')
}

export function createStandaloneRuntime(serverUrl?: string): RuntimeServices {
  const apiHost = typeof window === 'undefined' || window.location.hostname === 'localhost' ? '127.0.0.1' : window.location.hostname
  const defaultApiUrl =
    typeof window === 'undefined' ? 'http://127.0.0.1:8080' : `http://${apiHost}:8080`
  const storedServerUrl = typeof localStorage === 'undefined' ? null : localStorage.getItem('og-suite:server-url')
  const baseUrl = serverUrl ?? import.meta.env.VITE_OG_API_URL ?? storedServerUrl ?? defaultApiUrl
  const clientId = loadClientId()
  return {
    api: createHttpApiClient(
      baseUrl,
      () => localStorage.getItem('og-suite:auth:access-token'),
      createTokenRefresher(baseUrl),
    ),
    cache: createLocalCache(),
    syncQueue: createBrowserSyncQueue('og-suite:notes:sync-queue'),
    presence: createWebSocketPresence(baseUrl, clientId, () => localStorage.getItem('og-suite:auth:access-token')),
    documentUpdates: createWebSocketDocumentUpdates(baseUrl, clientId, () =>
      localStorage.getItem('og-suite:auth:access-token'),
    ),
    tokens: loadStoredTokens(),
    clientId,
    runtimeMode: 'remote',
    serverUrl: baseUrl,
  }
}

export function createLocalOnlyRuntime(): RuntimeServices {
  const clientId = loadClientId()
  return {
    api: createOfflineApiClient(),
    cache: createLocalCache(),
    syncQueue: createBrowserSyncQueue('og-suite:notes:sync-queue'),
    presence: createNoopPresenceChannel(),
    documentUpdates: createNoopDocumentUpdateChannel(),
    tokens: loadStoredTokens(),
    clientId,
    runtimeMode: 'local',
  }
}
