import type { CrdtDocumentState, Note, SyncEnvelope } from '@og-suite/contracts'
import { applyUpdates, createDocumentState } from '@og-suite/crdt'
import type { LocalCache } from '@og-suite/runtime'
import { exists, mkdir, readTextFile, writeTextFile } from '@tauri-apps/plugin-fs'
import { appLocalDataDir, homeDir, join } from '@tauri-apps/api/path'

/**
 * Real on-disk storage for local-only mode: notes as plain, portable
 * `.md` files under `~/OGNote/`, not a JSON blob in browser localStorage.
 * The point (per the design doc) is that a note stays readable/openable
 * in any other editor even if OG Note itself is gone — so the markdown
 * text is the file, and everything else (folders, note metadata, sync
 * cursors) is a small side index next to it, not the source of truth.
 */

const vaultDirName = 'OGNote'
const notesDirName = 'notes'
const indexFileName = 'index.json'

type StoredIndex = Omit<SyncEnvelope, 'documents' | 'documentUpdates'>

function isAndroid() {
  return typeof navigator !== 'undefined' && /Android/i.test(navigator.userAgent)
}

async function vaultRoot() {
  // Android's scoped storage blocks writes under the shared home dir
  // (/storage/emulated/0) without extra runtime permissions, so mobile
  // uses the app-private local data dir instead of ~/OGNote.
  const base = isAndroid() ? await appLocalDataDir() : await homeDir()
  return join(base, vaultDirName)
}

async function notesDir() {
  return join(await vaultRoot(), notesDirName)
}

async function indexPath() {
  return join(await vaultRoot(), indexFileName)
}

function slugForNote(note: Note): string {
  const base = note.path.trim() || note.title.trim() || note.id
  const cleaned = base
    .replace(/[\\/]+/g, '-')
    .replace(/[^a-zA-Z0-9-_ ]/g, '')
    .trim()
  return cleaned || note.id
}

export function createFileSystemLocalCache(): LocalCache {
  return {
    async loadEnvelope() {
      const idxPath = await indexPath()
      if (!(await exists(idxPath))) return null

      const index = JSON.parse(await readTextFile(idxPath)) as StoredIndex
      const dir = await notesDir()
      const documents: CrdtDocumentState[] = []
      for (const note of index.notes) {
        const filePath = await join(dir, `${slugForNote(note)}.md`)
        const text = (await exists(filePath)) ? await readTextFile(filePath) : ''
        documents.push(createDocumentState(note.documentId, 'note', text))
      }
      return { ...index, documents, documentUpdates: [] }
    },

    async saveEnvelope(envelope) {
      const dir = await notesDir()
      await mkdir(dir, { recursive: true })

      for (const note of envelope.notes) {
        const state = envelope.documents.find((document) => document.id === note.documentId)
        if (!state) continue
        const { text } = applyUpdates(state)
        const filePath = await join(dir, `${slugForNote(note)}.md`)

        // Keep the previous version rather than silently overwriting it —
        // same rule as a server copy arriving newer than the local one:
        // audit/restore over newest-wins.
        const previous = (await exists(filePath)) ? await readTextFile(filePath) : null
        if (previous !== null && previous !== text) {
          const historyDir = `${filePath}.history`
          await mkdir(historyDir, { recursive: true })
          const stamp = new Date().toISOString().replace(/[:.]/g, '-')
          await writeTextFile(await join(historyDir, `${stamp}.md`), previous)
        }
        await writeTextFile(filePath, text)
      }

      // Deleted notes' .md files are left in place rather than removed —
      // tombstones already track deletion intent in the index; the file
      // itself staying on disk is a feature here; a "delete" that
      // silently removes a plain text file has no undo.
      const { documents: _documents, documentUpdates: _documentUpdates, ...index } = envelope
      await writeTextFile(await indexPath(), JSON.stringify(index))
    },
  }
}
