import type {
  AppCapability,
  AppRegistryEntry,
  CrdtUpdate,
  DesignTokens,
  PresencePeer,
  SyncEnvelope,
  SyncOperation,
} from '@og-suite/contracts'

/**
 * Runs async tasks strictly one at a time, in call order — the queued task
 * only starts once every task ahead of it has fully settled. Use this for
 * any operation whose correctness depends on reading state that a prior
 * call of the same operation just wrote (a text-diff save that reads
 * "what changed since last save", for example): without serialization, two
 * overlapping calls can each read the same stale pre-write state and
 * silently produce a wrong or lossy result instead of throwing.
 */
export function createSerialQueue() {
  let tail: Promise<unknown> = Promise.resolve()
  return function enqueue<T>(task: () => Promise<T>): Promise<T> {
    const result = tail.then(task, task)
    tail = result.catch(() => undefined)
    return result
  }
}

export type ToolbarItem =
  | { kind: 'button'; id: string; label: string; command: string }
  | { kind: 'dropdown'; id: string; label: string; options: { label: string; command: string }[] }

export type AppManifest = {
  id: string
  name: string
  route: string
  standaloneRoute: string
  capabilities: AppCapability[]
  toolbar: ToolbarItem[]
}

export type ApiClient = {
  get<T>(path: string): Promise<T>
  post<T>(path: string, body: unknown): Promise<T>
  put<T>(path: string, body: unknown): Promise<T>
  patch<T>(path: string, body: unknown): Promise<T>
  delete(path: string): Promise<void>
}

export type LocalCache = {
  loadEnvelope(): Promise<SyncEnvelope | null>
  saveEnvelope(envelope: SyncEnvelope): Promise<void>
}

export type SyncQueue = {
  list(): Promise<QueuedOperation[]>
  enqueue(operation: SyncOperation): Promise<QueuedOperation>
  remove(ids: string[]): Promise<void>
}

export type PresenceChannel = {
  connect(documentId: string, onPeers: (peers: PresencePeer[]) => void): () => void
  publishCursor(documentId: string, cursor: number | null): void
}

export type DocumentUpdateChannel = {
  connect(
    documentId: string,
    onUpdate: (update: CrdtUpdate) => void,
    onAck?: (ack: { updateId: string; documentId: string; documentVersion?: number }) => void,
    onError?: (error: { updateId?: string; message: string }) => void,
  ): () => void
  publishUpdate(documentId: string, update: CrdtUpdate): boolean
}

export type RuntimeServices = {
  api: ApiClient
  cache: LocalCache
  syncQueue: SyncQueue
  presence: PresenceChannel
  documentUpdates: DocumentUpdateChannel
  tokens: DesignTokens
  clientId: string
  runtimeMode?: 'remote' | 'local'
  serverUrl?: string
}

export type RegisteredApp = {
  manifest: AppManifest
  registryEntry: AppRegistryEntry
}

export type QueuedOperation = {
  id: string
  operation: SyncOperation
  createdAt: string
}

