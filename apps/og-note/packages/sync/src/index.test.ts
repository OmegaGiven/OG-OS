import { describe, expect, it, vi } from 'vitest'
import type { ApiClient, RuntimeServices } from '@og-suite/runtime'
import { createMemoryLocalCache, createMemorySyncQueue, createNoopDocumentUpdateChannel, createNoopPresenceChannel } from '@og-suite/runtime'
import type { Note, SyncEnvelope, SyncPullResponse, SyncPushResponse } from '@og-suite/contracts'
import { bootstrapWorkspace, emptyEnvelope, flushQueuedOperations, mergeEnvelope, pullChanges, queueOperation } from './index'

function note(id: string, overrides: Partial<Note> = {}): Note {
  return {
    id,
    documentId: `doc-${id}`,
    title: `Note ${id}`,
    path: '/',
    tags: [],
    ownerId: 'owner-1',
    workspaceId: 'workspace-1',
    createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-01-01T00:00:00.000Z',
    deletedAt: null,
    ...overrides,
  }
}

function fakeApi(overrides: Partial<ApiClient> = {}): ApiClient {
  return {
    get: vi.fn(async () => {
      throw new Error('unexpected GET')
    }),
    post: vi.fn(async () => {
      throw new Error('unexpected POST')
    }),
    put: vi.fn(async () => {
      throw new Error('unexpected PUT')
    }),
    patch: vi.fn(async () => {
      throw new Error('unexpected PATCH')
    }),
    delete: vi.fn(async () => {
      throw new Error('unexpected DELETE')
    }),
    ...overrides,
  }
}

function makeServices(overrides: Partial<RuntimeServices> = {}): RuntimeServices {
  return {
    api: fakeApi(),
    cache: createMemoryLocalCache(),
    syncQueue: createMemorySyncQueue(),
    presence: createNoopPresenceChannel(),
    documentUpdates: createNoopDocumentUpdateChannel(),
    tokens: {} as RuntimeServices['tokens'],
    clientId: 'test-client',
    runtimeMode: 'remote',
    ...overrides,
  }
}

describe('mergeEnvelope', () => {
  it('keeps the newest tombstone and drops the tombstoned note from the merged list', () => {
    const base: SyncEnvelope = {
      ...emptyEnvelope(),
      notes: [note('1')],
    }
    const incoming: SyncEnvelope = {
      ...emptyEnvelope(),
      tombstones: [{ entity: 'notes', id: '1', deletedAt: '2026-01-02T00:00:00.000Z' }],
    }

    const merged = mergeEnvelope(base, incoming)

    expect(merged.notes.map((item) => item.id)).not.toContain('1')
    expect(merged.tombstones).toHaveLength(1)
  })

  it('deduplicates notes by id, keeping the incoming (newer) copy', () => {
    const base: SyncEnvelope = { ...emptyEnvelope(), notes: [note('1', { title: 'Old title' })] }
    const incoming: SyncEnvelope = { ...emptyEnvelope(), notes: [note('1', { title: 'New title' })] }

    const merged = mergeEnvelope(base, incoming)

    expect(merged.notes).toHaveLength(1)
    expect(merged.notes[0].title).toBe('New title')
  })

  it('attaches document updates to their owning document even across merges', () => {
    const base: SyncEnvelope = {
      ...emptyEnvelope(),
      notes: [note('1')],
      documents: [{ id: 'doc-1', kind: 'note', snapshot: '', updates: [], version: 0, compactedAt: null }],
    }
    const incoming: SyncEnvelope = {
      ...emptyEnvelope(),
      documentUpdates: [
        { id: 'u1', documentId: 'doc-1', clientId: 'a', sequence: 1, payload: 'x', createdAt: '2026-01-01T00:00:00.000Z' },
      ],
    }

    const merged = mergeEnvelope(base, incoming)

    expect(merged.documents[0].updates.map((update) => update.id)).toEqual(['u1'])
  })
})

