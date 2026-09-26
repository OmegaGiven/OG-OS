import type { SyncEnvelope, SyncOperation, SyncPullResponse, SyncPushResponse } from '@og-suite/contracts'
import type { QueuedOperation, RuntimeServices } from '@og-suite/runtime'

export function emptyEnvelope(): SyncEnvelope {
  return {
    cursors: { generatedAt: new Date(0).toISOString() },
    apps: [],
    noteFolders: [],
    notes: [],
    documents: [],
    documentUpdates: [],
    tombstones: [],
    conflicts: [],
  }
}

export function mergeEnvelope(current: SyncEnvelope | null, incoming: SyncEnvelope): SyncEnvelope {
  const base = current ?? emptyEnvelope()
  const tombstones = new Map(base.tombstones.map((item) => [`${item.entity}:${item.id}`, item] as const))
  for (const tombstone of incoming.tombstones ?? []) tombstones.set(`${tombstone.entity}:${tombstone.id}`, tombstone)
  const deleted = new Set(Array.from(tombstones.values()).map((item) => `${item.entity}:${item.id}`))
  const documentUpdates = uniqueBy([...(base.documentUpdates ?? []), ...(incoming.documentUpdates ?? [])], (item) => item.id)
  const documents = uniqueBy([...(base.documents ?? []), ...(incoming.documents ?? [])], (item) => item.id)
    .map((document) => ({
      ...document,
      updates: uniqueBy(
        [...document.updates, ...documentUpdates.filter((update) => update.documentId === document.id)],
        (update) => update.id,
      ),
    }))
    .filter((doc) => !deleted.has(`documents:${doc.id}`))
  return {
    cursors: incoming.cursors,
    apps: uniqueBy([...(base.apps ?? []), ...(incoming.apps ?? [])], (item) => item.id),
    noteFolders: uniqueBy([...(base.noteFolders ?? []), ...(incoming.noteFolders ?? [])], (item) => item.id).filter(
      (folder) => !deleted.has(`noteFolders:${folder.id}`),
    ),
    notes: uniqueBy([...(base.notes ?? []), ...(incoming.notes ?? [])], (item) => item.id).filter((note) => !deleted.has(`notes:${note.id}`)),
    documents,
    documentUpdates,
    tombstones: Array.from(tombstones.values()),
    conflicts: incoming.conflicts,
  }
}

export function normalizeRemoteEnvelope(incoming: SyncEnvelope): SyncEnvelope {
  return mergeEnvelope(emptyEnvelope(), incoming)
}

export async function bootstrapWorkspace(services: RuntimeServices): Promise<SyncEnvelope> {
  const queued = await services.syncQueue.list()
  if (queued.length > 0) {
    try {
      return await flushQueuedOperations(services)
    } catch {
      return (await services.cache.loadEnvelope()) ?? emptyEnvelope()
    }
  }

  try {
    const remote = await services.api.get<SyncEnvelope>('/api/v1/sync/bootstrap')
    const merged = normalizeRemoteEnvelope(remote)
    await services.cache.saveEnvelope(merged)
    return merged
  } catch {
    return (await services.cache.loadEnvelope()) ?? emptyEnvelope()
  }
}

export async function queueOperation(services: RuntimeServices, operation: SyncOperation): Promise<QueuedOperation> {
  const current = (await services.cache.loadEnvelope()) ?? emptyEnvelope()
  const optimistic = mergeEnvelope(current, envelopeFromOperation(operation))
  await services.cache.saveEnvelope(optimistic)
  return services.syncQueue.enqueue(operation)
}

export async function flushQueuedOperations(services: RuntimeServices): Promise<SyncEnvelope> {
  const queued = await services.syncQueue.list()
  if (queued.length === 0) {
    return (await services.cache.loadEnvelope()) ?? emptyEnvelope()
  }
  const response = await services.api.post<SyncPushResponse>('/api/v1/sync/push', {
    operations: queued.map((item) => item.operation),
  })
  await services.syncQueue.remove(queued.map((item) => item.id))
  const merged = normalizeRemoteEnvelope(response)
  await services.cache.saveEnvelope(merged)
  return merged
}

export async function pullChanges(services: RuntimeServices): Promise<SyncEnvelope> {
  const queued = await services.syncQueue.list()
  if (queued.length > 0) return flushQueuedOperations(services)

  const current = (await services.cache.loadEnvelope()) ?? emptyEnvelope()
  const response = await services.api.post<SyncPullResponse>('/api/v1/sync/pull', { cursors: current.cursors })
  const merged = normalizeRemoteEnvelope(response)
  await services.cache.saveEnvelope(merged)
  return merged
}

function envelopeFromOperation(operation: SyncOperation): SyncEnvelope {
  const envelope = emptyEnvelope()
  if (operation.kind === 'create_note') {
    envelope.notes = [operation.note]
    envelope.documents = [operation.document]
  }
  if (operation.kind === 'update_note_metadata') envelope.notes = [operation.note]
  if (operation.kind === 'delete_note') envelope.tombstones = [{ entity: 'notes', id: operation.id, deletedAt: operation.deletedAt }]
  if (operation.kind === 'create_note_folder') envelope.noteFolders = [operation.folder]
  if (operation.kind === 'delete_note_folder') envelope.tombstones = [{ entity: 'noteFolders', id: operation.id, deletedAt: operation.deletedAt }]
  if (operation.kind === 'append_document_update') envelope.documentUpdates = [operation.update]
  return envelope
}

function uniqueBy<T>(items: T[], key: (item: T) => string): T[] {
  const map = new Map<string, T>()
  for (const item of items) map.set(key(item), item)
  return Array.from(map.values())
}