export function createRuntimeId(prefix = 'id'): string {
  if (globalThis.crypto && typeof globalThis.crypto.randomUUID === 'function') {
    return globalThis.crypto.randomUUID()
  }
  if (globalThis.crypto && typeof globalThis.crypto.getRandomValues === 'function') {
    const bytes = new Uint8Array(16)
    globalThis.crypto.getRandomValues(bytes)
    bytes[6] = (bytes[6] & 0x0f) | 0x40
    bytes[8] = (bytes[8] & 0x3f) | 0x80
    const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')
    return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
  }
  return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`
}

export function registerApp(manifest: AppManifest): RegisteredApp {
  return {
    manifest,
    registryEntry: {
      id: manifest.id,
      name: manifest.name,
      route: manifest.route,
      standaloneRoute: manifest.standaloneRoute,
      capabilities: manifest.capabilities,
    },
  }
}

export function createHttpApiClient(
  baseUrl: string,
  getAccessToken?: () => string | null,
  onUnauthorized?: () => Promise<string | null>,
): ApiClient {
  // Sessions expire (12h server-side); without this every client would
  // silently get logged out mid-day and have to re-enter credentials.
  // A single in-flight refresh is shared so a burst of 401s (several
  // requests failing around the same expiry moment) only triggers one
  // refresh call instead of a stampede.
  let refreshInFlight: Promise<string | null> | null = null

  async function fetchOnce(method: string, path: string, body: unknown, accessToken: string | null) {
    return fetch(`${baseUrl}${path}`, {
      method,
      headers: {
        ...(body === undefined ? {} : { 'content-type': 'application/json' }),
        ...(accessToken ? { authorization: `Bearer ${accessToken}` } : {}),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
  }

  async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const accessToken = getAccessToken?.() ?? null
    let response = await fetchOnce(method, path, body, accessToken)
    if (response.status === 401 && onUnauthorized && path !== '/api/v1/auth/refresh') {
      refreshInFlight ??= onUnauthorized().finally(() => {
        refreshInFlight = null
      })
      const refreshedToken = await refreshInFlight
      if (refreshedToken) response = await fetchOnce(method, path, body, refreshedToken)
    }
    if (!response.ok) {
      const message = await response.text().catch(() => '')
      throw new Error(`${method} ${path} failed with ${response.status}${message ? `: ${message}` : ''}`)
    }
    if (response.status === 204) return undefined as T
    return (await response.json()) as T
  }
  return {
    get: (path) => request('GET', path),
    post: (path, body) => request('POST', path, body),
    put: (path, body) => request('PUT', path, body),
    patch: (path, body) => request('PATCH', path, body),
    delete: (path) => request('DELETE', path),
  }
}

export function createOfflineApiClient(message = 'Local-only mode is not connected to a server.'): ApiClient {
  async function unavailable<T>(): Promise<T> {
    throw new Error(message)
  }
  return {
    get: () => unavailable(),
    post: () => unavailable(),
    put: () => unavailable(),
    patch: () => unavailable(),
    delete: () => unavailable(),
  }
}

export function createBrowserLocalCache(key = 'og-suite:workspace'): LocalCache {
  function serializeEnvelope(envelope: SyncEnvelope, includeDocumentUpdates = true) {
    return JSON.stringify(includeDocumentUpdates ? envelope : { ...envelope, documentUpdates: [] })
  }

  return {
    async loadEnvelope() {
      const raw = localStorage.getItem(key)
      return raw ? (JSON.parse(raw) as SyncEnvelope) : null
    },
    async saveEnvelope(envelope) {
      try {
        localStorage.setItem(key, serializeEnvelope(envelope))
      } catch (error) {
        try {
          localStorage.setItem(key, serializeEnvelope(envelope, false))
          console.warn(`Saved compact workspace cache for ${key} after full cache write failed.`, error)
        } catch (compactError) {
          console.warn(`Skipped workspace cache save for ${key}; browser storage is full or unavailable.`, compactError)
        }
      }
    },
  }
}

export function createMemoryLocalCache(initialEnvelope: SyncEnvelope | null = null): LocalCache {
  let envelope = initialEnvelope
  return {
    async loadEnvelope() {
      return envelope
    },
    async saveEnvelope(nextEnvelope) {
      envelope = nextEnvelope
    },
  }
}

export function createBrowserSyncQueue(key = 'og-suite:sync-queue'): SyncQueue {
  function load(): QueuedOperation[] {
    const raw = localStorage.getItem(key)
    return raw ? (JSON.parse(raw) as QueuedOperation[]) : []
  }
  function save(items: QueuedOperation[]) {
    localStorage.setItem(key, JSON.stringify(items))
  }
  return {
    async list() {
      return load()
    },
    async enqueue(operation) {
      const queued = {
        id: createRuntimeId('queue'),
        operation,
        createdAt: new Date().toISOString(),
      }
      save([...load(), queued])
      return queued
    },
    async remove(ids) {
      const idSet = new Set(ids)
      save(load().filter((item) => !idSet.has(item.id)))
    },
  }
}

export function createMemorySyncQueue(initialItems: QueuedOperation[] = []): SyncQueue {
  let items = [...initialItems]
  return {
    async list() {
      return [...items]
    },
    async enqueue(operation) {
      const queued = {
        id: createRuntimeId('queue'),
        operation,
        createdAt: new Date().toISOString(),
      }
      items = [...items, queued]
      return queued
    },
    async remove(ids) {
      const idSet = new Set(ids)
      items = items.filter((item) => !idSet.has(item.id))
    },
  }
}

export function createNoopPresenceChannel(): PresenceChannel {
  return {
    connect: () => () => {},
    publishCursor: () => {},
  }
}

export function createNoopDocumentUpdateChannel(): DocumentUpdateChannel {
  return {
    connect: () => () => {},
    publishUpdate: () => false,
  }
}

export function createWebSocketPresence(
  baseUrl: string,
  clientId: string,
  getToken?: () => string | null,
): PresenceChannel {
  const sockets = new Map<string, WebSocket>()
  return {
    connect(documentId, onPeers) {
      const token = getToken?.() ?? ''
      const url = `${baseUrl.replace(/^http/, 'ws')}/ws/presence/${documentId}?client_id=${encodeURIComponent(clientId)}&token=${encodeURIComponent(token)}`
      const socket = new WebSocket(url)
      sockets.set(documentId, socket)
      socket.addEventListener('message', (event) => {
        const payload = JSON.parse(String(event.data)) as { peers?: PresencePeer[] }
        onPeers(payload.peers ?? [])
      })
      return () => {
        sockets.delete(documentId)
        socket.close()
      }
    },
    publishCursor(documentId, cursor) {
      const socket = sockets.get(documentId)
      if (socket?.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ kind: 'cursor', cursor }))
      }
    },
  }
}

export function createWebSocketDocumentUpdates(
  baseUrl: string,
  clientId: string,
  getToken?: () => string | null,
): DocumentUpdateChannel {
  const sockets = new Map<string, WebSocket>()
  const pending = new Map<string, CrdtUpdate[]>()

  function flushPending(documentId: string) {
    const socket = sockets.get(documentId)
    if (socket?.readyState !== WebSocket.OPEN) return false
    const updates = pending.get(documentId) ?? []
    for (const update of updates) socket.send(JSON.stringify({ kind: 'update', update }))
    pending.delete(documentId)
    return true
  }

  return {
    connect(documentId, onUpdate, onAck, onError) {
      const token = getToken?.() ?? ''
      const url = `${baseUrl.replace(/^http/, 'ws')}/ws/documents/${documentId}?client_id=${encodeURIComponent(clientId)}&token=${encodeURIComponent(token)}`
      const socket = new WebSocket(url)
      sockets.set(documentId, socket)
      socket.addEventListener('open', () => {
        flushPending(documentId)
      })
      socket.addEventListener('message', (event) => {
        const payload = JSON.parse(String(event.data)) as {
          kind?: 'update' | 'ack' | 'error'
          update?: CrdtUpdate
          updateId?: string
          documentId?: string
          documentVersion?: number
          message?: string
        }
        if (payload.kind === 'ack' && payload.updateId && payload.documentId) {
          onAck?.({ updateId: payload.updateId, documentId: payload.documentId, documentVersion: payload.documentVersion })
          return
        }
        if (payload.kind === 'error') {
          onError?.({ updateId: payload.updateId, message: payload.message ?? 'Live document update failed.' })
          return
        }
        if (payload.update && payload.update.clientId !== clientId) onUpdate(payload.update)
      })
      return () => {
        sockets.delete(documentId)
        socket.close()
      }
    },
    publishUpdate(documentId, update) {
      const socket = sockets.get(documentId)
      if (socket?.readyState !== WebSocket.OPEN) {
        pending.set(documentId, [...(pending.get(documentId) ?? []), update])
        return false
      }
      socket.send(JSON.stringify({ kind: 'update', update }))
      return true
    },
  }
}