describe('bootstrapWorkspace', () => {
  it('flushes queued local operations before treating the workspace as up to date', async () => {
    const syncQueue = createMemorySyncQueue()
    const pushed: string[] = []
    const services = makeServices({
      syncQueue,
      api: fakeApi({
        post: vi.fn(async (path: string, body: unknown) => {
          if (path === '/api/v1/sync/push') {
            pushed.push(JSON.stringify(body))
            return { ...emptyEnvelope(), notes: [note('1')] } satisfies SyncPushResponse
          }
          throw new Error(`unexpected POST ${path}`)
        }),
      }),
    })
    await syncQueue.enqueue({ kind: 'delete_note', id: '1', deletedAt: '2026-01-01T00:00:00.000Z' })

    const result = await bootstrapWorkspace(services)

    expect(pushed).toHaveLength(1)
    expect(result.notes.map((item) => item.id)).toEqual(['1'])
  })

  it('falls back to the local cache when the server is unreachable', async () => {
    const cachedEnvelope: SyncEnvelope = { ...emptyEnvelope(), notes: [note('cached')] }
    const cache = createMemoryLocalCache(cachedEnvelope)
    const services = makeServices({
      cache,
      api: fakeApi({
        get: vi.fn(async () => {
          throw new Error('network unreachable')
        }),
      }),
    })

    const result = await bootstrapWorkspace(services)

    expect(result.notes.map((item) => item.id)).toEqual(['cached'])
  })
})

describe('queueOperation + flushQueuedOperations', () => {
  it('applies the operation optimistically to the local cache before the server ever sees it', async () => {
    const services = makeServices()
    const created = note('local-only')
    const document = { id: created.documentId, kind: 'note' as const, snapshot: '', updates: [], version: 0, compactedAt: null }

    await queueOperation(services, { kind: 'create_note', note: created, document })

    const cached = await services.cache.loadEnvelope()
    expect(cached?.notes.map((item) => item.id)).toEqual(['local-only'])
  })

  it('clears queued operations only after the server accepts them', async () => {
    const services = makeServices({
      api: fakeApi({
        post: vi.fn(async (path: string) => {
          if (path === '/api/v1/sync/push') return { ...emptyEnvelope() } satisfies SyncPushResponse
          throw new Error(`unexpected POST ${path}`)
        }),
      }),
    })
    const created = note('flush-me')
    const document = { id: created.documentId, kind: 'note' as const, snapshot: '', updates: [], version: 0, compactedAt: null }
    await queueOperation(services, { kind: 'create_note', note: created, document })

    expect(await services.syncQueue.list()).toHaveLength(1)
    await flushQueuedOperations(services)
    expect(await services.syncQueue.list()).toHaveLength(0)
  })

  it('leaves the queue intact if the server push fails, so nothing is lost', async () => {
    const services = makeServices({
      api: fakeApi({
        post: vi.fn(async () => {
          throw new Error('offline')
        }),
      }),
    })
    const created = note('stays-queued')
    const document = { id: created.documentId, kind: 'note' as const, snapshot: '', updates: [], version: 0, compactedAt: null }
    await queueOperation(services, { kind: 'create_note', note: created, document })

    await expect(flushQueuedOperations(services)).rejects.toThrow('offline')
    expect(await services.syncQueue.list()).toHaveLength(1)
  })
})

describe('pullChanges', () => {
  it('flushes queued local writes first instead of letting a pull overwrite them', async () => {
    const calls: string[] = []
    const services = makeServices({
      api: fakeApi({
        post: vi.fn(async (path: string) => {
          calls.push(path)
          if (path === '/api/v1/sync/push') return { ...emptyEnvelope(), notes: [note('local')] } satisfies SyncPushResponse
          if (path === '/api/v1/sync/pull') return { ...emptyEnvelope() } satisfies SyncPullResponse
          throw new Error(`unexpected POST ${path}`)
        }),
      }),
    })
    const created = note('local')
    const document = { id: created.documentId, kind: 'note' as const, snapshot: '', updates: [], version: 0, compactedAt: null }
    await queueOperation(services, { kind: 'create_note', note: created, document })

    const result = await pullChanges(services)

    expect(calls).toEqual(['/api/v1/sync/push'])
    expect(result.notes.map((item) => item.id)).toEqual(['local'])
  })
})
