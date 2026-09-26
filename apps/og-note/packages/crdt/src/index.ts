import type { CrdtDocumentState, CrdtUpdate } from '@og-suite/contracts'
import * as Y from 'yjs'

const textKey = 'content'
const richContentKey = 'richContent'
const clientSchemaVersion = 2
const legacySnapshotClientId = 1

export type TextDocumentReplica = {
  id: string
  text: string
  version: number
}

export function createDocumentState(id: string, kind: CrdtDocumentState['kind'], text = ''): CrdtDocumentState {
  const doc = createYDoc(text)
  return {
    id,
    kind,
    snapshot: encodeUpdate(Y.encodeStateAsUpdate(doc)),
    updates: [],
    version: 0,
    compactedAt: null,
  }
}

export function applyUpdates(state: CrdtDocumentState): TextDocumentReplica {
  const doc = hydrateYDoc(state)
  return {
    id: state.id,
    text: doc.getText(textKey).toString(),
    version: state.version,
  }
}

export function createTextReplacementUpdate(
  documentId: string,
  clientId: string,
  sequence: number,
  text: string,
  baseState?: CrdtDocumentState | null,
): Omit<CrdtUpdate, 'id' | 'createdAt'> {
  const doc = baseState ? hydrateYDoc(baseState) : createYDoc()
  const stateVector = Y.encodeStateVector(doc)
  replaceTextWithMinimalEdit(doc.getText(textKey), text)
  return {
    documentId,
    clientId,
    sequence,
    payload: encodeUpdate(Y.encodeStateAsUpdate(doc, stateVector)),
    clientSchemaVersion,
  }
}

export function createTextDiffUpdate(
  documentId: string,
  clientId: string,
  sequence: number,
  previousText: string,
  nextText: string,
  baseState?: CrdtDocumentState | null,
): Omit<CrdtUpdate, 'id' | 'createdAt'> {
  const doc = baseState ? hydrateYDoc(baseState) : createYDoc()
  const stateVector = Y.encodeStateVector(doc)
  applyMappedTextDiff(doc.getText(textKey), previousText, nextText)
  return {
    documentId,
    clientId,
    sequence,
    payload: encodeUpdate(Y.encodeStateAsUpdate(doc, stateVector)),
    clientSchemaVersion,
  }
}

function hydrateYDoc(state: CrdtDocumentState): Y.Doc {
  const doc = createYDoc()
  applySnapshot(doc, state.snapshot)
  for (const update of orderedUpdates(state.updates)) applyStoredUpdate(doc, update.payload)
  return doc
}

/**
 * Live-editing entry point for the Rich editor: unlike everything else in
 * this module, which reconstructs a Y.Doc just long enough to read a
 * derived value (text, a diff) out of it, this hands back the actual
 * mutable Y.Doc so a real editor binding (Tiptap's Collaboration
 * extension) can write ProseMirror transactions directly into it as they
 * happen. That's what makes edits synchronous and un-losable: there's no
 * "diff two snapshots later" step in between the keystroke and the CRDT
 * state.
 */
export function hydrateLiveYDoc(state: CrdtDocumentState): Y.Doc {
  return hydrateYDoc(state)
}

export function getRichFragment(doc: Y.Doc): Y.XmlFragment {
  return doc.getXmlFragment(richContentKey)
}

export function getDocStateVector(doc: Y.Doc): Uint8Array {
  return Y.encodeStateVector(doc)
}

/**
 * Applies a remote CrdtUpdate directly onto a live Y.Doc (as opposed to
 * applyUpdates, which reconstructs a throwaway doc just to read text back
 * out). Used for the "another device is editing this note right now" path
 * — Y.applyUpdate is commutative/idempotent, so this is safe to call even
 * if the update turns out to already be reflected in the doc.
 */
export function applyEncodedUpdate(doc: Y.Doc, payload: string): void {
  if (!payload) return
  Y.applyUpdate(doc, decodeUpdate(payload))
}

/**
 * Reconciles a live editing Y.Doc with a CrdtDocumentState fetched from the
 * server/envelope (a pull, a periodic refresh, a fresh document fetch) —
 * without ever replacing the live doc's content wholesale. Hydrates the
 * server state into a throwaway doc, diffs it against what the live doc
 * already has, and applies only the missing delta. Safe to call even if
 * the server state is stale relative to the live doc (the diff is then
 * empty and this is a no-op) — this is what makes it safe to use as the
 * general "sync in whatever's new" path instead of every caller needing
 * its own staleness check before deciding whether to touch the live doc.
 */
export function syncLiveDocFromState(liveDoc: Y.Doc, state: CrdtDocumentState): void {
  const remoteDoc = hydrateYDoc(state)
  const delta = Y.encodeStateAsUpdate(remoteDoc, Y.encodeStateVector(liveDoc))
  if (delta.length > 2) Y.applyUpdate(liveDoc, delta)
}

/**
 * Rich mode's edits live in a separate Yjs field (richContent, a
 * Y.XmlFragment) from TXT/MD's plain "content" Y.Text — they used to only
 * get reconciled at an explicit mode-switch boundary within the same
 * session. Two devices with different *persisted* mode preferences (the
 * toggle choice is saved to localStorage per app install) never hit that
 * boundary at all: one always writes richContent, the other always reads
 * "content", and they silently diverge — device B's TXT/MD view shows
 * nothing device A typed in Rich mode. Calling this alongside every Rich
 * save keeps "content" continuously up to date (via the same doc's shared
 * update, not a separate round trip) so any TXT/MD viewer sees current
 * text regardless of which mode last edited it. This module only handles
 * this direction — going the other way (plain edit -> richContent) needs
 * a real ProseMirror schema/editor to build valid XmlFragment content,
 * which this framework-agnostic package intentionally doesn't depend on.
 * The app layer (NotesApp.svelte's syncRichFragmentFromMarkdown) provides
 * that other direction via a detached Tiptap editor bound to the same
 * live doc.
 */
