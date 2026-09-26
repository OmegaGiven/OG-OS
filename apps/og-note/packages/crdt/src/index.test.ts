import { describe, expect, it } from 'vitest'
import type { CrdtUpdate } from '@og-suite/contracts'
import { applyUpdates, createDocumentState, createTextDiffUpdate, createTextReplacementUpdate } from './index'

function updateFromClient(
  documentId: string,
  clientId: string,
  sequence: number,
  text: string,
  base = createDocumentState(documentId, 'note', ''),
): CrdtUpdate {
  return {
    ...createTextReplacementUpdate(documentId, clientId, sequence, text, base),
    id: `${clientId}-${sequence}`,
    createdAt: new Date(sequence).toISOString(),
  }
}

describe('Yjs document CRDT', () => {
  it('merges concurrent inserts from two clients without conflict copies', () => {
    const documentId = 'doc-1'
    const base = createDocumentState(documentId, 'note', '')
    const first = updateFromClient(documentId, 'client-a', 1, 'Alpha', base)
    const second = updateFromClient(documentId, 'client-b', 1, 'Beta', base)

    const merged = applyUpdates({
      ...base,
      updates: [first, second],
      version: 2,
    })

    expect(merged.text).toContain('Alpha')
    expect(merged.text).toContain('Beta')
    expect(merged.text).toHaveLength('AlphaBeta'.length)
  })

  it('preserves edits made against the same prior snapshot', () => {
    const documentId = 'doc-2'
    const base = createDocumentState(documentId, 'note', 'Start')
    const first = updateFromClient(documentId, 'client-a', 1, 'Start A', base)
    const second = updateFromClient(documentId, 'client-b', 1, 'Start B', base)

    const merged = applyUpdates({
      ...base,
      updates: [first, second],
      version: 2,
    })

    expect(merged.text).toContain('Start')
    expect(merged.text).toContain('A')
    expect(merged.text).toContain('B')
  })

  it('maps a local text diff onto a document that already has remote changes', () => {
    const documentId = 'doc-3'
    const base = createDocumentState(documentId, 'note', 'Seed\n')
    const remote = updateFromClient(documentId, 'client-b', 1, 'Seed\nRemote\n', base)
    const localBase = {
      ...base,
      updates: [remote],
      version: 1,
    }
    const local = {
      ...createTextDiffUpdate(documentId, 'client-a', 1, 'Seed\n', 'Seed\nLocal\n', localBase),
      id: 'client-a-1',
      createdAt: new Date(1).toISOString(),
    }

    const merged = applyUpdates({
      ...base,
      updates: [remote, local],
      version: 2,
    })

    expect(merged.text).toContain('Seed')
    expect(merged.text).toContain('Remote')
    expect(merged.text).toContain('Local')
  })

  it('converges repeated concurrent backspace/delete updates across desktop and mobile replicas', () => {
    const documentId = 'doc-delete-stress'
    const base = createDocumentState(documentId, 'note', 'abcdefghijklmnopqrstuvwxyz\n0123456789\n')
    const desktopUpdates: CrdtUpdate[] = []
    const mobileUpdates: CrdtUpdate[] = []
    let desktopState = base
    let mobileState = base
    let desktopText = applyUpdates(base).text
    let mobileText = applyUpdates(base).text

    for (let step = 0; step < 18; step += 1) {
      const desktopNext = deleteAt(desktopText, Math.max(0, desktopText.length - 2 - (step % 7)))
      const mobileNext = deleteAt(mobileText, Math.max(0, (step * 3) % Math.max(1, mobileText.length)))

      const desktopUpdate: CrdtUpdate = {
        ...createTextDiffUpdate(documentId, 'desktop-browser', step + 1, desktopText, desktopNext, desktopState),
        id: `desktop-${step + 1}`,
        createdAt: new Date(step + 1).toISOString(),
      }
      const mobileUpdate: CrdtUpdate = {
        ...createTextDiffUpdate(documentId, 'mobile-app', step + 1, mobileText, mobileNext, mobileState),
        id: `mobile-${step + 1}`,
        createdAt: new Date(step + 1).toISOString(),
      }

      desktopUpdates.push(desktopUpdate)
      mobileUpdates.push(mobileUpdate)
      desktopState = { ...base, updates: interleave(desktopUpdates, mobileUpdates), version: desktopUpdates.length + mobileUpdates.length }
      mobileState = { ...base, updates: interleave(mobileUpdates, desktopUpdates), version: desktopUpdates.length + mobileUpdates.length }
      desktopText = applyUpdates(desktopState).text
      mobileText = applyUpdates(mobileState).text

      expect(desktopText).toBe(mobileText)
    }
  })

  it('can delete from legacy plain-text snapshots created by the server notes API', () => {
    const documentId = 'doc-legacy-plain'
    const legacyState = {
      id: documentId,
      kind: 'note' as const,
      snapshot: 'legacy plain text snapshot',
      updates: [],
      version: 0,
      compactedAt: null,
    }
    const firstDelete: CrdtUpdate = {
      ...createTextDiffUpdate(documentId, 'desktop-browser', 1, 'legacy plain text snapshot', 'legacy plain snapshot', legacyState),
      id: 'desktop-delete',
      createdAt: new Date(1).toISOString(),
    }
    const secondDelete: CrdtUpdate = {
      ...createTextDiffUpdate(documentId, 'mobile-app', 1, 'legacy plain text snapshot', 'legacy text snapshot', legacyState),
      id: 'mobile-delete',
      createdAt: new Date(1).toISOString(),
    }

    const merged = applyUpdates({
      ...legacyState,
      updates: [firstDelete, secondDelete],
      version: 2,
    })

    expect(merged.text).not.toBe(legacyState.snapshot)
    expect(merged.text).not.toContain('plain text')
    expect(merged.text).toContain('legacy')
    expect(merged.text).toContain('snapshot')
  })
})

function deleteAt(text: string, index: number) {
  if (!text) return text
  const clamped = Math.max(0, Math.min(index, text.length - 1))
  return `${text.slice(0, clamped)}${text.slice(clamped + 1)}`
}

function interleave<T>(first: T[], second: T[]) {
  const result: T[] = []
  for (let index = 0; index < Math.max(first.length, second.length); index += 1) {
    if (first[index]) result.push(first[index])
    if (second[index]) result.push(second[index])
  }
  return result
}