export function syncPlainTextFieldFromMarkdown(doc: Y.Doc, markdown: string): void {
  replaceTextWithMinimalEdit(doc.getText(textKey), markdown)
}

/**
 * Encodes everything that changed in `doc` since `sinceStateVector` as one
 * Yjs update, ready to hand to createTextDiffUpdate's sibling call sites
 * (queueOperation/publishUpdate) as a CrdtUpdate payload. Returns null if
 * nothing changed — callers should skip persisting/broadcasting a no-op.
 */
export function encodeDocUpdateSince(doc: Y.Doc, sinceStateVector: Uint8Array): string | null {
  const delta = Y.encodeStateAsUpdate(doc, sinceStateVector)
  // An update encoding "nothing changed" is still a few bytes of Yjs
  // protocol framing, not zero-length — compare against a fresh doc's
  // empty delta instead of checking length === 0.
  if (delta.length <= 2) return null
  return encodeUpdate(delta)
}

function createYDoc(text = '') {
  const doc = new Y.Doc()
  if (text) doc.getText(textKey).insert(0, text)
  return doc
}

function orderedUpdates(updates: CrdtUpdate[]) {
  return [...updates].sort((left, right) => {
    if (left.sequence !== right.sequence) return left.sequence - right.sequence
    return left.clientId.localeCompare(right.clientId)
  })
}

function applySnapshot(doc: Y.Doc, payload: string) {
  if (!payload) return
  try {
    Y.applyUpdate(doc, decodeUpdate(payload))
  } catch {
    const text = doc.getText(textKey)
    if (text.length > 0) return
    const clientId = doc.clientID
    doc.clientID = legacySnapshotClientId
    text.insert(0, payload)
    doc.clientID = clientId
  }
}

function applyStoredUpdate(doc: Y.Doc, payload: string) {
  if (!payload) return
  Y.applyUpdate(doc, decodeUpdate(payload))
}

function replaceTextWithMinimalEdit(yText: Y.Text, nextText: string) {
  const currentText = yText.toString()
  if (currentText === nextText) return

  let prefixLength = 0
  const minLength = Math.min(currentText.length, nextText.length)
  while (prefixLength < minLength && currentText[prefixLength] === nextText[prefixLength]) {
    prefixLength += 1
  }

  let suffixLength = 0
  while (
    suffixLength < currentText.length - prefixLength
    && suffixLength < nextText.length - prefixLength
    && currentText[currentText.length - 1 - suffixLength] === nextText[nextText.length - 1 - suffixLength]
  ) {
    suffixLength += 1
  }

  const deleteLength = currentText.length - prefixLength - suffixLength
  const insertText = nextText.slice(prefixLength, nextText.length - suffixLength)
  if (deleteLength > 0) yText.delete(prefixLength, deleteLength)
  if (insertText) yText.insert(prefixLength, insertText)
}

function applyMappedTextDiff(yText: Y.Text, previousText: string, nextText: string) {
  if (previousText === nextText) return
  const currentText = yText.toString()
  const diff = textDiff(previousText, nextText)
  const start = mapTextPosition(previousText, currentText, diff.start)
  const end = mapTextPosition(previousText, currentText, diff.start + diff.deleteLength)
  const deleteLength = Math.max(0, end - start)
  if (deleteLength > 0) yText.delete(start, deleteLength)
  if (diff.insertText) yText.insert(start, diff.insertText)
}

function textDiff(previousText: string, nextText: string) {
  let prefixLength = 0
  const minLength = Math.min(previousText.length, nextText.length)
  while (prefixLength < minLength && previousText[prefixLength] === nextText[prefixLength]) {
    prefixLength += 1
  }

  let suffixLength = 0
  while (
    suffixLength < previousText.length - prefixLength
    && suffixLength < nextText.length - prefixLength
    && previousText[previousText.length - 1 - suffixLength] === nextText[nextText.length - 1 - suffixLength]
  ) {
    suffixLength += 1
  }

  return {
    start: prefixLength,
    deleteLength: previousText.length - prefixLength - suffixLength,
    insertText: nextText.slice(prefixLength, nextText.length - suffixLength),
  }
}

function mapTextPosition(previousText: string, currentText: string, position: number) {
  let prefixLength = 0
  const shortestLength = Math.min(previousText.length, currentText.length)
  while (prefixLength < shortestLength && previousText[prefixLength] === currentText[prefixLength]) prefixLength += 1

  let suffixLength = 0
  while (
    suffixLength < previousText.length - prefixLength
    && suffixLength < currentText.length - prefixLength
    && previousText[previousText.length - 1 - suffixLength] === currentText[currentText.length - 1 - suffixLength]
  ) {
    suffixLength += 1
  }

  const previousChangedEnd = previousText.length - suffixLength
  if (position <= prefixLength) return position
  if (position >= previousChangedEnd) return Math.max(0, position + currentText.length - previousText.length)
  return currentText.length - suffixLength
}

function encodeUpdate(update: Uint8Array) {
  if (typeof Buffer !== 'undefined') return Buffer.from(update).toString('base64')
  let binary = ''
  for (const byte of update) binary += String.fromCharCode(byte)
  return btoa(binary)
}

function decodeUpdate(payload: string) {
  if (typeof Buffer !== 'undefined') return new Uint8Array(Buffer.from(payload, 'base64'))
  const binary = atob(payload)
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index)
  return bytes
}
