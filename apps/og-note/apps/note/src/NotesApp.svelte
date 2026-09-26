<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import type { CustomFont, DesignTokens } from '@og-suite/contracts'
  import {
    applyEncodedUpdate,
    applyUpdates,
    createDocumentState,
    createTextDiffUpdate,
    encodeDocUpdateSince,
    getDocStateVector,
    getRichFragment,
    hydrateLiveYDoc,
    syncLiveDocFromState,
    syncPlainTextFieldFromMarkdown,
  } from '@og-suite/crdt'
  import type { CrdtDocumentState, CrdtUpdate, DocumentVersionSummary, Note, NoteFolder, PresencePeer, SyncEnvelope, SyncOperation } from '@og-suite/contracts'
  import { createHttpApiClient, createRuntimeId, createSerialQueue } from '@og-suite/runtime'
  import type { RuntimeServices } from '@og-suite/runtime'
  import { bootstrapWorkspace, flushQueuedOperations, mergeEnvelope, pullChanges, queueOperation } from '@og-suite/sync'
  import * as Y from 'yjs'
  import { Editor, Extension } from '@tiptap/core'
  import Collaboration from '@tiptap/extension-collaboration'
  import StarterKit from '@tiptap/starter-kit'
  import Underline from '@tiptap/extension-underline'
  import Link from '@tiptap/extension-link'
  import Image from '@tiptap/extension-image'
  import TextAlign from '@tiptap/extension-text-align'
  import TaskList from '@tiptap/extension-task-list'
  import TaskItem from '@tiptap/extension-task-item'
  import { Table } from '@tiptap/extension-table'
  import TableRow from '@tiptap/extension-table-row'
  import TableHeader from '@tiptap/extension-table-header'
  import TableCell from '@tiptap/extension-table-cell'
  import { TextStyle } from '@tiptap/extension-text-style'
  import FontFamily from '@tiptap/extension-font-family'
  import Color from '@tiptap/extension-color'
  import Highlight from '@tiptap/extension-highlight'
  import DOMPurify from 'dompurify'
  import { marked } from 'marked'
  import TurndownService from 'turndown'
  import ActionBar from '@og-suite/ui/ActionBar'
  import Icon from '@og-suite/ui/Icon'
  import MobileSuiteTopBar from '@og-suite/ui/MobileSuiteTopBar'
  import { builtInFontOptions, customFontsChangedEvent, fontFamilyForCustomFont, loadStoredFonts, applyTokens, saveStoredTokens } from '@og-suite/ui'
  import AppearanceSettings from './AppearanceSettings.svelte'

  type EditorRenderMode = 'text' | 'markdown' | 'rich'

  // MD mode's raw-source-plus-preview split is a desktop power-user
  // feature; on a phone screen it just splits the note in half. Mobile
  // only gets TXT/RICH in the mode toggle.
  const isMobilePlatform = typeof navigator !== 'undefined' && /Android|iPhone|iPad|iPod/i.test(navigator.userAgent)
  type ListStyle = 'dash' | 'star' | 'checkbox' | 'numbered' | 'emoji'
  type SaveIndicatorState = 'saved' | 'pending' | 'syncing' | 'offline'
  type TocHeading = {
    id: string
    level: number
    title: string
    position: number
  }
  type SuiteNavItem = {
    id: string
    name: string
    disabled?: boolean
  }
  type SuiteOpenTarget = {
    appId: string
    targetKind: string
    targetId: string
    targetLabel: string
    requestId: number
  }

  export let services: RuntimeServices
  export let mode: 'suite' | 'standalone' = 'standalone'
  export let suiteNavItems: SuiteNavItem[] = []
  export let activeSuiteAppId = ''
  export let onSuiteAppSelect: ((appId: string) => void) | undefined = undefined
  export let onOpenSuiteSettings: (() => void) | undefined = undefined
  export let openTarget: SuiteOpenTarget | null = null
  export let onBackupToServer: (() => void) | undefined = undefined
  export let onOpenServerManager: (() => void) | undefined = undefined
  export let activeServerUrl = ''
  export let connectedServers: Array<{
    id: string
    url: string
    username: string
    displayName: string
    workspaceName: string
    accessToken?: string
    active: boolean
  }> = []

  let envelope: SyncEnvelope | null = null
  let selectedNoteId = ''
  let editorText = ''
  let editorDocumentId = ''
  let lastSavedEditorText = ''
  const enqueueSave = createSerialQueue()
  let draftTitle = ''
  let draftPath = '/'
  let status = 'Starting'
  let loggedStatus = ''
  let statusDialogOpen = false
  // Tracks which document's versions are currently loaded, so the reactive
  // load-trigger below only actually fires on a real note switch or the
  // dialog opening — not every time `selectedNote` gets a new object
  // reference (which happens on every periodic pull, same note or not).
  let versionsLoadedForDocumentId = ''
  let documentVersions: DocumentVersionSummary[] = []
  let versionsLoading = false
  let versionsError = ''
  let restoringVersionId = ''
  let syncLog: Array<{ id: string; message: string; at: string; iso: string }> = []
  let lastSyncAttemptAt = ''
  let lastSyncPushAt = ''
  let lastSyncPullAt = ''
  let lastRemoteUpdateAt = ''
  let lastDocumentRefreshAt = ''
  let lastSyncError = ''
  let queuedOperationCount = 0
  let queuedDocumentIds: string[] = []
  let backingUpServerId = ''
  let manualSyncBusy: 'push' | 'pull' | '' = ''
  let logCopyStatus = ''
  let lastEditorInteractionAt = 0
  let deferredDocumentRefreshId = ''
  let lastTextSelection = { start: 0, end: 0 }
  let peers: PresencePeer[] = []
  let sequence = 1
  let unsubscribePresence: (() => void) | null = null
  let unsubscribeDocumentUpdates: (() => void) | null = null
  let liveUpdateQueueIds = new Map<string, string>()
  let liveUpdateFallbackTimers = new Map<string, ReturnType<typeof setTimeout>>()
  let saveTimer: ReturnType<typeof setTimeout> | null = null
  let flushTimer: ReturnType<typeof setTimeout> | null = null
  let pullTimer: ReturnType<typeof setInterval> | null = null
  let settingsOpen = false
  let tokens = services.tokens
  let editorElement: HTMLTextAreaElement | null = null
  let richEditorElement: HTMLDivElement | null = null
  let richEditor: Editor | null = null
  let richDocumentId = ''
  // The live Y.Doc backing the Rich editor's Collaboration binding.
  // ProseMirror transactions write into this synchronously, as part of the
  // same call stack as the keystroke — ⁠there's no debounce or diff step
  // between typing and the CRDT state being correct. liveYDocLastVector
  // tracks what's already been persisted/broadcast so encodeDocUpdateSince
  // only ships the delta, not the whole doc, on each save.
  let liveYDoc: Y.Doc | null = null
  let liveYDocLastVector: Uint8Array | null = null
  let richActiveStateVersion = 0
  let richActiveStateFrame: number | null = null
  let uploadInputElement: HTMLInputElement | null = null
  let textColor = services.tokens.colorText
  let highlightColor = services.tokens.colorWarning
  let tableMenuOpen = false
  let richTableMenuStyle = ''
  let tocOpen = false
  let listMenuOpen = false
  let listMenuStyle = ''
  let searchOpen = false
  let searchQuery = ''
  let activeFolderPath = '/'
  let selectedFolderPath = ''
  let draggedNoteId = ''
  let draggedFolderPath = ''
  let dragTargetPath = ''
  let lastDragTargetPath = ''
  let touchDragActive = false
  let touchDragSuppressClick = false
  let touchPressTimer: ReturnType<typeof setTimeout> | null = null
  let touchPressStartX = 0
  let touchPressStartY = 0
  let touchPressSource:
    | { kind: 'note'; id: string }
    | { kind: 'folder'; path: string }
    | null = null
  let collapsedFolderPaths: string[] = []
  let favoriteNoteIds: string[] = loadFavoriteNoteIds()
  let multiSelectMode = false
  let multiSelectedNoteIds: Set<string> = new Set()
  // Long-pressing a note on mobile arms move-mode (see startMobileTreePress)
  // and, if released without dropping onto a folder, toggles this note as
  // "selected for download" without opening it. Opening a note on mobile
  // replaces the entire list view (and its toolbar, including the
  // download button) with the editor, so there was previously no way to
  // download a note without also navigating away from the button that
  // does it. Holding the same note again clears the selection.
  let downloadSelectedNoteId = ''
  let multiSelectDeleteBusy = false
  let deleteButtonPressTimer: ReturnType<typeof setTimeout> | null = null
  let mobileFilesOpen = false
  let editorFocused = false
  let sidebarWidth = 380
  let resizingSidebar = false
  let handledOpenTargetKey = ''
  let isLocalRuntime = services.runtimeMode === 'local'
  let customFonts: CustomFont[] = loadStoredFonts()
  let editorRenderMode: EditorRenderMode = loadEditorRenderMode()
  const selectionProtectionMs = 1800
  const turndown = new TurndownService({ headingStyle: 'atx', codeBlockStyle: 'fenced' })

  marked.use({
    async: false,
    gfm: true,
    breaks: false,
  })

  turndown.addRule('taskListItems', {
    filter: (node) => node.nodeName === 'LI' && (node as HTMLElement).getAttribute('data-type') === 'taskItem',
    replacement: (content, node) => {
      const input = (node as HTMLElement).querySelector('input[type="checkbox"]') as HTMLInputElement | null
      const checked = input?.checked ? 'x' : ' '
      const text = content.replace(/^\s+|\s+$/g, '').replace(/^\[[ xX]\]\s*/, '')
      return `- [${checked}] ${text || 'Task'}\n`
    },
  })

  turndown.addRule('styledInlineElements', {
    filter: (node) => {
      if (!(node instanceof HTMLElement)) return false
      const tag = node.tagName.toLowerCase()
      return tag === 'span' || tag === 'mark' || tag === 'u'
    },
    replacement: (content, node) => {
      const element = node as HTMLElement
      const tag = element.tagName.toLowerCase()
      if (tag === 'u') return `<u>${content}</u>`
      const style = sanitizeInlineStyle(element.getAttribute('style') ?? '')
      if (!style && tag === 'span') return content
      const styleAttribute = style ? ` style="${escapeHtmlAttribute(style)}"` : ''
      return `<${tag}${styleAttribute}>${content}</${tag}>`
    },
  })

  $: notes = envelope?.notes.filter((note) => !note.deletedAt) ?? []
  $: noteFolders = envelope?.noteFolders?.filter((folder) => !folder.deletedAt) ?? []
  $: filteredNotes = searchQuery.trim()
    ? notes.filter((note) => `${note.title} ${note.path}`.toLowerCase().includes(searchQuery.trim().toLowerCase()))
    : notes
  $: folderPaths = Array.from(
    new Set(
      [
        ...noteFolders
          .filter((folder) => folderMatchesSearch(folder))
          .map((folder) => normalizeFolderPath(folder.path))
          .filter((path) => path !== '/'),
        ...filteredNotes
          .map((note) => normalizeFolderPath(note.path))
          .filter((path) => path !== '/'),
      ],
    ),
  ).sort((left, right) => left.localeCompare(right))
  $: rootNotes = filteredNotes.filter((note) => normalizeFolderPath(note.path) === '/')
  $: folderGroups = folderPaths.map((folder) => ({
    path: folder,
    notes: filteredNotes.filter((note) => normalizeFolderPath(note.path) === folder),
  }))
  $: selectedNote = notes.find((note) => note.id === selectedNoteId) ?? notes[0] ?? null
  $: selectedFolderNotes = selectedFolderPath
    ? notes.filter((note) => isSameOrNestedPath(normalizeFolderPath(note.path), selectedFolderPath))
    : []
  $: selectedFolderFolders = selectedFolderPath
    ? noteFolders.filter((folder) => isSameOrNestedPath(normalizeFolderPath(folder.path), selectedFolderPath))
    : []
  $: selectedFolderCanDelete = selectedFolderPath !== '' && selectedFolderPath !== '/' && (selectedFolderNotes.length > 0 || selectedFolderFolders.length > 0)
  $: selectedDocument = selectedNote ? envelope?.documents.find((doc) => doc.id === selectedNote.documentId) ?? null : null
  $: renderedMarkdown = editorRenderMode === 'markdown' ? renderMarkdown(editorText) : ''
  $: if (editorRenderMode === 'rich' && selectedNote && selectedDocument && richEditorElement) {
    ensureRichEditor()
  }
  $: if (editorRenderMode !== 'rich' && richEditor) {
    exportRichEditorToMarkdown()
    destroyRichEditor()
  }

  $: if (selectedNote && selectedNote.id !== selectedNoteId) {
    selectNote(selectedNote)
  }
  $: if (openTarget?.appId === 'notes' && envelope) {
    void openSuiteTarget(openTarget, notes.length, envelope.documents.length)
  }
  $: saveIndicatorState = getSaveIndicatorState(status)
  $: editorFontOptions = [
    ...builtInFontOptions,
    ...customFonts.map((font) => ({ label: font.name, value: fontFamilyForCustomFont(font) })),
  ]
  $: saveIndicatorLabel = saveIndicatorState === 'saved'
    ? 'Saved'
    : saveIndicatorState === 'syncing'
      ? 'Syncing'
      : saveIndicatorState === 'pending'
        ? 'Saving'
        : 'Offline'
  $: tocHeadings = editorRenderMode === 'rich' ? getRichHeadings(richActiveStateVersion) : getMarkdownHeadings(editorText)
  $: isLocalRuntime = services.runtimeMode === 'local'
  $: activeServerLabel = isLocalRuntime
    ? 'Local device only'
    : mode === 'suite'
      ? activeServerUrl || services.serverUrl || 'Primary Suite server'
      : activeServerUrl || services.serverUrl || 'Remote server'
  $: hasServerBackupTargets = mode !== 'suite' && connectedServers.length > 0
  $: syncLogText = buildSyncLogText()
  $: if (status && status !== loggedStatus) {
    loggedStatus = status
    logSyncEvent(status)
  }

  function logSyncEvent(message: string) {
    syncLog = [{ id: `${Date.now()}-${syncLog.length}`, message, at: new Date().toLocaleTimeString(), iso: new Date().toISOString() }, ...syncLog].slice(0, 160)
  }

  function recordSyncError(error: unknown, fallback: string) {
    const message = error instanceof Error ? error.message : fallback
    lastSyncError = `${new Date().toLocaleTimeString()} - ${message}`
    logSyncEvent(`Error: ${message}`)
    if (error instanceof Error && error.stack) logSyncEvent(`Stack: ${error.stack}`)
    return message
  }

  function buildSyncLogText() {
    const selected = selectedNote ? `${selectedNote.title} (${selectedNote.id}, document ${selectedNote.documentId})` : 'None'
    const lines = [
      `OG Notes sync log`,
      `Generated: ${new Date().toISOString()}`,
      `Mode: ${isLocalRuntime ? 'local' : 'remote'}`,
      `Server: ${activeServerLabel}`,
      `Client: ${services.clientId}`,
      `Selected note: ${selected}`,
      `Queued changes: ${queuedOperationCount}`,
      `Queued document ids: ${queuedDocumentIds.join(', ') || 'none'}`,
      `Last attempt: ${lastSyncAttemptAt || 'none'}`,
      `Last push: ${lastSyncPushAt || 'none'}`,
      `Last pull: ${lastSyncPullAt || 'none'}`,
      `Last remote update: ${lastRemoteUpdateAt || 'none'}`,
      `Last document refresh: ${lastDocumentRefreshAt || 'none'}`,
      `Last error: ${lastSyncError || 'none'}`,
      '',
      'Recent activity:',
      ...syncLog.map((item) => `[${item.iso ?? item.at}] ${item.message}`),
    ]
    return lines.join('\n')
  }

  async function copySyncLog() {
    try {
      await navigator.clipboard.writeText(syncLogText)
      logCopyStatus = 'Copied'
      setTimeout(() => {
        logCopyStatus = ''
      }, 1800)
    } catch (error) {
      recordSyncError(error, 'Could not copy sync log.')
      logCopyStatus = 'Copy failed'
    }
  }

  function getSaveIndicatorState(currentStatus: string): SaveIndicatorState {
    if (currentStatus.toLowerCase().includes('offline')) return 'offline'
    if (flushTimer) return 'syncing'
    if (saveTimer) return 'pending'
    return 'saved'
  }

  async function start() {
    applyTokens(services.tokens)
    await discardLegacyQueuedDocumentUpdates()
    await refreshQueuedOperationCount()
    envelope = await bootstrapWorkspace(services)
    await refreshQueuedOperationCount()
    if (!isLocalRuntime) await tryFlushAndPull()
    if (notes[0]) selectNote(notes[0])
    if (!isLocalRuntime) startRemotePullFallback()
    status = isLocalRuntime ? 'Loaded local notes' : mode === 'suite' ? 'Loaded in Suite' : 'Loaded standalone'
  }

  async function discardLegacyQueuedDocumentUpdates() {
    const queued = await services.syncQueue.list()
    const legacyDocumentUpdateIds = queued
      .filter((item) => item.operation.kind === 'append_document_update' && item.operation.update.clientSchemaVersion !== 2)
      .map((item) => item.id)
    if (legacyDocumentUpdateIds.length > 0) {
      await services.syncQueue.remove(legacyDocumentUpdateIds)
      status = `Cleared ${legacyDocumentUpdateIds.length} legacy queued edit${legacyDocumentUpdateIds.length === 1 ? '' : 's'}`
    }
  }

  async function tryFlushAndPull() {
    lastSyncAttemptAt = new Date().toLocaleTimeString()
    if (isLocalRuntime) {
      envelope = await services.cache.loadEnvelope()
      refreshSelectedEditorFromEnvelope()
      status = 'Saved locally'
      return
    }
    try {
      envelope = await flushQueuedOperations(services)
      lastSyncPushAt = new Date().toLocaleTimeString()
      await refreshQueuedOperationCount()
      if (queuedOperationCount === 0 && !shouldProtectEditorSelection()) {
        envelope = await pullChanges(services)
        lastSyncPullAt = new Date().toLocaleTimeString()
        refreshSelectedEditorFromEnvelope()
        await refreshSelectedDocumentFromServer()
      }
    } catch (error) {
      recordSyncError(error, 'Could not reach server while syncing.')
      status = 'Offline, saving locally'
    }
  }

  async function manualPushToServer() {
    if (isLocalRuntime || manualSyncBusy) return
    manualSyncBusy = 'push'
    lastSyncAttemptAt = new Date().toLocaleTimeString()
    logSyncEvent('Manual push started')
    try {
      await flushPendingEditorSave()
      envelope = await flushQueuedOperations(services)
      lastSyncPushAt = new Date().toLocaleTimeString()
      await refreshQueuedOperationCount()
      refreshSelectedEditorFromEnvelope()
      status = 'Manual push complete'
      logSyncEvent(`Manual push complete with ${queuedOperationCount} queued change${queuedOperationCount === 1 ? '' : 's'} remaining`)
    } catch (error) {
      recordSyncError(error, 'Manual push failed.')
      status = 'Manual push failed'
    } finally {
      manualSyncBusy = ''
    }
  }

  async function manualPullFromServer() {
    if (isLocalRuntime || manualSyncBusy) return
    manualSyncBusy = 'pull'
    lastSyncAttemptAt = new Date().toLocaleTimeString()
    logSyncEvent('Manual pull started')
    try {
      await flushPendingEditorSave()
      await refreshQueuedOperationCount()
      if (queuedOperationCount > 0) {
        status = 'Push queued changes before pulling'
        logSyncEvent(`Manual pull stopped because ${queuedOperationCount} local change${queuedOperationCount === 1 ? '' : 's'} are queued`)
        return
      }
      envelope = await pullChanges(services)
      lastSyncPullAt = new Date().toLocaleTimeString()
      refreshSelectedEditorFromEnvelope()
      await refreshSelectedDocumentFromServer()
      status = 'Manual pull complete'
      logSyncEvent(`Manual pull complete; loaded ${notes.length} note${notes.length === 1 ? '' : 's'}`)
    } catch (error) {
      recordSyncError(error, 'Manual pull failed.')
      status = 'Manual pull failed'
    } finally {
      manualSyncBusy = ''
    }
  }

  // Background/periodic refreshes (silent: true) must never touch anything
  // the user can see unless the version list actually changed — this used
  // to unconditionally flip versionsLoading and replace documentVersions
  // with a freshly-deserialized (so always a new object identity) array
  // every ~1200ms pull tick, which made the whole version history section
  // flicker in and out of a loading state every second even though nothing
  // had changed.
  async function loadDocumentVersions(options: { silent?: boolean } = {}) {
    if (isLocalRuntime || !selectedNote) {
      if (!options.silent) documentVersions = []
      return
    }
    if (!options.silent) {
      versionsLoading = true
      versionsError = ''
    }
    try {
      const next = await services.api.get<DocumentVersionSummary[]>(
        `/api/v1/documents/${selectedNote.documentId}/versions`,
      )
      if (!sameDocumentVersionList(documentVersions, next)) documentVersions = next
    } catch (error) {
      if (!options.silent) {
        versionsError = error instanceof Error ? error.message : 'Could not load version history.'
        documentVersions = []
      }
    } finally {
      if (!options.silent) versionsLoading = false
    }
  }

  function sameDocumentVersionList(a: DocumentVersionSummary[], b: DocumentVersionSummary[]) {
    if (a.length !== b.length) return false
    return a.every((item, index) => {
      const other = b[index]
      return other && item.id === other.id && item.createdAt === other.createdAt && item.reason === other.reason
    })
  }

  async function restoreDocumentVersion(versionId: string) {
    if (!selectedNote || restoringVersionId) return
    if (tokens.confirmDelete && !window.confirm('Restore this version? The current content will be kept as its own version first, so this is undoable.')) return
    restoringVersionId = versionId
    try {
      await services.api.post(`/api/v1/documents/${selectedNote.documentId}/versions/${versionId}/restore`, {})
      // The restored state lives on the server now; drop any local live
      // editing state for this document so the next open re-hydrates from
      // it fresh instead of a stale in-memory Y.Doc.
      if (richDocumentId === selectedNote.documentId) destroyRichEditor()
      envelope = await services.cache.loadEnvelope()
      await refreshSelectedDocumentFromServer()
      refreshSelectedEditorFromEnvelope()
      await loadDocumentVersions()
      status = 'Restored an earlier version'
    } catch (error) {
      recordSyncError(error, 'Could not restore that version.')
    } finally {
      restoringVersionId = ''
    }
  }

  $: if (!statusDialogOpen) versionsLoadedForDocumentId = ''
  $: if (statusDialogOpen && selectedNote && selectedNote.documentId !== versionsLoadedForDocumentId) {
    versionsLoadedForDocumentId = selectedNote.documentId
    void loadDocumentVersions()
  }

  async function refreshQueuedOperationCount() {
    const queued = await services.syncQueue.list()
    queuedOperationCount = queued.length
    queuedDocumentIds = Array.from(
      new Set(queued.map((item) => documentIdForOperation(item.operation)).filter((documentId): documentId is string => Boolean(documentId))),
    )
  }

  function documentIdForOperation(operation: SyncOperation) {
    if (operation.kind === 'create_note') return operation.note.documentId
    if (operation.kind === 'update_note_metadata') return operation.note.documentId
    if (operation.kind === 'append_document_update') return operation.update.documentId
    return ''
  }

  function hasQueuedLocalDocumentChange(documentId = selectedNote?.documentId) {
    return Boolean(documentId && queuedDocumentIds.includes(documentId))
  }

  function advanceSequenceFromDocument(document = selectedDocument) {
    if (!document) return
    const maxClientSequence = document.updates.reduce((max, update) => {
      return update.clientId === services.clientId ? Math.max(max, update.sequence) : max
    }, 0)
    sequence = Math.max(sequence, maxClientSequence + 1)
  }

  async function backupSelectedNoteToServer(server: (typeof connectedServers)[number]) {
    if (!selectedNote || !selectedDocument || !server.accessToken) return
    backingUpServerId = server.id
    logSyncEvent(`Manual backup started for ${server.url}`)
    try {
      await flushPendingEditorSave()
      const document = envelope?.documents.find((item) => item.id === selectedNote.documentId) ?? selectedDocument
      const api = createHttpApiClient(server.url, () => server.accessToken ?? '')
      await api.post('/api/v1/sync/push', {
        operations: [
          {
            kind: 'create_note',
            note: selectedNote,
            document,
          },
        ],
      })
      lastSyncPushAt = new Date().toLocaleTimeString()
      status = `Backed up to ${server.url}`
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Unknown error'
      recordSyncError(error, `Backup failed for ${server.url}.`)
      status = `Backup failed for ${server.url}: ${message}`
    } finally {
      backingUpServerId = ''
    }
  }

  async function selectNote(note: Note) {
    const pendingDocumentId = editorDocumentId
    const hadSaveTimer = Boolean(saveTimer)
    if (saveTimer) {
      clearTimeout(saveTimer)
      saveTimer = null
    }
    // Must be awaited, not fire-and-forget: this switch reads `envelope`
    // for the note being opened a few lines down. If the outgoing note's
    // save were still in flight when that read happens, switching back to
    // it later would show whatever was in `envelope` before the save
    // landed — i.e. the edit looks reverted even though it was never lost,
    // just not committed yet.
    if (hadSaveTimer || hasPendingLocalEditorChange(pendingDocumentId)) {
      await saveDocument()
    }
    selectedNoteId = note.id
    logSyncEvent(`Opened note ${note.title || note.id}`)
    selectedFolderPath = ''
    draftTitle = note.title
    draftPath = note.path
    const document = envelope?.documents.find((item) => item.id === note.documentId)
    advanceSequenceFromDocument(document)
    editorDocumentId = note.documentId
    editorText = document ? applyUpdates(document).text : ''
    lastSavedEditorText = editorText
    destroyRichEditor()
    unsubscribePresence?.()
    unsubscribeDocumentUpdates?.()
    unsubscribePresence = services.presence.connect(note.documentId, (nextPeers) => {
      peers = nextPeers.filter((peer) => peer.clientId !== services.clientId)
    })
    unsubscribeDocumentUpdates = services.documentUpdates.connect(note.documentId, applyRemoteDocumentUpdate, handleLiveDocumentAck, handleLiveDocumentError)
    void refreshSelectedDocumentFromServer(note.documentId)
    mobileFilesOpen = false
  }

  async function openSuiteTarget(target: SuiteOpenTarget, _noteCount: number, _documentCount: number) {
    const key = `${target.requestId}:${target.appId}:${target.targetKind}:${target.targetId}`
    if (handledOpenTargetKey === key) return
    handledOpenTargetKey = key

    if (target.targetKind === 'folder') {
      selectFolder(target.targetLabel)
      status = `Opened folder ${target.targetLabel}`
      mobileFilesOpen = false
      return
    }

    let note = findNoteForTarget(target)
    if (!note && ['note', 'document'].includes(target.targetKind)) {
      await tryFlushAndPull()
      note = findNoteForTarget(target)
    }

    if (note) {
      selectNote(note)
      activeFolderPath = normalizeFolderPath(note.path)
      status = `Opened ${note.title}`
    } else {
      status = `Could not find ${target.targetLabel}`
    }
    mobileFilesOpen = false
  }

  function findNoteForTarget(target: SuiteOpenTarget) {
    if (target.targetKind === 'note') return notes.find((note) => note.id === target.targetId) ?? null
    if (target.targetKind === 'document') return notes.find((note) => note.documentId === target.targetId) ?? null
    return null
  }

  function selectSuiteApp(appId: string) {
    onSuiteAppSelect?.(appId)
    mobileFilesOpen = false
  }

  function openSuiteSettings() {
    onOpenSuiteSettings?.()
    mobileFilesOpen = false
  }

  async function createNote(path = activeFolderPath) {
    const folderPath = normalizeFolderPath(path)
    const now = new Date().toISOString()
    const noteId = createRuntimeId('note')
    const documentId = createRuntimeId('document')
    const note: Note = {
      id: noteId,
      documentId,
      title: 'Untitled note',
      path: folderPath,
      tags: [],
      ownerId: 'local-user',
      workspaceId: 'default',
      createdAt: now,
      updatedAt: now,
    }
    const document = createDocumentState(documentId, 'note', '')
    await queueWorkspaceOperation({ kind: 'create_note', note, document })
    await refreshQueuedOperationCount()
    envelope = await services.cache.loadEnvelope()
    selectNote(note)
    status = 'Created locally'
  }

  async function createNoteFromUpload(file: File, text: string) {
    const now = new Date().toISOString()
    const noteId = createRuntimeId('note')
    const documentId = createRuntimeId('document')
    const title = file.name.replace(/\.(md|txt)$/i, '') || 'Uploaded note'
    const note: Note = {
      id: noteId,
      documentId,
      title,
      path: normalizeFolderPath(activeFolderPath),
      tags: [],
      ownerId: 'local-user',
      workspaceId: 'default',
      createdAt: now,
      updatedAt: now,
    }
    const document = createDocumentState(documentId, 'note', text)
    await queueWorkspaceOperation({ kind: 'create_note', note, document })
    await refreshQueuedOperationCount()
    envelope = await services.cache.loadEnvelope()
    selectNote(note)
    status = `Uploaded ${file.name}`
  }

  async function saveMetadata() {
    if (!selectedNote) return
    const updated: Note = {
      ...selectedNote,
      title: draftTitle.trim() || 'Untitled note',
      path: draftPath.trim() || '/',
      updatedAt: new Date().toISOString(),
    }
    await queueWorkspaceOperation({ kind: 'update_note_metadata', note: updated })
    await refreshQueuedOperationCount()
    envelope = await services.cache.loadEnvelope()
    status = 'Metadata queued'
  }

  async function moveNoteToFolder(noteId: string, path: string) {
    const note = notes.find((item) => item.id === noteId)
    if (!note) return
    const nextPath = normalizeFolderPath(path)
    if (normalizeFolderPath(note.path) === nextPath) return
    const updated: Note = {
      ...note,
      path: nextPath,
      updatedAt: new Date().toISOString(),
    }
    await queueWorkspaceOperation({ kind: 'update_note_metadata', note: updated })
    await refreshQueuedOperationCount()
    envelope = await services.cache.loadEnvelope()
    if (selectedNoteId === noteId) draftPath = nextPath
    activeFolderPath = nextPath
    status = `Moved to ${nextPath}`
  }

  async function moveFolderToFolder(folderPath: string, targetPath: string) {
    const sourcePath = normalizeFolderPath(folderPath)
    const destinationPath = normalizeFolderPath(targetPath)
    if (sourcePath === '/' || sourcePath === destinationPath || destinationPath.startsWith(`${sourcePath}/`)) return

    const movedPath = normalizeFolderPath(`${destinationPath}/${folderName(sourcePath)}`)
    if (movedPath === sourcePath) return

    const now = new Date().toISOString()
    const affectedFolders = noteFolders.filter((folder) => {
      const currentPath = normalizeFolderPath(folder.path)
      return currentPath === sourcePath || currentPath.startsWith(`${sourcePath}/`)
    })
    const affectedNotes = notes.filter((note) => {
      const currentPath = normalizeFolderPath(note.path)
      return currentPath === sourcePath || currentPath.startsWith(`${sourcePath}/`)
    })

    for (const folder of affectedFolders) {
      const currentPath = normalizeFolderPath(folder.path)
      const nextPath = remapPath(currentPath, sourcePath, movedPath)
      await queueWorkspaceOperation({
        kind: 'create_note_folder',
        folder: {
          ...folder,
          path: nextPath,
          name: folderName(nextPath),
          updatedAt: now,
        },
      })
    }

    for (const note of affectedNotes) {
      await queueWorkspaceOperation({
        kind: 'update_note_metadata',
        note: {
          ...note,
          path: remapPath(normalizeFolderPath(note.path), sourcePath, movedPath),
          updatedAt: now,
        },
      })
    }

    envelope = await services.cache.loadEnvelope()
    await refreshQueuedOperationCount()
    if (activeFolderPath === sourcePath || activeFolderPath.startsWith(`${sourcePath}/`)) {
      activeFolderPath = remapPath(activeFolderPath, sourcePath, movedPath)
    }
    if (selectedNote && (draftPath === sourcePath || draftPath.startsWith(`${sourcePath}/`))) {
      draftPath = remapPath(draftPath, sourcePath, movedPath)
    }
    status = `Moved folder to ${destinationPath}`
  }

  async function renameSelectedFolder() {
    const sourcePath = normalizeFolderPath(selectedFolderPath)
    if (!sourcePath || sourcePath === '/') return
    const nextName = window.prompt('Folder name', folderName(sourcePath))?.trim()
    if (!nextName) return
    const destinationPath = parentFolderPath(sourcePath)
    const renamedPath = normalizeFolderPath(`${destinationPath}/${nextName}`)
    if (renamedPath === sourcePath) return
    const existingFolder = noteFolders.find((folder) => normalizeFolderPath(folder.path) === renamedPath)
    const existingNoteInTarget = notes.find((note) => normalizeFolderPath(note.path) === renamedPath)
    if (existingFolder || existingNoteInTarget) {
      status = `Folder already exists: ${renamedPath}`
      return
    }

    const now = new Date().toISOString()
    const affectedFolders = noteFolders.filter((folder) => isSameOrNestedPath(folder.path, sourcePath))
    const affectedNotes = notes.filter((note) => isSameOrNestedPath(note.path, sourcePath))

    for (const folder of affectedFolders) {
      const nextPath = remapPath(normalizeFolderPath(folder.path), sourcePath, renamedPath)
      await queueWorkspaceOperation({
        kind: 'create_note_folder',
        folder: {
          ...folder,
          path: nextPath,
          name: folderName(nextPath),
          updatedAt: now,
        },
      })
    }

    for (const note of affectedNotes) {
      await queueWorkspaceOperation({
        kind: 'update_note_metadata',
        note: {
          ...note,
          path: remapPath(normalizeFolderPath(note.path), sourcePath, renamedPath),
          updatedAt: now,
        },
      })
    }

    envelope = await services.cache.loadEnvelope()
    activeFolderPath = renamedPath
    selectedFolderPath = renamedPath
    if (selectedNote && isSameOrNestedPath(draftPath, sourcePath)) {
      draftPath = remapPath(draftPath, sourcePath, renamedPath)
    }
    status = `Renamed folder to ${renamedPath}`
  }

  async function renameSelectedFileTarget() {
    if (selectedFolderPath) {
      await renameSelectedFolder()
      return
    }
    if (!selectedNote) return
    const title = window.prompt('Note name', selectedNote.title)?.trim()
    if (!title) return
    draftTitle = title
    await saveMetadata()
  }

  function startNoteDrag(event: DragEvent, note: Note) {
    draggedNoteId = note.id
    draggedFolderPath = ''
    lastDragTargetPath = ''
    event.dataTransfer?.setData('text/plain', note.id)
    event.dataTransfer?.setData('application/x-og-note-id', note.id)
    if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move'
  }

  function startMobileNotePress(event: PointerEvent, note: Note) {
    startMobileTreePress(event, { kind: 'note', id: note.id })
  }

  function startMobileFolderPress(event: PointerEvent, path: string) {
    startMobileTreePress(event, { kind: 'folder', path: normalizeFolderPath(path) })
  }

  function startMobileTreePress(
    event: PointerEvent,
    source: { kind: 'note'; id: string } | { kind: 'folder'; path: string },
  ) {
    if (event.pointerType === 'mouse' || event.button !== 0 || multiSelectMode) return
    clearMobileTreePress()
    touchPressSource = source
    touchPressStartX = event.clientX
    touchPressStartY = event.clientY
    ;(event.currentTarget as HTMLElement | null)?.setPointerCapture?.(event.pointerId)
    touchPressTimer = setTimeout(() => {
      touchDragActive = true
      touchDragSuppressClick = true
      if (source.kind === 'note') {
        draggedNoteId = source.id
        draggedFolderPath = ''
      } else {
        draggedNoteId = ''
        draggedFolderPath = source.path
      }
      updateMobileDragTarget(event.clientX, event.clientY)
      status = source.kind === 'note' ? 'Move note to a folder' : 'Move folder'
    }, 420)
  }

  function moveMobileTreePress(event: PointerEvent) {
    if (!touchPressSource) return
    const movedDistance = Math.hypot(event.clientX - touchPressStartX, event.clientY - touchPressStartY)
    if (!touchDragActive && movedDistance > 10) {
      clearMobileTreePress()
      return
    }
    if (!touchDragActive) return
    event.preventDefault()
    updateMobileDragTarget(event.clientX, event.clientY)
  }

  async function endMobileTreePress(event: PointerEvent) {
    const wasActive = touchDragActive
    if (wasActive) updateMobileDragTarget(event.clientX, event.clientY)
    const targetPath = dragTargetPath
    const source = touchPressSource
    clearMobileTreePress(false)
    if (!wasActive || !source) return
    event.preventDefault()
    event.stopPropagation()
    if (!targetPath) {
      // Long-press-then-release with no folder drop: toggle download
      // selection instead of doing nothing (see downloadSelectedNoteId's
      // doc comment). Folders don't get this — only notes have a
      // download action to select for.
      if (source.kind === 'note') {
        downloadSelectedNoteId = downloadSelectedNoteId === source.id ? '' : source.id
      }
      return
    }
    if (source.kind === 'note') {
      await moveNoteToFolder(source.id, targetPath)
      if (downloadSelectedNoteId === source.id) downloadSelectedNoteId = ''
    }
    if (source.kind === 'folder') await moveFolderToFolder(source.path, targetPath)
    draggedNoteId = ''
    draggedFolderPath = ''
    dragTargetPath = ''
    lastDragTargetPath = ''
  }

  function cancelMobileTreePress() {
    clearMobileTreePress()
  }

  function clearMobileTreePress(clearSuppressClick = true) {
    if (touchPressTimer) {
      clearTimeout(touchPressTimer)
      touchPressTimer = null
    }
    touchPressSource = null
    touchDragActive = false
    draggedNoteId = ''
    draggedFolderPath = ''
    dragTargetPath = ''
    lastDragTargetPath = ''
    if (clearSuppressClick) touchDragSuppressClick = false
  }

  function updateMobileDragTarget(clientX: number, clientY: number) {
    const target = document.elementFromPoint(clientX, clientY)?.closest<HTMLElement>('[data-folder-drop-target]')
    const targetPath = normalizeFolderPath(target?.dataset.folderDropTarget ?? '')
    if (!target || !targetPath) {
      dragTargetPath = ''
      return
    }
    if (draggedFolderPath && (targetPath === draggedFolderPath || targetPath.startsWith(`${draggedFolderPath}/`))) {
      dragTargetPath = ''
      return
    }
    dragTargetPath = targetPath
    lastDragTargetPath = targetPath
  }

  function handleNoteRowClick(event: MouseEvent, note: Note) {
    if (touchDragSuppressClick) {
      event.preventDefault()
      event.stopPropagation()
      touchDragSuppressClick = false
      return
    }
    if (multiSelectMode) {
      toggleMultiSelectNote(note.id)
      return
    }
    selectNote(note)
  }

  function handleFolderRowClick(event: MouseEvent, path: string) {
    if (touchDragSuppressClick) {
      event.preventDefault()
      event.stopPropagation()
      touchDragSuppressClick = false
      return
    }
    selectFolder(path)
  }

  function selectFolder(path: string) {
    const normalized = normalizeFolderPath(path)
    activeFolderPath = normalized
    selectedFolderPath = normalized === '/' ? '' : normalized
  }

  function startFolderDrag(event: DragEvent, path: string) {
    draggedFolderPath = normalizeFolderPath(path)
    draggedNoteId = ''
    lastDragTargetPath = ''
    event.dataTransfer?.setData('text/plain', draggedFolderPath)
    event.dataTransfer?.setData('application/x-og-folder-path', draggedFolderPath)
    if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move'
  }

  function allowFolderDrop(event: DragEvent, path: string) {
    const transferTypes = event.dataTransfer ? Array.from(event.dataTransfer.types) : []
    const hasDraggedEntity =
      Boolean(draggedNoteId || draggedFolderPath) ||
      transferTypes.includes('application/x-og-note-id') ||
      transferTypes.includes('application/x-og-folder-path')
    if (!hasDraggedEntity) return
    const targetPath = normalizeFolderPath(path)
    if (draggedFolderPath && (targetPath === draggedFolderPath || targetPath.startsWith(`${draggedFolderPath}/`))) return
    event.preventDefault()
    dragTargetPath = targetPath
    lastDragTargetPath = targetPath
    if (event.dataTransfer) event.dataTransfer.dropEffect = 'move'
  }

  async function dropNoteOnFolder(event: DragEvent, path: string) {
    event.preventDefault()
    event.stopPropagation()
    const noteId = event.dataTransfer?.getData('application/x-og-note-id') || draggedNoteId
    const folderPath = event.dataTransfer?.getData('application/x-og-folder-path') || draggedFolderPath
    draggedNoteId = ''
    draggedFolderPath = ''
    dragTargetPath = ''
    lastDragTargetPath = ''
    if (noteId) await moveNoteToFolder(noteId, path)
    if (folderPath) await moveFolderToFolder(folderPath, path)
  }

  async function endNoteDrag() {
    const noteId = draggedNoteId
    const folderPath = draggedFolderPath
    const targetPath = dragTargetPath || lastDragTargetPath
    draggedNoteId = ''
    draggedFolderPath = ''
    dragTargetPath = ''
    lastDragTargetPath = ''
    if (!targetPath) return
    if (noteId) await moveNoteToFolder(noteId, targetPath)
    if (folderPath) await moveFolderToFolder(folderPath, targetPath)
  }

  // Every caller funnels through this queue instead of calling
  // saveDocumentNow directly. saveDocumentNow diffs `editorText` against
  // `lastSavedEditorText` and applies that diff to whatever document state
  // is currently in `envelope` — if two calls ever overlapped (debounce
  // firing while a blur-triggered save was still mid-flight, for example),
  // the second one would diff against a stale baseline the first hadn't
  // committed yet, corrupting the CRDT text (dropped/duplicated
  // characters). createSerialQueue guarantees each save only starts once
  // the prior one has fully committed.
  function saveDocument(): Promise<void> {
    return enqueueSave(saveDocumentNow)
  }

  async function saveDocumentNow() {
    const note = selectedNote
    const activeEditorDocumentId = editorDocumentId || note?.documentId || ''
    const document = envelope?.documents.find((item) => item.id === activeEditorDocumentId) ?? selectedDocument
    const noteForDocument = notes.find((item) => item.documentId === activeEditorDocumentId) ?? note
    if (!noteForDocument || !document || !activeEditorDocumentId) return

    // Rich mode: the Collaboration extension has already written this
    // edit into liveYDoc as a real Yjs transaction (synchronously, when
    // the user typed it) — no diffing needed, just ship whatever changed
    // in the doc since the last thing we persisted. exportRichEditorToMarkdown
    // still runs so editorText stays valid for display and for TXT/MD mode
    // switches, but it's no longer what produces the persisted update.
    if (editorRenderMode === 'rich' && liveYDoc && richDocumentId === activeEditorDocumentId) {
      exportRichEditorToMarkdown()
      lastSavedEditorText = editorText
      // Keep the plain "content" field current too — see
      // syncPlainTextFieldFromMarkdown's doc comment. This mutates the same
      // liveYDoc that encodeDocUpdateSince is about to encode, so it rides
      // along in the same update instead of needing a separate round trip.
      syncPlainTextFieldFromMarkdown(liveYDoc, editorText)
      const payload = encodeDocUpdateSince(liveYDoc, liveYDocLastVector ?? getDocStateVector(liveYDoc))
      if (!payload) return
      liveYDocLastVector = getDocStateVector(liveYDoc)
      const update = {
        id: createRuntimeId('update'),
        documentId: activeEditorDocumentId,
        clientId: services.clientId,
        sequence: sequence++,
        payload,
        createdAt: new Date().toISOString(),
      }
      const queued = await queueOperation(services, { kind: 'append_document_update', update })
      liveUpdateQueueIds.set(update.id, queued.id)
      await refreshQueuedOperationCount()
      envelope = await services.cache.loadEnvelope()
      const broadcasted = services.documentUpdates.publishUpdate(activeEditorDocumentId, update)
      logSyncEvent(`${broadcasted ? 'Shared live edit' : 'Queued edit'} ${update.id} for ${noteForDocument.title || noteForDocument.id}`)
      if (broadcasted) {
        scheduleLiveAckFallback(update.id)
      } else {
        scheduleRemoteFlush()
      }
      services.presence.publishCursor(activeEditorDocumentId, richEditor?.state.selection.from ?? null)
      status = isLocalRuntime ? 'Saved locally' : broadcasted ? 'Document shared live' : 'Document queued for sync'
      return
    }

    const previousText = lastSavedEditorText
    const nextText = editorText
    if (previousText === nextText) return
    const update = {
      ...createTextDiffUpdate(activeEditorDocumentId, services.clientId, sequence++, previousText, nextText, document),
      id: createRuntimeId('update'),
      createdAt: new Date().toISOString(),
    }
    const queued = await queueOperation(services, { kind: 'append_document_update', update })
    liveUpdateQueueIds.set(update.id, queued.id)
    await refreshQueuedOperationCount()
    envelope = await services.cache.loadEnvelope()
    if (editorDocumentId === activeEditorDocumentId) {
      lastSavedEditorText = nextText
    }
    const broadcasted = services.documentUpdates.publishUpdate(activeEditorDocumentId, update)
    logSyncEvent(`${broadcasted ? 'Shared live edit' : 'Queued edit'} ${update.id} for ${noteForDocument.title || noteForDocument.id}`)
    if (broadcasted) {
      scheduleLiveAckFallback(update.id)
    } else {
      scheduleRemoteFlush()
    }

    // Mirror this TXT/MD edit into richContent too — see
    // syncRichFragmentFromMarkdown's doc comment. Reuses a persistent
    // headless doc/editor for this documentId across saves instead of
    // rebuilding one every time.
    const richPayload = syncRichFragmentFromMarkdown(activeEditorDocumentId, document, nextText)
    if (richPayload) {
      const richUpdate = {
        id: createRuntimeId('update'),
        documentId: activeEditorDocumentId,
        clientId: services.clientId,
        sequence: sequence++,
        payload: richPayload,
        createdAt: new Date().toISOString(),
      }
      const queuedRich = await queueOperation(services, { kind: 'append_document_update', update: richUpdate })
      liveUpdateQueueIds.set(richUpdate.id, queuedRich.id)
      await refreshQueuedOperationCount()
      envelope = await services.cache.loadEnvelope()
      const richBroadcasted = services.documentUpdates.publishUpdate(activeEditorDocumentId, richUpdate)
      if (richBroadcasted) scheduleLiveAckFallback(richUpdate.id)
      else scheduleRemoteFlush()
    }

    services.presence.publishCursor(activeEditorDocumentId, editorElement?.selectionStart ?? nextText.length)
    status = isLocalRuntime ? 'Saved locally' : broadcasted ? 'Document shared live' : 'Document queued for sync'
  }

  async function handleLiveDocumentAck(ack: { updateId: string; documentId: string; documentVersion?: number }) {
    const queuedId = liveUpdateQueueIds.get(ack.updateId)
    if (!queuedId) return
    clearLiveAckFallback(ack.updateId)
    liveUpdateQueueIds.delete(ack.updateId)
    await services.syncQueue.remove([queuedId])
    await refreshQueuedOperationCount()
    lastSyncPushAt = new Date().toLocaleTimeString()
    status = 'Live edit saved to server'
    logSyncEvent(`Server saved live edit ${ack.updateId}${ack.documentVersion ? ` at document version ${ack.documentVersion}` : ''}`)
  }

  function handleLiveDocumentError(error: { updateId?: string; message: string }) {
    if (error.updateId) clearLiveAckFallback(error.updateId)
    logSyncEvent(`Live edit failed${error.updateId ? ` ${error.updateId}` : ''}: ${error.message}`)
    status = 'Live edit queued for sync'
    scheduleRemoteFlush()
  }

  function scheduleLiveAckFallback(updateId: string) {
    clearLiveAckFallback(updateId)
    const timer = setTimeout(() => {
      liveUpdateFallbackTimers.delete(updateId)
      if (!liveUpdateQueueIds.has(updateId)) return
      logSyncEvent(`Live edit ${updateId} is waiting for server ack; falling back to queued sync`)
      scheduleRemoteFlush()
    }, 5000)
    liveUpdateFallbackTimers.set(updateId, timer)
  }

  function clearLiveAckFallback(updateId: string) {
    const timer = liveUpdateFallbackTimers.get(updateId)
    if (timer) clearTimeout(timer)
    liveUpdateFallbackTimers.delete(updateId)
  }

  function scheduleDocumentSave() {
    if (saveTimer) clearTimeout(saveTimer)
    saveTimer = setTimeout(() => {
      saveTimer = null
      void saveDocument()
    }, 180)
  }

  async function flushPendingEditorSave() {
    const hadSaveTimer = Boolean(saveTimer)
    if (saveTimer) {
      clearTimeout(saveTimer)
      saveTimer = null
    }
    if (hadSaveTimer || hasPendingLocalEditorChange(editorDocumentId)) await saveDocument()
  }

  // setTimeout is not a durable save mechanism on mobile: WebViews throttle
  // or pause JS timers the instant the app backgrounds (screen lock, app
  // switch, notification shade), which is exactly the moment a user is
  // most likely to leave a note. scheduleDocumentSave's 180ms debounce and
  // scheduleRemoteFlush's 800ms debounce can both be sitting unfired when
  // that happens. visibilitychange/pagehide fire synchronously as part of
  // the backgrounding transition itself, before any timer throttling
  // kicks in, so they're the actual reliable "about to lose the JS event
  // loop" signal — flush everything pending right there instead of
  // trusting the timers to eventually run.
  async function flushAllPendingWork() {
    try {
      await flushPendingEditorSave()
    } catch (error) {
      console.debug('Flush of pending editor save failed on backgrounding', error)
    }
    if (flushTimer) {
      clearTimeout(flushTimer)
      flushTimer = null
    }
    if (isLocalRuntime) return
    try {
      envelope = await flushQueuedOperations(services)
      lastSyncPushAt = new Date().toLocaleTimeString()
      await refreshQueuedOperationCount()
    } catch (error) {
      recordSyncError(error, 'Could not flush queued edits to the server.')
      // Still safe: flushPendingEditorSave already committed the edit to
      // the local cache above, so it survives even if the network push
      // itself failed here — the sync queue will retry it later.
    }
  }

  function handleVisibilityFlush() {
    if (document.visibilityState === 'hidden') void flushAllPendingWork()
  }

  async function queueWorkspaceOperation(operation: SyncOperation) {
    const queued = await queueOperation(services, operation)
    scheduleRemoteFlush()
    return queued
  }

  function scheduleRemoteFlush() {
    if (isLocalRuntime) {
      status = 'Saved locally'
      return
    }
    if (flushTimer) clearTimeout(flushTimer)
    flushTimer = setTimeout(async () => {
      flushTimer = null
      lastSyncAttemptAt = new Date().toLocaleTimeString()
      const activeDocumentId = selectedNote?.documentId
      const shouldProtectActiveEditor = document.activeElement === editorElement && (Boolean(saveTimer) || hasPendingLocalEditorChange())
      if (shouldProtectActiveEditor) {
        logSyncEvent('Deferred remote sync while the editor has local input')
        scheduleRemoteFlush()
        return
      }
      try {
        envelope = await flushQueuedOperations(services)
        lastSyncPushAt = new Date().toLocaleTimeString()
        await refreshQueuedOperationCount()
        if (activeDocumentId) {
          const note = notes.find((item) => item.documentId === activeDocumentId)
          if (note) {
            selectedNoteId = note.id
            if (!hasQueuedLocalDocumentChange(activeDocumentId)) refreshSelectedEditorFromEnvelope(activeDocumentId)
          }
        }
        status = 'Synced'
      } catch (error) {
        recordSyncError(error, 'Could not flush queued edits to the server.')
        status = 'Offline, saving locally'
      }
    }, 800)
  }

  async function applyRemoteDocumentUpdate(update: CrdtUpdate) {
    if (!envelope || update.clientId === services.clientId) return
    lastRemoteUpdateAt = new Date().toLocaleTimeString()
    logSyncEvent(`Received collaborator edit ${update.id}`)
    await flushPendingEditorSave()
    envelope = mergeEnvelope(envelope, {
      cursors: envelope.cursors,
      apps: [],
      noteFolders: [],
      notes: [],
      documents: [],
      documentUpdates: [update],
      tombstones: [],
      conflicts: [],
    })
    await services.cache.saveEnvelope(envelope)

    // Fluid concurrent editing: if this document is live in the Rich
    // editor right now, apply the collaborator's Yjs update directly onto
    // the shared doc — the Collaboration extension's binding reflects it
    // in the editor immediately (real merge, cursor position preserved),
    // no destructive re-render from a re-derived markdown string. Advance
    // liveYDocLastVector so our next local save only ships *our* changes,
    // not an unnecessary echo of what we just received back out.
    if (liveYDoc && richDocumentId === update.documentId) {
      applyEncodedUpdate(liveYDoc, update.payload)
      liveYDocLastVector = getDocStateVector(liveYDoc)
      status = 'Merged collaborator edit'
      return
    }

    refreshSelectedEditorFromEnvelope(update.documentId)
    status = 'Merged collaborator edit'
  }

  function refreshSelectedEditorFromEnvelope(documentId = selectedNote?.documentId) {
    if (!documentId || !envelope || saveTimer) return
    if (selectedNote?.documentId !== documentId) return
    if (hasQueuedLocalDocumentChange(documentId)) {
      deferredDocumentRefreshId = documentId
      return
    }
    if (shouldProtectEditorSelection(documentId)) {
      deferredDocumentRefreshId = documentId
      return
    }
    if (editorRenderMode === 'rich') {
      const richDocument = envelope.documents.find((document) => document.id === documentId)
      if (richDocument) applyRichDocumentState(richDocument)
      return
    }
    if (document.activeElement === editorElement && hasPendingLocalEditorChange(documentId)) return
    const nextDocument = envelope.documents.find((document) => document.id === documentId)
    if (nextDocument) setEditorTextPreservingSelection(applyUpdates(nextDocument).text, true)
  }

  function applyRichDocumentState(documentState: CrdtDocumentState) {
    if (editorRenderMode === 'rich' && richEditor) {
      // If this document is live-bound (liveYDoc), the "content" Y.Text
      // this function used to derive its text from is no longer what rich
      // edits write to — only "richContent" is. Reconciling by re-deriving
      // markdown from the stale "content" key and doing a destructive
      // setContent would wipe out whatever's actually in the live doc
      // (this was a real bug: the periodic pull-fallback timer clobbered
      // in-progress rich edits within ~1-2s of typing). Merge the delta
      // into the live doc instead — safe no-op if there's nothing new.
      if (liveYDoc && richDocumentId === documentState.id) {
        syncLiveDocFromState(liveYDoc, documentState)
        liveYDocLastVector = getDocStateVector(liveYDoc)
        return
      }
      if (
        hasQueuedLocalDocumentChange(documentState.id) ||
        shouldProtectEditorSelection(documentState.id) ||
        (richEditor.isFocused && hasPendingLocalEditorChange(documentState.id))
      ) {
        deferredDocumentRefreshId = documentState.id
        logSyncEvent('Deferred rich document refresh while editing')
        return
      }
      const nextText = applyUpdates(documentState).text
      editorText = nextText
      lastSavedEditorText = nextText
      richEditor.commands.setContent(renderMarkdown(nextText), { emitUpdate: false })
      return
    }
  }

  function handleEditorInput() {
    markEditorInteraction()
    if (selectedNote) services.presence.publishCursor(selectedNote.documentId, editorElement?.selectionStart ?? editorText.length)
    scheduleDocumentSave()
  }

  async function handleEditorBlur() {
    editorFocused = false
    await saveDocument()
    applyDeferredDocumentRefresh()
  }

  function markEditorInteraction() {
    lastEditorInteractionAt = Date.now()
  }

  function shouldProtectEditorSelection(documentId = selectedNote?.documentId) {
    if (!documentId || selectedNote?.documentId !== documentId) return false
    const recentlyInteracted = Date.now() - lastEditorInteractionAt < selectionProtectionMs
    if (editorRenderMode === 'rich' && richEditor?.isFocused) {
      return recentlyInteracted || !richEditor.state.selection.empty
    }
    if (!editorElement || document.activeElement !== editorElement) return false
    return recentlyInteracted || editorElement.selectionStart !== editorElement.selectionEnd
  }

  function applyDeferredDocumentRefresh() {
    const documentId = deferredDocumentRefreshId
    if (!documentId || shouldProtectEditorSelection(documentId)) return
    deferredDocumentRefreshId = ''
    refreshSelectedEditorFromEnvelope(documentId)
  }

  function hasPendingLocalEditorChange(documentId = selectedNote?.documentId) {
    if (!documentId || editorDocumentId !== documentId) return false
    return lastSavedEditorText !== editorText
  }

  function setEditorTextPreservingSelection(nextText: string, markSaved = false) {
    if (editorText === nextText) {
      if (markSaved) lastSavedEditorText = nextText
      return
    }
    const textarea = editorElement
    const shouldPreserveSelection = textarea && document.activeElement === textarea
    const previousText = editorText
    const previousStart = textarea?.selectionStart ?? previousText.length
    const previousEnd = textarea?.selectionEnd ?? previousStart
    const previousDirection = textarea?.selectionDirection ?? 'none'

    editorText = nextText
    if (markSaved) lastSavedEditorText = nextText

    if (!shouldPreserveSelection || !textarea) return
    const nextStart = mapTextPosition(previousText, nextText, previousStart)
    const nextEnd = mapTextPosition(previousText, nextText, previousEnd)
    queueMicrotask(() => {
      if (document.activeElement !== textarea) return
      textarea.setSelectionRange(nextStart, nextEnd, previousDirection)
    })
  }

  function mapTextPosition(previousText: string, nextText: string, position: number) {
    let prefixLength = 0
    const shortestLength = Math.min(previousText.length, nextText.length)
    while (prefixLength < shortestLength && previousText[prefixLength] === nextText[prefixLength]) prefixLength += 1

    let suffixLength = 0
    while (
      suffixLength < previousText.length - prefixLength &&
      suffixLength < nextText.length - prefixLength &&
      previousText[previousText.length - 1 - suffixLength] === nextText[nextText.length - 1 - suffixLength]
    ) {
      suffixLength += 1
    }

    const previousChangedEnd = previousText.length - suffixLength
    const nextChangedEnd = nextText.length - suffixLength
    if (position <= prefixLength) return position
    if (position >= previousChangedEnd) return Math.max(0, position + nextText.length - previousText.length)
    return nextChangedEnd
  }

  async function refreshSelectedDocumentFromServer(documentId = selectedNote?.documentId) {
    if (isLocalRuntime) return
    if (!documentId || !envelope || !canApplyRemoteDocumentRefresh(documentId)) return
    try {
      const document = await services.api.get<CrdtDocumentState>(`/api/v1/documents/${documentId}`)
      if (selectedNote?.documentId !== documentId || !canApplyRemoteDocumentRefresh(documentId)) {
        deferredDocumentRefreshId = documentId
        logSyncEvent('Deferred server document refresh while local editor state is active')
        return
      }
      envelope = mergeEnvelope(envelope, {
        cursors: envelope.cursors,
        apps: [],
        noteFolders: [],
        notes: [],
        documents: [document],
        documentUpdates: document.updates,
        tombstones: [],
        conflicts: [],
      })
      await services.cache.saveEnvelope(envelope)
      lastDocumentRefreshAt = new Date().toLocaleTimeString()
      refreshSelectedEditorFromEnvelope(documentId)
    } catch (error) {
      recordSyncError(error, 'Could not fetch the selected document from the server.')
      // Some local notes may not exist on the remote yet; queued sync will create them.
    }
  }

  function canApplyRemoteDocumentRefresh(documentId = selectedNote?.documentId) {
    if (!documentId || saveTimer || flushTimer) return false
    if (hasQueuedLocalDocumentChange(documentId) || shouldProtectEditorSelection(documentId)) return false
    if (document.activeElement === editorElement && hasPendingLocalEditorChange(documentId)) return false
    return true
  }

  function startRemotePullFallback() {
    if (isLocalRuntime) return
    if (pullTimer) clearInterval(pullTimer)
    pullTimer = setInterval(async () => {
      if (!selectedNote || saveTimer || flushTimer) return
      await refreshQueuedOperationCount()
      if (queuedOperationCount > 0 || hasQueuedLocalDocumentChange() || shouldProtectEditorSelection()) return
      try {
        envelope = await pullChanges(services)
        lastSyncPullAt = new Date().toLocaleTimeString()
        refreshSelectedEditorFromEnvelope()
        await refreshSelectedDocumentFromServer()
        if (statusDialogOpen && selectedNote) void loadDocumentVersions({ silent: true })
      } catch (error) {
        lastSyncError = `${new Date().toLocaleTimeString()} - pull fallback failed`
        console.debug('Remote pull fallback failed', error)
        // Live websocket is the primary path; this fallback can fail quietly offline.
      }
    }, 1200)
  }

  async function deleteSelectedNote() {
    if (!selectedNote) return
    if (tokens.confirmDelete && !window.confirm(`Delete "${selectedNote.title}"?`)) return
    await queueWorkspaceOperation({
      kind: 'delete_note',
      id: selectedNote.id,
      deletedAt: new Date().toISOString(),
    })
    await refreshQueuedOperationCount()
    envelope = await services.cache.loadEnvelope()
    selectedNoteId = ''
    status = 'Deleted locally'
  }

  async function deleteSelectedFolder() {
    const folderPath = normalizeFolderPath(selectedFolderPath)
    if (!selectedFolderCanDelete || folderPath === '/') return

    const noteCount = selectedFolderNotes.length
    const folderCount = selectedFolderFolders.length
    const noteLabel = `${noteCount} note${noteCount === 1 ? '' : 's'}`
    const folderLabel = `${folderCount} folder${folderCount === 1 ? '' : 's'}`
    if (tokens.confirmDelete && !window.confirm(`Delete "${folderPath}" and its ${noteLabel}/${folderLabel}?`)) return

    const deletedAt = new Date().toISOString()
    for (const note of selectedFolderNotes) {
      await queueWorkspaceOperation({
        kind: 'delete_note',
        id: note.id,
        deletedAt,
      })
    }
    for (const folder of selectedFolderFolders) {
      await queueWorkspaceOperation({
        kind: 'delete_note_folder',
        id: folder.id,
        deletedAt,
      })
    }

    const selectedNoteWasDeleted = selectedNote ? selectedFolderNotes.some((note) => note.id === selectedNote.id) : false
    envelope = await services.cache.loadEnvelope()
    await refreshQueuedOperationCount()
    selectedFolderPath = ''
    activeFolderPath = '/'
    if (selectedNoteWasDeleted) selectedNoteId = ''
    status = `Deleted folder ${folderPath}`
  }

  async function deleteSelectedFileTarget() {
    if (selectedFolderCanDelete) {
      await deleteSelectedFolder()
      return
    }
    await deleteSelectedNote()
  }

  function startDeleteButtonPress() {
    cancelDeleteButtonPress()
    deleteButtonPressTimer = setTimeout(() => {
      deleteButtonPressTimer = null
      enterMultiSelectMode()
    }, 500)
  }

  function cancelDeleteButtonPress() {
    if (deleteButtonPressTimer) {
      clearTimeout(deleteButtonPressTimer)
      deleteButtonPressTimer = null
    }
  }

  function handleDeleteButtonClick() {
    // A long-press already fired enterMultiSelectMode and cleared the
    // timer, so a real click event following it (pointerup->click) would
    // otherwise also run the normal single-delete action. Guard on mode.
    if (multiSelectMode) return
    void deleteSelectedFileTarget()
  }

  function enterMultiSelectMode() {
    multiSelectMode = true
    multiSelectedNoteIds = new Set()
    downloadSelectedNoteId = ''
    status = 'Select notes to delete'
  }

  function exitMultiSelectMode() {
    multiSelectMode = false
    multiSelectedNoteIds = new Set()
  }

  function toggleMultiSelectNote(noteId: string) {
    const next = new Set(multiSelectedNoteIds)
    if (next.has(noteId)) next.delete(noteId)
    else next.add(noteId)
    multiSelectedNoteIds = next
  }

  async function deleteMultiSelectedNotes() {
    const ids = Array.from(multiSelectedNoteIds)
    if (ids.length === 0) return
    if (tokens.confirmDelete && !window.confirm(`Delete ${ids.length} note${ids.length === 1 ? '' : 's'}?`)) return
    multiSelectDeleteBusy = true
    try {
      const deletedAt = new Date().toISOString()
      for (const id of ids) {
        await queueWorkspaceOperation({ kind: 'delete_note', id, deletedAt })
      }
      await refreshQueuedOperationCount()
      envelope = await services.cache.loadEnvelope()
      if (selectedNote && ids.includes(selectedNote.id)) selectedNoteId = ''
      status = `Deleted ${ids.length} note${ids.length === 1 ? '' : 's'}`
      exitMultiSelectMode()
    } finally {
      multiSelectDeleteBusy = false
    }
  }

  function downloadSelectedNote() {
    // A note held-selected for download (see downloadSelectedNoteId) isn't
    // open in the editor, so editorText won't have its content — read the
    // current text straight from its document state instead, the same way
    // any other not-currently-open note's text is derived elsewhere.
    if (downloadSelectedNoteId) {
      const note = notes.find((item) => item.id === downloadSelectedNoteId)
      const documentState = note && envelope?.documents.find((item) => item.id === note.documentId)
      if (note && documentState) {
        downloadNoteText(note.title, applyUpdates(documentState).text)
        return
      }
    }
    if (!selectedNote) return
    downloadNoteText(selectedNote.title, editorText)
  }

  function downloadNoteText(title: string, text: string) {
    const safeTitle = (title.trim() || 'Untitled note').replace(/[^\w.-]+/g, '-').replace(/^-+|-+$/g, '') || 'note'
    const blob = new Blob([text], { type: 'text/markdown;charset=utf-8' })
    const url = URL.createObjectURL(blob)
    const link = document.createElement('a')
    link.href = url
    link.download = `${safeTitle}.md`
    link.click()
    URL.revokeObjectURL(url)
    status = 'Downloaded note'
  }

  function runCommand(command: string) {
    listMenuOpen = false
    if (editorRenderMode === 'rich' && runRichCommand(command)) return
    if (command.startsWith('table-')) tableMenuOpen = false
    if (command === 'undo') undoRedo('undo')
    if (command === 'redo') undoRedo('redo')
    if (command === 'save') void saveDocument()
    if (command === 'heading-1') setHeading(1)
    if (command === 'heading-2') setHeading(2)
    if (command === 'heading-3') setHeading(3)
    if (command === 'heading-4') setHeading(4)
    if (command === 'heading-5') setHeading(5)
    if (command === 'heading-6') setHeading(6)
    if (command === 'paragraph') setHeading(0)
    if (command === 'bold') wrapSelection('**', '**', 'bold text')
    if (command === 'italic') wrapSelection('*', '*', 'italic text')
    if (command === 'underline') wrapSelection('<u>', '</u>', 'underlined text')
    if (command === 'strikethrough') wrapSelection('~~', '~~', 'struck text')
    if (command === 'quote') transformSelectedLines((line) => (line.startsWith('> ') ? line : `> ${line}`))
    if (command === 'code-block') wrapSelection('```\n', '\n```', 'code')
    if (command === 'divider') insertBlock('\n---\n')
    if (command === 'indent') transformSelectedLines((line) => `  ${line}`)
    if (command === 'outdent') transformSelectedLines((line) => line.replace(/^( {1,2}|\t)/, ''))
    if (command === 'list-dash') applyListStyle('dash')
    if (command === 'list-star') applyListStyle('star')
    if (command === 'list-checkbox') applyListStyle('checkbox')
    if (command === 'list-numbered') applyListStyle('numbered')
    if (command === 'list-emoji') applyListStyle('emoji')
    if (command === 'align-left') wrapSelection('<div style="text-align: left;">\n', '\n</div>', 'Aligned text')
    if (command === 'align-center') wrapSelection('<div style="text-align: center;">\n', '\n</div>', 'Aligned text')
    if (command === 'align-right') wrapSelection('<div style="text-align: right;">\n', '\n</div>', 'Aligned text')
    if (command === 'table') insertTable()
    if (command === 'table-add-row') updateMarkdownTable('add-row')
    if (command === 'table-remove-row') updateMarkdownTable('remove-row')
    if (command === 'table-add-column') updateMarkdownTable('add-column')
    if (command === 'table-remove-column') updateMarkdownTable('remove-column')
    if (command === 'image') insertImage()
    if (command === 'link') insertLink()
  }

  function toggleListMenu(event: MouseEvent) {
    if (listMenuOpen) {
      listMenuOpen = false
      return
    }
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
    const left = Math.max(8, Math.min(rect.left, window.innerWidth - 206))
    const top = Math.min(rect.bottom + 6, window.innerHeight - 220)
    listMenuStyle = `top: ${top}px; left: ${left}px;`
    listMenuOpen = true
  }

  function handleToolbarMouseDown(event: MouseEvent) {
    const target = event.target as HTMLElement | null
    if (!target?.closest('button')) return
    event.preventDefault()
  }

  function selection() {
    const textarea = editorElement
    const hasLiveSelection = Boolean(textarea && document.activeElement === textarea)
    const start = hasLiveSelection && textarea ? textarea.selectionStart : lastTextSelection.start
    const end = hasLiveSelection && textarea ? textarea.selectionEnd : lastTextSelection.end
    return { start, end, selected: editorText.slice(start, end) }
  }

  function captureTextSelection() {
    if (!editorElement) return
    lastTextSelection = {
      start: editorElement.selectionStart,
      end: editorElement.selectionEnd,
    }
    markEditorInteraction()
  }

  function undoRedo(command: 'undo' | 'redo') {
    editorElement?.focus()
    document.execCommand(command)
  }

  function replaceRange(start: number, end: number, replacement: string, nextStart = start, nextEnd = start + replacement.length) {
    editorText = `${editorText.slice(0, start)}${replacement}${editorText.slice(end)}`
    lastTextSelection = { start: nextStart, end: nextEnd }
    scheduleDocumentSave()
    queueMicrotask(() => {
      editorElement?.focus()
      editorElement?.setSelectionRange(nextStart, nextEnd)
    })
  }

  function wrapSelection(prefix: string, suffix: string, placeholder: string) {
    const { start, end, selected } = selection()
    const content = selected || placeholder
    replaceRange(start, end, `${prefix}${content}${suffix}`, start + prefix.length, start + prefix.length + content.length)
  }

  function insertBlock(block: string) {
    const { start, end } = selection()
    const prefix = start > 0 && !editorText.slice(0, start).endsWith('\n') ? '\n' : ''
    const suffix = end < editorText.length && !editorText.slice(end).startsWith('\n') ? '\n' : ''
    replaceRange(start, end, `${prefix}${block}${suffix}`)
  }

  function transformSelectedLines(transform: (line: string, index: number) => string) {
    const { start, end } = selection()
    const lineStart = editorText.lastIndexOf('\n', Math.max(0, start - 1)) + 1
    const nextBreak = editorText.indexOf('\n', end)
    const lineEnd = nextBreak === -1 ? editorText.length : nextBreak
    const block = editorText.slice(lineStart, lineEnd)
    replaceRange(lineStart, lineEnd, block.split('\n').map(transform).join('\n'))
  }

  function applyListStyle(style: ListStyle) {
    const emoji = style === 'emoji' ? window.prompt('Emoji bullet', '✅')?.trim() || '✅' : ''
    const markerForStyle = (index: number) => {
      if (style === 'star') return '- ★ '
      if (style === 'checkbox') return '- [ ] '
      if (style === 'numbered') return `${index + 1}. `
      if (style === 'emoji') return `- ${emoji} `
      return '- '
    }
    transformSelectedLines((line, index) => {
      const content = line.replace(/^(\s*)(?:[-*]\s+\[[ xX]\]\s+|[-*]\s+(?:[★•]\s+|[\p{Extended_Pictographic}]\s+)?|[★•]\s+|\d+\.\s+)/u, '$1')
      const indent = content.match(/^\s*/)?.[0] ?? ''
      const text = content.slice(indent.length) || 'List item'
      return `${indent}${markerForStyle(index)}${text}`
    })
  }

  function setHeading(level: number) {
    transformSelectedLines((line) => {
      const cleaned = line.replace(/^#{1,6}\s*/, '')
      return level === 0 ? cleaned : `${'#'.repeat(level)} ${cleaned || 'Heading'}`
    })
  }

  function getMarkdownHeadings(text: string): TocHeading[] {
    const headings: TocHeading[] = []
    let position = 0
    for (const [index, line] of text.split('\n').entries()) {
      const match = /^(#{1,6})\s+(.+?)\s*$/.exec(line)
      if (match) {
        headings.push({
          id: `markdown-heading-${index}`,
          level: match[1].length,
          title: match[2],
          position,
        })
      }
      position += line.length + 1
    }
    return headings
  }

  function getRichHeadings(_version: number): TocHeading[] {
    if (!richEditor) return []
    const headings: TocHeading[] = []
    richEditor.state.doc.descendants((node, position) => {
      if (node.type.name !== 'heading') return
      const title = node.textContent.trim()
      if (!title) return
      headings.push({
        id: `rich-heading-${position}`,
        level: Number(node.attrs.level ?? 1),
        title,
        position,
      })
    })
    return headings
  }

  function jumpToHeading(heading: TocHeading) {
    tocOpen = false
    if (editorRenderMode === 'rich' && richEditor) {
      richEditor.chain().focus().setTextSelection(Math.min(heading.position + 1, richEditor.state.doc.content.size)).scrollIntoView().run()
      return
    }
    queueMicrotask(() => {
      editorElement?.focus()
      editorElement?.setSelectionRange(heading.position, heading.position)
      const lineIndex = editorText.slice(0, heading.position).split('\n').length - 1
      editorElement?.scrollTo({ top: Math.max(0, lineIndex * 24 - 24), behavior: 'smooth' })
    })
  }

  function applyColor(kind: 'color' | 'highlight', color: string) {
    if (editorRenderMode === 'rich' && richEditor) {
      if (kind === 'color') richEditor.chain().focus().setColor(color).run()
      if (kind === 'highlight') richEditor.chain().focus().toggleHighlight({ color }).run()
      return
    }
    if (kind === 'color') wrapSelection(`<span style="color: ${color};">`, '</span>', 'colored text')
    if (kind === 'highlight') wrapSelection(`<mark style="background: ${color};">`, '</mark>', 'highlighted text')
  }

  function applyFont(fontFamily: string) {
    if (!fontFamily) return
    if (editorRenderMode === 'rich' && richEditor) {
      richEditor.chain().focus().setFontFamily(fontFamily).run()
      return
    }
    wrapSelection(`<span style="font-family: ${fontFamily};">`, '</span>', 'font text')
  }

  function insertLink() {
    const url = window.prompt('Link URL')
    if (!url) return
    const { selected } = selection()
    wrapSelection('[', `](${url})`, selected || 'link text')
  }

  function insertImage() {
    const url = window.prompt('Image URL')
    if (!url) return
    const alt = window.prompt('Alt text') ?? 'image'
    insertBlock(`![${alt}](${url})`)
  }

  function insertTable() {
    insertBlock('\n| Column 1 | Column 2 | Column 3 |\n| --- | --- | --- |\n| Cell | Cell | Cell |\n')
  }

  function updateMarkdownTable(action: 'add-row' | 'remove-row' | 'add-column' | 'remove-column') {
    const current = selection()
    const lines = editorText.split('\n')
    const beforeCursor = editorText.slice(0, current.start).split('\n')
    const cursorLine = beforeCursor.length - 1
    let start = cursorLine
    let end = cursorLine
    while (start > 0 && isTableLine(lines[start - 1])) start -= 1
    while (end < lines.length - 1 && isTableLine(lines[end + 1])) end += 1
    if (!isTableLine(lines[cursorLine]) || end - start < 1) {
      insertTable()
      return
    }
    const table = lines.slice(start, end + 1)
    const columnCount = Math.max(...table.map((line) => splitTableRow(line).length), 2)
    if (action === 'add-row') table.push(tableRow(Array.from({ length: columnCount }, () => 'Cell')))
    if (action === 'remove-row' && table.length > 2) table.splice(Math.max(2, cursorLine - start), 1)
    if (action === 'add-column') {
      for (let index = 0; index < table.length; index += 1) {
        const cells = splitTableRow(table[index])
        cells.push(index === 1 ? '---' : index === 0 ? `Column ${cells.length + 1}` : 'Cell')
        table[index] = tableRow(cells)
      }
    }
    if (action === 'remove-column' && columnCount > 1) {
      for (let index = 0; index < table.length; index += 1) {
        const cells = splitTableRow(table[index])
        cells.pop()
        table[index] = tableRow(cells)
      }
    }
    lines.splice(start, end - start + 1, ...table)
    editorText = lines.join('\n')
    scheduleDocumentSave()
  }

  function isTableLine(line: string) {
    return /^\s*\|.*\|\s*$/.test(line)
  }

  function splitTableRow(line: string) {
    return line.trim().replace(/^\|/, '').replace(/\|$/, '').split('|').map((cell) => cell.trim())
  }

  function tableRow(cells: string[]) {
    return `| ${cells.join(' | ')} |`
  }

  async function createFolder() {
    const name = window.prompt('Folder name')
    if (!name) return
    const folderPath = normalizeFolderPath(`${activeFolderPath}/${name}`)
    const existingFolder = noteFolders.find((folder) => normalizeFolderPath(folder.path) === folderPath)
    if (!existingFolder) {
      const now = new Date().toISOString()
      const folder: NoteFolder = {
        id: createRuntimeId('folder'),
        path: folderPath,
        name: folderName(folderPath),
        ownerId: 'local-user',
        workspaceId: 'default',
        createdAt: now,
        updatedAt: now,
      }
      await queueWorkspaceOperation({ kind: 'create_note_folder', folder })
      await refreshQueuedOperationCount()
      envelope = await services.cache.loadEnvelope()
    }
    activeFolderPath = folderPath
    selectedFolderPath = folderPath
    status = `Folder selected: ${activeFolderPath}`
  }

  async function handleUploadFiles(files: FileList | null) {
    if (!files) return
    for (const file of Array.from(files)) {
      if (!/\.(md|txt)$/i.test(file.name)) {
        status = 'Only .txt and .md uploads are supported'
        continue
      }
      await createNoteFromUpload(file, await file.text())
    }
    if (uploadInputElement) uploadInputElement.value = ''
  }

  function normalizeFolderPath(path: string) {
    const normalized = `/${path}`.replace(/\/+/g, '/').replace(/\/$/, '')
    return normalized === '' ? '/' : normalized
  }

  function folderName(path: string) {
    const normalized = normalizeFolderPath(path)
    return normalized === '/' ? 'Root' : normalized.split('/').filter(Boolean).at(-1) ?? normalized
  }

  function parentFolderPath(path: string) {
    const parts = normalizeFolderPath(path).split('/').filter(Boolean)
    parts.pop()
    return parts.length === 0 ? '/' : `/${parts.join('/')}`
  }

  function remapPath(path: string, sourcePath: string, movedPath: string) {
    const normalized = normalizeFolderPath(path)
    if (normalized === sourcePath) return movedPath
    return normalizeFolderPath(`${movedPath}/${normalized.slice(sourcePath.length)}`)
  }

  function isSameOrNestedPath(path: string, parentPath: string) {
    const normalizedPath = normalizeFolderPath(path)
    const normalizedParent = normalizeFolderPath(parentPath)
    return normalizedPath === normalizedParent || normalizedPath.startsWith(`${normalizedParent}/`)
  }

  function toggleFolderCollapsed(event: MouseEvent, path: string) {
    event.stopPropagation()
    const normalized = normalizeFolderPath(path)
    collapsedFolderPaths = collapsedFolderPaths.includes(normalized)
      ? collapsedFolderPaths.filter((item) => item !== normalized)
      : [...collapsedFolderPaths, normalized]
  }

  function isFolderCollapsed(path: string) {
    return collapsedFolderPaths.includes(normalizeFolderPath(path))
  }

  function toggleFavoriteNote(event: MouseEvent | KeyboardEvent | PointerEvent, noteId: string) {
    event.preventDefault()
    event.stopPropagation()
    favoriteNoteIds = favoriteNoteIds.includes(noteId)
      ? favoriteNoteIds.filter((id) => id !== noteId)
      : [...favoriteNoteIds, noteId]
    saveFavoriteNoteIds()
  }

  function isFavoriteNote(noteId: string) {
    return favoriteNoteIds.includes(noteId)
  }

  function handleFolderKey(event: KeyboardEvent, path: string) {
    if (event.key !== 'Enter' && event.key !== ' ') return
    event.preventDefault()
    selectFolder(path)
  }

  function refreshCustomFonts() {
    customFonts = loadStoredFonts()
  }

  function updateMobileKeyboardInset() {
    const viewport = window.visualViewport
    const inset = viewport ? Math.max(0, window.innerHeight - viewport.height - viewport.offsetTop) : 0
    document.documentElement.style.setProperty('--notes-keyboard-inset', `${Math.round(inset)}px`)
  }

  function folderMatchesSearch(folder: NoteFolder) {
    const query = searchQuery.trim().toLowerCase()
    if (!query) return true
    return `${folder.name} ${folder.path}`.toLowerCase().includes(query)
  }

  start()

  window.addEventListener(customFontsChangedEvent, refreshCustomFonts)

  onMount(() => {
    if ('virtualKeyboard' in navigator) {
      ;(navigator as Navigator & { virtualKeyboard?: { overlaysContent: boolean } }).virtualKeyboard!.overlaysContent = true
    }
    updateMobileKeyboardInset()
    window.visualViewport?.addEventListener('resize', updateMobileKeyboardInset)
    window.visualViewport?.addEventListener('scroll', updateMobileKeyboardInset)
    window.addEventListener('resize', updateMobileKeyboardInset)
    document.addEventListener('visibilitychange', handleVisibilityFlush)
    window.addEventListener('pagehide', flushAllPendingWork)
    return () => {
      document.documentElement.style.removeProperty('--notes-keyboard-inset')
      window.visualViewport?.removeEventListener('resize', updateMobileKeyboardInset)
      window.visualViewport?.removeEventListener('scroll', updateMobileKeyboardInset)
      window.removeEventListener('resize', updateMobileKeyboardInset)
      document.removeEventListener('visibilitychange', handleVisibilityFlush)
      window.removeEventListener('pagehide', flushAllPendingWork)
    }
  })

  onDestroy(() => {
    window.removeEventListener(customFontsChangedEvent, refreshCustomFonts)
    unsubscribePresence?.()
    unsubscribeDocumentUpdates?.()
    destroyRichEditor()
    destroyHeadlessSyncEditor()
    if (saveTimer) clearTimeout(saveTimer)
    if (flushTimer) clearTimeout(flushTimer)
    for (const timer of liveUpdateFallbackTimers.values()) clearTimeout(timer)
    if (pullTimer) clearInterval(pullTimer)
  })

  $: applyTokens(tokens)

  function updateTokens(nextTokens: DesignTokens) {
    tokens = nextTokens
    services.tokens = nextTokens
    saveStoredTokens(nextTokens)
    applyTokens(nextTokens)
  }

  function startSidebarResize(event: PointerEvent) {
    resizingSidebar = true
    const target = event.currentTarget as HTMLElement | null
    target?.setPointerCapture(event.pointerId)
  }

  function resizeSidebar(event: PointerEvent) {
    if (!resizingSidebar) return
    sidebarWidth = Math.min(560, Math.max(260, event.clientX))
  }

  function stopSidebarResize(event: PointerEvent) {
    resizingSidebar = false
    const target = event.currentTarget as HTMLElement | null
    target?.releasePointerCapture(event.pointerId)
  }

  async function setEditorRenderMode(mode: EditorRenderMode) {
    if (editorRenderMode === 'rich' && mode !== 'rich') {
      exportRichEditorToMarkdown()
      scheduleDocumentSave()
    }
    editorRenderMode = mode
    if (typeof localStorage !== 'undefined') localStorage.setItem('og-suite:notes:editor-render-mode', mode)
  }

  function loadEditorRenderMode(): EditorRenderMode {
    if (typeof localStorage === 'undefined') return 'text'
    const stored = localStorage.getItem('og-suite:notes:editor-render-mode')
    if (stored === 'markdown' && isMobilePlatform) return 'rich'
    if (stored === 'markdown' || stored === 'rich') return stored
    return 'text'
  }

  function loadFavoriteNoteIds() {
    if (typeof localStorage === 'undefined') return []
    try {
      const stored = JSON.parse(localStorage.getItem('og-suite:notes:favorite-note-ids') ?? '[]')
      return Array.isArray(stored) ? stored.filter((id): id is string => typeof id === 'string') : []
    } catch {
      return []
    }
  }

  function saveFavoriteNoteIds() {
    if (typeof localStorage === 'undefined') return
    localStorage.setItem('og-suite:notes:favorite-note-ids', JSON.stringify(favoriteNoteIds))
  }

  function renderMarkdown(source: string) {
    const html = marked.parse(normalizeMarkdownHeadings(source), { async: false }) as string
    return sanitizeRenderedMarkdown(html)
  }

  function normalizeMarkdownHeadings(source: string) {
    return source.replace(/^(#{1,6})([^\s#].*)$/gm, (_match, hashes: string, text: string) => `${hashes} ${text}`)
  }

  function sanitizeRenderedMarkdown(html: string) {
    return DOMPurify.sanitize(html, {
      USE_PROFILES: { html: true },
      ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto|tel):|[^a-z]|[a-z+.-]+(?:[^a-z+.-:]|$))/i,
    })
  }

  function sanitizeInlineStyle(style: string) {
    return style
      .split(';')
      .map((part) => part.trim())
      .filter((part) => {
        const property = part.split(':', 1)[0]?.trim().toLowerCase()
        return property === 'color'
          || property === 'background'
          || property === 'background-color'
          || property === 'font-family'
          || property === 'text-decoration'
      })
      .join('; ')
  }

  function escapeHtmlAttribute(value: string) {
    return value.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;')
  }

  const IndentExtension = Extension.create({
    name: 'indent',
    addGlobalAttributes() {
      return [
        {
          types: ['paragraph', 'heading'],
          attributes: {
            indent: {
              default: 0,
              parseHTML: (element) => Number(element.getAttribute('data-indent') ?? 0),
              renderHTML: (attributes) => {
                const indent = Number(attributes.indent ?? 0)
                if (!indent) return {}
                return {
                  'data-indent': String(indent),
                  style: `margin-left: ${indent * 1.5}em;`,
                }
              },
            },
          },
        },
      ]
    },
    addCommands() {
      return {
        indent:
          () =>
          ({ editor, chain }: any) => {
            const nodeName = editor.isActive('heading') ? 'heading' : 'paragraph'
            const currentIndent = Number(editor.getAttributes(nodeName).indent ?? 0)
            return chain()
              .updateAttributes(nodeName, { indent: Math.min(currentIndent + 1, 8) })
              .run()
          },
        outdent:
          () =>
          ({ editor, chain }: any) => {
            const nodeName = editor.isActive('heading') ? 'heading' : 'paragraph'
            const currentIndent = Number(editor.getAttributes(nodeName).indent ?? 0)
            return chain()
              .updateAttributes(nodeName, { indent: Math.max(currentIndent - 1, 0) })
              .run()
          },
      } as any
    },
  })

  // Shared between the real (visible, DOM-bound) Rich editor and the
  // headless one-off editor used to translate a TXT/MD-mode edit into
  // richContent (see syncRichFragmentFromMarkdown) — both must resolve to
  // the exact same ProseMirror schema, or content written by one and read
  // by the other can silently lose marks/nodes the other side doesn't know
  // about.
  function richContentExtensions(liveDoc: Y.Doc) {
    return [
      Collaboration.configure({ document: liveDoc, field: 'richContent' }),
      StarterKit.configure({ undoRedo: false }),
      Underline,
      Link.configure({
        openOnClick: false,
        autolink: true,
        protocols: ['http', 'https', 'mailto', 'tel'],
      }),
      Image,
      TextStyle,
      FontFamily,
      Color,
      Highlight.configure({ multicolor: true }),
      TextAlign.configure({ types: ['heading', 'paragraph'] }),
      TaskList,
      TaskItem.configure({ nested: true }),
      IndentExtension,
      Table.configure({ resizable: true }),
      TableRow,
      TableHeader,
      TableCell,
    ]
  }

  // The mirror image of syncPlainTextFieldFromMarkdown: a TXT/MD-mode edit
  // only ever touches the plain "content" Y.Text, never richContent — so
  // without this, a Rich-mode device never sees the edit, and its next
  // save (which always re-derives "content" from its own richContent)
  // silently reverts it. This is a real bug reported live: typing in the
  // web UI (TXT mode) did nothing on the phone app (Rich mode), and the
  // phone's next edit wiped the web UI's change out from under it.
  //
  // There's no plain-text equivalent of replaceTextWithMinimalEdit for a
  // Y.XmlFragment (structural tree, not a flat sequence), so this goes
  // through a real, detached Tiptap Editor bound to a live Y.Doc via
  // Collaboration — the same machinery the visible Rich editor uses — and
  // lets it translate a full setContent() into the correct Y updates via
  // ySyncPlugin. Like the initial-seed case this can clobber concurrent
  // Rich-mode formatting on another device if both edit at once; that's an
  // accepted tradeoff over the previous behavior of never propagating at
  // all.
  //
  // Reported slow/unsnappy typing on Android traced partly to this: every
  // debounced save was building and tearing down a whole Editor instance
  // (all 15+ extensions, a fresh ProseMirror EditorState) from scratch.
  // Kept alive per documentId instead — same lifecycle as the visible Rich
  // editor's liveYDoc — so a plain-mode editing session pays that cost
  // once, not on every save.
  let headlessSyncEditor: Editor | null = null
  let headlessSyncDoc: Y.Doc | null = null
  let headlessSyncDocumentId = ''
  let headlessSyncVector: Uint8Array | null = null

  function destroyHeadlessSyncEditor() {
    headlessSyncEditor?.destroy()
    headlessSyncEditor = null
    headlessSyncDoc?.destroy()
    headlessSyncDoc = null
    headlessSyncDocumentId = ''
    headlessSyncVector = null
  }

  function syncRichFragmentFromMarkdown(documentId: string, baseState: CrdtDocumentState, markdown: string): string | null {
    if (headlessSyncDocumentId !== documentId) {
      destroyHeadlessSyncEditor()
      headlessSyncDoc = hydrateLiveYDoc(baseState)
      headlessSyncVector = getDocStateVector(headlessSyncDoc)
      headlessSyncEditor = new Editor({
        element: window.document.createElement('div'),
        extensions: richContentExtensions(headlessSyncDoc),
        editable: true,
      })
      headlessSyncDocumentId = documentId
    }
    headlessSyncEditor!.commands.setContent(renderMarkdown(markdown), { emitUpdate: true })
    const payload = encodeDocUpdateSince(headlessSyncDoc!, headlessSyncVector ?? getDocStateVector(headlessSyncDoc!))
    headlessSyncVector = getDocStateVector(headlessSyncDoc!)
    return payload
  }

  function ensureRichEditor() {
    if (!selectedNote || !selectedDocument || !richEditorElement) return
    if (richEditor && richDocumentId === selectedNote.documentId) return
    destroyRichEditor()

    richDocumentId = selectedNote.documentId
    liveYDoc = hydrateLiveYDoc(selectedDocument)
    liveYDocLastVector = getDocStateVector(liveYDoc)
    const richFragment = getRichFragment(liveYDoc)
    const richFragmentWasEmpty = richFragment.length === 0

    richEditor = new Editor({
      element: richEditorElement,
      extensions: richContentExtensions(liveYDoc),
      editorProps: {
        attributes: {
          class: 'rich-editor-content',
          'aria-label': 'Rich text editor',
        },
      },
      onUpdate: () => {
        markEditorInteraction()
        requestRichActiveStateRefresh()
        scheduleDocumentSave()
        status = 'Rich text editing locally'
        services.presence.publishCursor(selectedNote.documentId, richEditor?.state.selection.from ?? null)
      },
      onFocus: () => {
        editorFocused = true
        markEditorInteraction()
      },
      onSelectionUpdate: () => {
        markEditorInteraction()
        requestRichActiveStateRefresh()
      },
      onBlur: () => {
        editorFocused = false
        // Unlike the plain-text textarea's handleEditorBlur, this used to
        // skip straight to applyDeferredDocumentRefresh() without saving
        // first. Blurring cancels nothing — the debounced save timer keeps
        // running — but on mobile, backgrounding the app (switching apps,
        // locking the screen) right after typing throttles/pauses that
        // timer before it fires. Any deferred refresh that then runs
        // reads the last *committed* text, not what was just typed —
        // exactly the "typed something, it reverted" symptom. Saving
        // first, same as the textarea path, closes that gap.
        void (async () => {
          await saveDocument()
          applyDeferredDocumentRefresh()
        })()
      },
      onTransaction: () => {
        requestRichActiveStateRefresh()
      },
    })

    // First time this document has ever been opened in Rich mode: the
    // Y.XmlFragment the Collaboration extension reads from is empty, so
    // seed it from the note's markdown source. setContent goes through the
    // same transaction pipeline as typing, so this generates real Yjs ops
    // into richFragment via the binding rather than bypassing it — no
    // separate "initial state" concept to keep in sync.
    if (richFragmentWasEmpty && editorText.trim()) {
      richEditor.commands.setContent(renderMarkdown(editorText), { emitUpdate: true })
    }

  }

  function requestRichActiveStateRefresh() {
    if (richActiveStateFrame !== null) return
    richActiveStateFrame = window.requestAnimationFrame(() => {
      richActiveStateFrame = null
      richActiveStateVersion += 1
      updateRichTableToolsFromSelection()
    })
  }

  function destroyRichEditor() {
    if (richActiveStateFrame !== null) {
      window.cancelAnimationFrame(richActiveStateFrame)
      richActiveStateFrame = null
    }
    richEditor?.destroy()
    richEditor = null
    richDocumentId = ''
    liveYDoc?.destroy()
    liveYDoc = null
    liveYDocLastVector = null
    tableMenuOpen = false
    richTableMenuStyle = ''
  }

  function exportRichEditorToMarkdown() {
    if (!richEditor) return
    const markdown = turndown.turndown(sanitizeRenderedMarkdown(richEditor.getHTML()))
    editorText = markdown.trim() ? `${markdown.trimEnd()}\n` : ''
  }

  function runRichCommand(command: string) {
    if (!richEditor) return false
    if (command.startsWith('table-')) tableMenuOpen = false
    if (command === 'undo') return richEditor.chain().focus().undo().run()
    if (command === 'redo') return richEditor.chain().focus().redo().run()
    if (command === 'save') {
      void saveDocument()
      return true
    }
    if (command === 'paragraph') return richEditor.chain().focus().setParagraph().run()
    if (command.startsWith('heading-')) return richEditor.chain().focus().setHeading({ level: Number(command.replace('heading-', '')) as 1 | 2 | 3 | 4 | 5 | 6 }).run()
    if (command === 'bold') return richEditor.chain().focus().toggleBold().run()
    if (command === 'italic') return richEditor.chain().focus().toggleItalic().run()
    if (command === 'underline') return richEditor.chain().focus().toggleUnderline().run()
    if (command === 'strikethrough') return richEditor.chain().focus().toggleStrike().run()
    if (command === 'quote') return richEditor.chain().focus().toggleBlockquote().run()
    if (command === 'code-block') return richEditor.chain().focus().toggleCodeBlock().run()
    if (command === 'divider') return richEditor.chain().focus().setHorizontalRule().run()
    if (command === 'list-dash') return richEditor.chain().focus().toggleBulletList().run()
    if (command === 'list-star') return richEditor.chain().focus().toggleBulletList().insertContent('★ ').run()
    if (command === 'list-numbered') return richEditor.chain().focus().toggleOrderedList().run()
    if (command === 'list-checkbox') return richEditor.chain().focus().toggleTaskList().run()
    if (command === 'list-emoji') {
      const emoji = window.prompt('Emoji bullet', '✅')?.trim() || '✅'
      return richEditor.chain().focus().toggleBulletList().insertContent(`${emoji} `).run()
    }
    if (command === 'link') {
      const href = window.prompt('Link URL')
      if (href) return richEditor.chain().focus().setLink({ href }).run()
      return true
    }
    if (command === 'image') {
      const src = window.prompt('Image URL')
      if (!src) return true
      const alt = window.prompt('Alt text') ?? ''
      return richEditor.chain().focus().setImage({ src, alt }).run()
    }
    if (command === 'indent') {
      if (richEditor.isActive('listItem')) return richEditor.chain().focus().sinkListItem('listItem').run()
      return applyRichIndent(1)
    }
    if (command === 'outdent') {
      if (richEditor.isActive('listItem')) return richEditor.chain().focus().liftListItem('listItem').run()
      return applyRichIndent(-1)
    }
    if (command === 'align-left') return richEditor.chain().focus().setTextAlign('left').run()
    if (command === 'align-center') return richEditor.chain().focus().setTextAlign('center').run()
    if (command === 'align-right') return richEditor.chain().focus().setTextAlign('right').run()
    if (command === 'table') return richEditor.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run()
    if (command === 'table-add-row') return richEditor.chain().focus().addRowAfter().run()
    if (command === 'table-remove-row') return richEditor.chain().focus().deleteRow().run()
    if (command === 'table-add-column') return richEditor.chain().focus().addColumnAfter().run()
    if (command === 'table-remove-column') return richEditor.chain().focus().deleteColumn().run()
    return false
  }

  function updateRichTableToolsFromPointer(event: MouseEvent) {
    if (editorRenderMode !== 'rich' || !richEditorElement) return
    const table = (event.target as Element | null)?.closest?.('.rich-editor-content table') as HTMLElement | null
    if (!table) {
      if (!richEditor?.isActive('table')) {
        tableMenuOpen = false
        richTableMenuStyle = ''
      }
      return
    }
    positionRichTableTools(table)
  }

  function richTableHost(node: HTMLDivElement) {
    const updateFromPointer = (event: MouseEvent) => updateRichTableToolsFromPointer(event)
    const handleMouseLeave = () => {
      if (!richEditor?.isActive('table')) tableMenuOpen = false
    }
    node.addEventListener('mousemove', updateFromPointer)
    node.addEventListener('click', updateFromPointer)
    node.addEventListener('mouseleave', handleMouseLeave)
    return {
      destroy() {
        node.removeEventListener('mousemove', updateFromPointer)
        node.removeEventListener('click', updateFromPointer)
        node.removeEventListener('mouseleave', handleMouseLeave)
      },
    }
  }

  function updateRichTableToolsFromSelection() {
    if (editorRenderMode !== 'rich' || !richEditor || !richEditorElement || !richEditor.isActive('table')) {
      tableMenuOpen = false
      richTableMenuStyle = ''
      return
    }
    const { node } = richEditor.view.domAtPos(richEditor.state.selection.from)
    const element = node.nodeType === Node.ELEMENT_NODE ? (node as Element) : node.parentElement
    const table = element?.closest?.('table') as HTMLElement | null
    if (table) positionRichTableTools(table)
  }

  function positionRichTableTools(table: HTMLElement) {
    if (!richEditorElement) return
    const hostRect = richEditorElement.getBoundingClientRect()
    const tableRect = table.getBoundingClientRect()
    const top = Math.max(8, tableRect.top - hostRect.top + richEditorElement.scrollTop + 6)
    const left = Math.max(8, tableRect.right - hostRect.left + richEditorElement.scrollLeft - 36)
    richTableMenuStyle = `top: ${top}px; left: ${left}px;`
    tableMenuOpen = true
  }

  function applyRichIndent(delta: number) {
    if (!richEditor) return false
    richEditor.commands.focus()
    const { state, view } = richEditor
    const { from, to, empty, $from } = state.selection
    const tr = state.tr
    const updateNodeIndent = (position: number, node: any) => {
      if (node.type.name !== 'paragraph' && node.type.name !== 'heading') return
      const indent = Math.max(0, Math.min(Number(node.attrs.indent ?? 0) + delta, 8))
      tr.setNodeMarkup(position, undefined, { ...node.attrs, indent })
    }

    if (empty) {
      for (let depth = $from.depth; depth > 0; depth -= 1) {
        const node = $from.node(depth)
        if (node.type.name === 'paragraph' || node.type.name === 'heading') {
          updateNodeIndent($from.before(depth), node)
          break
        }
      }
    } else {
      state.doc.nodesBetween(from, to, (node, position) => {
        updateNodeIndent(position, node)
      })
    }

    if (!tr.docChanged) return true
    view.dispatch(tr.scrollIntoView())
    richActiveStateVersion += 1
    return true
  }

  function richCommandActive(command: string, _version = richActiveStateVersion) {
    richActiveStateVersion
    if (editorRenderMode !== 'rich' || !richEditor) return false
    if (command === 'bold') return richEditor.isActive('bold') || richStoredMarkActive('bold')
    if (command === 'italic') return richEditor.isActive('italic') || richStoredMarkActive('italic')
    if (command === 'underline') return richEditor.isActive('underline') || richStoredMarkActive('underline')
    if (command === 'strikethrough') return richEditor.isActive('strike') || richStoredMarkActive('strike')
    if (command === 'quote') return richEditor.isActive('blockquote')
    if (command === 'code-block') return richEditor.isActive('codeBlock')
    if (command === 'link') return richEditor.isActive('link')
    if (command === 'table') return richEditor.isActive('table')
    if (command === 'align-left') return richEditor.isActive({ textAlign: 'left' })
    if (command === 'align-center') return richEditor.isActive({ textAlign: 'center' })
    if (command === 'align-right') return richEditor.isActive({ textAlign: 'right' })
    if (command === 'indent') return Number(richEditor.getAttributes('paragraph').indent ?? richEditor.getAttributes('heading').indent ?? 0) > 0
    if (command === 'list-dash' || command === 'list-star' || command === 'list-emoji') return richEditor.isActive('bulletList')
    if (command === 'list-checkbox') return richEditor.isActive('taskList')
    if (command === 'list-numbered') return richEditor.isActive('orderedList')
    if (command === 'paragraph') return richEditor.isActive('paragraph')
    if (command.startsWith('heading-')) return richEditor.isActive('heading', { level: Number(command.replace('heading-', '')) })
    return false
  }

  function richStoredMarkActive(markName: string) {
    if (!richEditor) return false
    const markType = richEditor.schema.marks[markName]
    if (!markType) return false
    return Boolean(richEditor.state.storedMarks?.some((mark) => mark.type === markType))
  }
</script>

<main class:resizing-sidebar={resizingSidebar} class="notes-app" data-mode={mode} style={`--notes-pane-width: ${sidebarWidth}px;`}>
  <aside class:mobile-open={mobileFilesOpen} class="notes-list" aria-label="Notes">
    <div class="notes-list-panel">
      {#if mode === 'standalone'}
        <div class="notes-mobile-menu-bar" aria-label="Notes menu controls">
          <div class="notes-mobile-menu-title">
            <strong>Files</strong>
            <span>{activeServerLabel}</span>
          </div>
          <div class="notes-mobile-menu-actions">
            <button class="icon-only-button" aria-label="Settings" title="Settings" on:click={() => settingsOpen = true}>
              <Icon name="settings" size={18} />
            </button>
            <button class="icon-only-button" aria-label="Connected servers" title="Connected servers" on:click={() => onOpenServerManager?.()}>
              <Icon name="sync" size={18} />
            </button>
            <button class="notes-menu-close" aria-label="Close notes menu" title="Close notes menu" on:click={() => mobileFilesOpen = false}>
              <span aria-hidden="true"></span>
            </button>
          </div>
        </div>
      {/if}

      {#if mode === 'suite'}
        <MobileSuiteTopBar
          navItems={suiteNavItems}
          activeAppId={activeSuiteAppId}
          onSelectApp={selectSuiteApp}
          onOpenSettings={() => {
            openSuiteSettings()
            mobileFilesOpen = false
          }}
          onClose={() => mobileFilesOpen = false}
        />
      {/if}

      <div class="panel-title">
        <div class="header-actions">
          <button class="icon-only-button" aria-label="Search notes" title="Search notes" on:click={() => searchOpen = !searchOpen}>
            <Icon name="search" size={18} />
          </button>
          <button class="icon-only-button" aria-label="New folder" title="New folder" on:click={createFolder}>
            <Icon name="new-folder" size={18} />
          </button>
          <button class="icon-only-button" aria-label="Upload text or markdown" title="Upload text or markdown" on:click={() => uploadInputElement?.click()}>
            <Icon name="upload" size={18} />
          </button>
          <button
            class="icon-only-button"
            aria-label={selectedFolderPath ? 'Rename selected folder' : 'Rename selected note'}
            title={selectedFolderPath ? 'Rename selected folder' : 'Rename selected note'}
            disabled={!selectedFolderPath && !selectedNote}
            on:click={renameSelectedFileTarget}
          >
            <Icon name="rename" size={18} />
          </button>
          <button class="icon-only-button" aria-label="Download selected note" title="Download selected note" disabled={!selectedNote && !downloadSelectedNoteId} on:click={downloadSelectedNote}>
            <Icon name="download" size={18} />
          </button>
          <button
            class:active={multiSelectMode}
            class="icon-only-button danger-action"
            aria-label={selectedFolderCanDelete ? 'Delete selected folder' : 'Delete selected note'}
            title="Delete — hold to select multiple notes to delete"
            disabled={multiSelectMode ? false : !selectedFolderCanDelete && !selectedNote && notes.length === 0}
            on:pointerdown={startDeleteButtonPress}
            on:pointerup={cancelDeleteButtonPress}
            on:pointerleave={cancelDeleteButtonPress}
            on:pointercancel={cancelDeleteButtonPress}
            on:click={handleDeleteButtonClick}
          >
            <Icon name="delete" size={18} />
          </button>
          <button class="icon-label-button" aria-label="New note" title="New note" on:click={() => createNote()}>
            <Icon name="new-note" size={18} />
          </button>
          <input
            bind:this={uploadInputElement}
            class="hidden-file-input"
            type="file"
            accept=".txt,.md,text/plain,text/markdown"
            multiple
            on:change={(event) => void handleUploadFiles(event.currentTarget.files)}
          />
        </div>
      </div>

      <div class="editor-mode-toggle" aria-label="Editor mode">
        <button
          class:active={editorRenderMode === 'text'}
          aria-pressed={editorRenderMode === 'text'}
          on:click={() => setEditorRenderMode('text')}
        >
          TXT
        </button>
        {#if !isMobilePlatform}
          <button
            class:active={editorRenderMode === 'markdown'}
            aria-pressed={editorRenderMode === 'markdown'}
            on:click={() => setEditorRenderMode('markdown')}
          >
            MD
          </button>
        {/if}
        <button
          class:active={editorRenderMode === 'rich'}
          aria-pressed={editorRenderMode === 'rich'}
          on:click={() => setEditorRenderMode('rich')}
        >
          RICH
        </button>
      </div>

      {#if searchOpen}
        <label class="sidebar-search">
          <Icon name="search" size={16} />
          <input aria-label="Search notes" placeholder="Search notes" bind:value={searchQuery} />
        </label>
      {/if}

      {#if notes.length === 0}
        <button class="empty-state icon-label-button" on:click={() => createNote()}>
          <Icon name="new-note" size={18} />
          <span>Create the first note</span>
        </button>
      {/if}

      <div class="file-tree" aria-label="Note folders">
        {#if rootNotes.length > 0 || draggedNoteId || draggedFolderPath}
          <div
            role="group"
            class="tree-group"
            data-folder-drop-target="/"
            on:dragover={(event) => allowFolderDrop(event, '/')}
            on:dragleave={() => dragTargetPath = ''}
            on:drop={(event) => dropNoteOnFolder(event, '/')}
          >
            <button
              class:active={activeFolderPath === '/'}
              class:drop-target={dragTargetPath === '/'}
              class="folder-row"
              data-folder-drop-target="/"
              on:click={(event) => handleFolderRowClick(event, '/')}
              on:dragover={(event) => allowFolderDrop(event, '/')}
              on:dragleave={() => dragTargetPath = ''}
              on:drop={(event) => dropNoteOnFolder(event, '/')}
            >
              <span class="folder-icon">/</span>
              <span>Root</span>
            </button>
            {#each rootNotes as note}
              <button
                class:selected={note.id === selectedNoteId}
                class:dragging={draggedNoteId === note.id}
                class:multi-selected={multiSelectedNoteIds.has(note.id)}
                class:download-selected={downloadSelectedNoteId === note.id}
                class="note-row file-row"
                data-folder-drop-target="/"
                draggable={!multiSelectMode}
                on:click={(event) => handleNoteRowClick(event, note)}
                on:dragstart={(event) => startNoteDrag(event, note)}
                on:dragend={endNoteDrag}
                on:pointerdown={(event) => startMobileNotePress(event, note)}
                on:pointermove={moveMobileTreePress}
                on:pointerup={endMobileTreePress}
                on:pointercancel={cancelMobileTreePress}
              >
                {#if multiSelectMode}
                  <span class="note-multiselect-check" aria-hidden="true">{multiSelectedNoteIds.has(note.id) ? '☑' : '☐'}</span>
                {/if}
                <strong>{note.title}</strong>
                <span
                  class:favorited={isFavoriteNote(note.id)}
                  class="note-favorite"
                  role="button"
                  tabindex="0"
                  aria-label={isFavoriteNote(note.id) ? `Remove ${note.title} from favorites` : `Add ${note.title} to favorites`}
                  title={isFavoriteNote(note.id) ? 'Remove favorite' : 'Add favorite'}
                  on:pointerdown={(event) => event.stopPropagation()}
                  on:click={(event) => toggleFavoriteNote(event, note.id)}
                  on:keydown={(event) => {
                    if (event.key === 'Enter' || event.key === ' ') toggleFavoriteNote(event, note.id)
                  }}
                >
                  {isFavoriteNote(note.id) ? '★' : '☆'}
                </span>
              </button>
            {/each}
          </div>
        {/if}

        {#each folderGroups as folder}
          <div
            role="group"
            class="tree-group"
            data-folder-drop-target={folder.path}
            on:dragover={(event) => allowFolderDrop(event, folder.path)}
            on:dragleave={() => dragTargetPath = ''}
            on:drop={(event) => dropNoteOnFolder(event, folder.path)}
          >
            <div
              role="button"
              tabindex="0"
              class:active={activeFolderPath === folder.path}
              class:selected-folder={selectedFolderPath === folder.path}
              class:drop-target={dragTargetPath === folder.path}
              class:dragging={draggedFolderPath === folder.path}
              class="folder-row"
              data-folder-drop-target={folder.path}
              draggable="true"
              on:click={(event) => handleFolderRowClick(event, folder.path)}
              on:keydown={(event) => handleFolderKey(event, folder.path)}
              on:dragstart={(event) => startFolderDrag(event, folder.path)}
              on:dragover={(event) => allowFolderDrop(event, folder.path)}
              on:dragleave={() => dragTargetPath = ''}
              on:drop={(event) => dropNoteOnFolder(event, folder.path)}
              on:dragend={endNoteDrag}
              on:pointerdown={(event) => startMobileFolderPress(event, folder.path)}
              on:pointermove={moveMobileTreePress}
              on:pointerup={endMobileTreePress}
              on:pointercancel={cancelMobileTreePress}
            >
              <span>{folder.path}</span>
              {#if folder.notes.length === 0}
                <span class="folder-empty">Empty</span>
              {/if}
              <span class="folder-spacer"></span>
              <button
                class="folder-minimize"
                aria-label={isFolderCollapsed(folder.path) ? 'Expand folder' : 'Minimize folder'}
                title={isFolderCollapsed(folder.path) ? 'Expand folder' : 'Minimize folder'}
                on:pointerdown={(event) => event.stopPropagation()}
                on:mousedown={(event) => event.stopPropagation()}
                on:dragstart={(event) => {
                  event.preventDefault()
                  event.stopPropagation()
                }}
                on:click={(event) => toggleFolderCollapsed(event, folder.path)}
              >
                {isFolderCollapsed(folder.path) ? '+' : '-'}
              </button>
            </div>
            {#if !isFolderCollapsed(folder.path)}
              {#each folder.notes as note}
                <button
                  class:selected={note.id === selectedNoteId}
                  class:dragging={draggedNoteId === note.id}
                  class:multi-selected={multiSelectedNoteIds.has(note.id)}
                class:download-selected={downloadSelectedNoteId === note.id}
                  class="note-row file-row nested"
                  data-folder-drop-target={folder.path}
                  draggable={!multiSelectMode}
                  on:click={(event) => handleNoteRowClick(event, note)}
                  on:dragstart={(event) => startNoteDrag(event, note)}
                  on:dragend={endNoteDrag}
                  on:pointerdown={(event) => startMobileNotePress(event, note)}
                  on:pointermove={moveMobileTreePress}
                  on:pointerup={endMobileTreePress}
                  on:pointercancel={cancelMobileTreePress}
                >
                  {#if multiSelectMode}
                    <span class="note-multiselect-check" aria-hidden="true">{multiSelectedNoteIds.has(note.id) ? '☑' : '☐'}</span>
                  {/if}
                  <strong>{note.title}</strong>
                  <span
                    class:favorited={isFavoriteNote(note.id)}
                    class="note-favorite"
                    role="button"
                    tabindex="0"
                    aria-label={isFavoriteNote(note.id) ? `Remove ${note.title} from favorites` : `Add ${note.title} to favorites`}
                    title={isFavoriteNote(note.id) ? 'Remove favorite' : 'Add favorite'}
                    on:pointerdown={(event) => event.stopPropagation()}
                    on:click={(event) => toggleFavoriteNote(event, note.id)}
                    on:keydown={(event) => {
                      if (event.key === 'Enter' || event.key === ' ') toggleFavoriteNote(event, note.id)
                    }}
                  >
                    {isFavoriteNote(note.id) ? '★' : '☆'}
                  </span>
                </button>
              {/each}
            {/if}
          </div>
        {/each}
      </div>

      {#if notes.length > 0 && filteredNotes.length === 0}
        <div class="empty-tree">No matching notes</div>
      {/if}

      {#if onBackupToServer}
        <button class="backup-menu-action icon-label-button" type="button" on:click={onBackupToServer}>
          <Icon name="upload" size={18} />
          <span>Back up to server</span>
        </button>
      {/if}
    </div>

    {#if multiSelectMode}
      <div class="multi-select-bar" role="toolbar" aria-label="Multi-select delete">
        <span>{multiSelectedNoteIds.size} selected</span>
        <div class="multi-select-bar-actions">
          <button type="button" on:click={exitMultiSelectMode} disabled={multiSelectDeleteBusy}>Cancel</button>
          <button
            type="button"
            class="danger-action"
            disabled={multiSelectedNoteIds.size === 0 || multiSelectDeleteBusy}
            on:click={() => void deleteMultiSelectedNotes()}
          >
            {multiSelectDeleteBusy ? 'Deleting…' : `Delete (${multiSelectedNoteIds.size})`}
          </button>
        </div>
      </div>
    {/if}
  </aside>

  <div
    class="sidebar-resizer"
    role="separator"
    aria-label="Resize folders panel"
    aria-orientation="vertical"
    aria-valuemin="220"
    aria-valuemax="520"
    aria-valuenow={sidebarWidth}
    on:pointerdown={startSidebarResize}
    on:pointermove={resizeSidebar}
    on:pointerup={stopSidebarResize}
    on:pointercancel={stopSidebarResize}
  ></div>

  <section class:editor-focused={editorFocused} class="editor-shell">
    <div class="metadata">
      <div class="note-title-panel">
        {#if selectedNote}
          <input aria-label="Title" bind:value={draftTitle} on:change={saveMetadata} />
          <button
            class={`save-indicator ${saveIndicatorState}`}
            type="button"
            aria-label={saveIndicatorLabel}
            title={saveIndicatorLabel}
            on:click={() => (statusDialogOpen = true)}
          >
            <Icon name="save" size={16} />
          </button>
        {:else}
          <div class="empty-note-title">Notes</div>
        {/if}
        <button
          class="mobile-files-toggle"
          aria-label="Open files"
          title="Files"
          on:click={() => mobileFilesOpen = true}
        >
          <span aria-hidden="true"></span>
        </button>
      </div>
    </div>

    <ActionBar ariaLabel="Editor toolbar" attached="top" wrap="wrap" className="toolbar" onMouseDown={handleToolbarMouseDown}>
      <button
        class:active-action={tocOpen}
        aria-label="Table of contents"
        title="Table of contents"
        on:click={() => tocOpen = !tocOpen}
      >
        <Icon name="list" size={18} />
      </button>
      <button aria-label="Undo" title="Undo" on:click={() => runCommand('undo')}><Icon name="undo" size={18} /></button>
      <button aria-label="Redo" title="Redo" on:click={() => runCommand('redo')}><Icon name="redo" size={18} /></button>
      <label class:active-action={richCommandActive('heading-1', richActiveStateVersion) || richCommandActive('heading-2', richActiveStateVersion) || richCommandActive('heading-3', richActiveStateVersion) || richCommandActive('heading-4', richActiveStateVersion) || richCommandActive('heading-5', richActiveStateVersion) || richCommandActive('heading-6', richActiveStateVersion)} class="icon-select-tool" title="Heading">
        <span class="toolbar-glyph">H</span>
        <span class="sr-only">Heading</span>
        <select aria-label="Heading" on:change={(event) => runCommand(event.currentTarget.value)}>
          <option value="paragraph">Paragraph</option>
          <option value="heading-1">Heading 1</option>
          <option value="heading-2">Heading 2</option>
          <option value="heading-3">Heading 3</option>
          <option value="heading-4">Heading 4</option>
          <option value="heading-5">Heading 5</option>
          <option value="heading-6">Heading 6</option>
        </select>
      </label>
      <button class:active-action={richCommandActive('bold', richActiveStateVersion)} aria-label="Bold" title="Bold" on:click={() => runCommand('bold')}><Icon name="bold" size={18} /></button>
      <button class:active-action={richCommandActive('italic', richActiveStateVersion)} aria-label="Italic" title="Italic" on:click={() => runCommand('italic')}><Icon name="italic" size={18} /></button>
      <button class:active-action={richCommandActive('underline', richActiveStateVersion)} aria-label="Underline" title="Underline" on:click={() => runCommand('underline')}><Icon name="underline" size={18} /></button>
      <button class:active-action={richCommandActive('indent', richActiveStateVersion)} aria-label="Indent" title="Indent" on:click={() => runCommand('indent')}><Icon name="indent" size={18} /></button>
      <button aria-label="Outdent" title="Outdent" on:click={() => runCommand('outdent')}><Icon name="outdent" size={18} /></button>
      <div class="list-menu-control">
        <button
          class:active-action={listMenuOpen || richCommandActive('list-dash', richActiveStateVersion) || richCommandActive('list-numbered', richActiveStateVersion)}
          aria-label="List style"
          title="List style"
          on:click={toggleListMenu}
        >
          <Icon name="list" size={18} />
        </button>
      </div>
      <button class:active-action={richCommandActive('strikethrough', richActiveStateVersion)} aria-label="Strikethrough" title="Strikethrough" on:click={() => runCommand('strikethrough')}><Icon name="strikethrough" size={18} /></button>
      <label title="Text color" class="color-tool">
        <span class="toolbar-glyph text-color-glyph">A</span>
        <input type="color" bind:value={textColor} on:input={() => applyColor('color', textColor)} />
      </label>
      <label title="Highlight color" class="color-tool">
        <span class="toolbar-glyph highlight-glyph">A</span>
        <input type="color" bind:value={highlightColor} on:input={() => applyColor('highlight', highlightColor)} />
      </label>
      <button aria-label="Divider" title="Divider" on:click={() => runCommand('divider')}><Icon name="divider" size={18} /></button>
      <button class:active-action={richCommandActive('quote', richActiveStateVersion)} aria-label="Quote" title="Quote" on:click={() => runCommand('quote')}><Icon name="quote" size={18} /></button>
      <button class:active-action={richCommandActive('code-block', richActiveStateVersion)} aria-label="Code block" title="Code block" on:click={() => runCommand('code-block')}><Icon name="code" size={18} /></button>
      <label class:active-action={richCommandActive('align-center', richActiveStateVersion) || richCommandActive('align-right', richActiveStateVersion)} class="icon-select-tool" title="Alignment">
        <Icon name="align-left" size={18} />
        <span class="sr-only">Alignment</span>
        <select aria-label="Alignment" on:change={(event) => runCommand(event.currentTarget.value)}>
          <option value="align-left">Left</option>
          <option value="align-center">Center</option>
          <option value="align-right">Right</option>
        </select>
      </label>
      <button class:active-action={richCommandActive('table', richActiveStateVersion)} aria-label="Insert table" title="Insert table" on:click={() => runCommand('table')}><Icon name="table" size={18} /></button>
      <button aria-label="Insert image" title="Insert image" on:click={() => runCommand('image')}><Icon name="image" size={18} /></button>
      <button class:active-action={richCommandActive('link', richActiveStateVersion)} aria-label="Insert link" title="Insert link" on:click={() => runCommand('link')}><Icon name="link" size={18} /></button>
      <label title="Font">
        <span class="sr-only">Font</span>
        <select class="plain-select" on:change={(event) => applyFont(event.currentTarget.value)}>
          <option value="">Font</option>
          {#each editorFontOptions as option}
            <option value={option.value}>{option.label}</option>
          {/each}
        </select>
      </label>
    </ActionBar>
    {#if tocOpen}
      <button class="toc-backdrop" aria-label="Close table of contents" on:click={() => tocOpen = false}></button>
      <div class="toc-popover" role="dialog" aria-label="Table of contents">
        <div class="toc-popover-heading">Table of contents</div>
        {#if tocHeadings.length > 0}
          <div class="toc-list">
            {#each tocHeadings as heading}
              <button
                class="toc-item"
                style={`--toc-indent: ${Math.max(0, heading.level - 1) * 16}px`}
                on:click={() => jumpToHeading(heading)}
              >
                <span>H{heading.level}</span>
                <strong>{heading.title}</strong>
              </button>
            {/each}
          </div>
        {:else}
          <p>No headings in this note.</p>
        {/if}
      </div>
    {/if}
    {#if listMenuOpen}
      <div class="list-style-menu" role="menu" tabindex="-1" aria-label="List style" style={listMenuStyle} on:mousedown={(event) => event.preventDefault()}>
        <button role="menuitem" on:click={() => runCommand('list-dash')}><span>•</span><strong>Dash list</strong></button>
        <button role="menuitem" on:click={() => runCommand('list-star')}><span>★</span><strong>Star list</strong></button>
        <button role="menuitem" on:click={() => runCommand('list-checkbox')}><span>☐</span><strong>Checkbox list</strong></button>
        <button role="menuitem" on:click={() => runCommand('list-numbered')}><span>1.</span><strong>Numbered list</strong></button>
        <button role="menuitem" on:click={() => runCommand('list-emoji')}><span>😀</span><strong>Emoji list</strong></button>
      </div>
    {/if}

    {#if selectedNote}
      <div class:markdown-mode={editorRenderMode === 'markdown'} class="editor-workspace">
        {#if editorRenderMode === 'rich'}
          <div
            bind:this={richEditorElement}
            use:richTableHost
            class="rich-editor-host"
          >
            {#if tableMenuOpen && richTableMenuStyle}
              <div
                class="rich-table-menu"
                role="menu"
                tabindex="-1"
                style={richTableMenuStyle}
                on:mousedown={(event) => event.preventDefault()}
              >
                <button role="menuitem" aria-label="Add table row" title="Add row" on:click={() => runCommand('table-add-row')}>+R</button>
                <button role="menuitem" aria-label="Remove table row" title="Remove row" on:click={() => runCommand('table-remove-row')}>-R</button>
                <button role="menuitem" aria-label="Add table column" title="Add column" on:click={() => runCommand('table-add-column')}>+C</button>
                <button role="menuitem" aria-label="Remove table column" title="Remove column" on:click={() => runCommand('table-remove-column')}>-C</button>
              </div>
            {/if}
          </div>
        {:else}
          <textarea
            bind:this={editorElement}
            bind:value={editorText}
            aria-label={editorRenderMode === 'markdown' ? 'Markdown source' : 'Note text'}
            on:input={handleEditorInput}
            on:select={captureTextSelection}
            on:keyup={captureTextSelection}
            on:pointerdown={markEditorInteraction}
            on:pointerup={captureTextSelection}
            on:focus={() => (editorFocused = true)}
            on:blur={() => void handleEditorBlur()}
            spellcheck="true"
          ></textarea>
        {/if}
        {#if editorRenderMode === 'markdown'}
          <article class="markdown-preview" aria-label="Markdown preview">
            {@html renderedMarkdown}
          </article>
        {/if}
      </div>
    {:else}
      <div class="empty-editor">No note selected</div>
    {/if}

    <footer>
      <span>{status}</span>
      <span>{peers.length} collaborator{peers.length === 1 ? '' : 's'}</span>
    </footer>
  </section>

  {#if settingsOpen}
    <AppearanceSettings {tokens} {services} onTokensChange={updateTokens} onClose={() => settingsOpen = false} />
  {/if}

  {#if statusDialogOpen}
    <div class="note-status-overlay">
      <button class="note-status-backdrop" type="button" aria-label="Close note status" on:click={() => (statusDialogOpen = false)}></button>
      <section class="note-status-dialog" aria-label="Note sync status">
        <header>
          <div>
            <p class="eyebrow">Note status</p>
            <h2>{selectedNote?.title ?? 'No note selected'}</h2>
          </div>
          <button class="notes-menu-close" aria-label="Close note status" title="Close" on:click={() => (statusDialogOpen = false)}>
            <span aria-hidden="true"></span>
          </button>
        </header>
        <div class="note-status-grid">
          <div>
            <span>Storage</span>
            <strong>{isLocalRuntime ? 'Local only' : mode === 'suite' ? 'Primary server only' : 'Server backed'}</strong>
          </div>
          <div>
            <span>Active server</span>
            <strong>{activeServerLabel}</strong>
          </div>
          <div>
            <span>Queued changes</span>
            <strong>{queuedOperationCount}</strong>
          </div>
          <div>
            <span>Collaborators</span>
            <strong>{peers.length}</strong>
          </div>
          <div>
            <span>Document</span>
            <strong>{selectedNote?.documentId ?? 'None'}</strong>
          </div>
          <div>
            <span>Client</span>
            <strong>{services.clientId}</strong>
          </div>
          <div>
            <span>Last attempt</span>
            <strong>{lastSyncAttemptAt || 'None'}</strong>
          </div>
          <div>
            <span>Last push</span>
            <strong>{lastSyncPushAt || 'None'}</strong>
          </div>
          <div>
            <span>Last pull</span>
            <strong>{lastSyncPullAt || 'None'}</strong>
          </div>
          <div>
            <span>Remote update</span>
            <strong>{lastRemoteUpdateAt || 'None'}</strong>
          </div>
          <div>
            <span>Document refresh</span>
            <strong>{lastDocumentRefreshAt || 'None'}</strong>
          </div>
          <div>
            <span>Queued docs</span>
            <strong>{queuedDocumentIds.length}</strong>
          </div>
        </div>
        {#if lastSyncError}
          <div class="note-status-section">
            <strong>Last sync error</strong>
            <p>{lastSyncError}</p>
          </div>
        {/if}
        {#if !isLocalRuntime}
          <div class="note-status-section">
            <div class="note-status-section-title">
              <strong>Version history</strong>
              <button type="button" disabled={versionsLoading} on:click={() => void loadDocumentVersions()}>
                {versionsLoading ? 'Loading' : 'Refresh'}
              </button>
            </div>
            {#if versionsError}
              <p>{versionsError}</p>
            {:else if documentVersions.length === 0}
              <p>{versionsLoading ? 'Loading versions…' : 'No saved versions yet — one is checkpointed automatically a few minutes after you start editing.'}</p>
            {:else}
              <ul class="note-status-version-list">
                {#each documentVersions as version (version.id)}
                  <li>
                    <span>{new Date(version.createdAt).toLocaleString()}</span>
                    <span class="note-status-version-reason">{version.reason}</span>
                    <button
                      type="button"
                      disabled={Boolean(restoringVersionId)}
                      on:click={() => void restoreDocumentVersion(version.id)}
                    >
                      {restoringVersionId === version.id ? 'Restoring…' : 'Restore'}
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}
          </div>
        {/if}
        <div class="note-status-section">
          <div class="note-status-section-title">
            <strong>Manual sync</strong>
            <span>{manualSyncBusy ? `${manualSyncBusy} running` : 'Ready'}</span>
          </div>
          <div class="note-status-actions">
            <button type="button" disabled={isLocalRuntime || Boolean(manualSyncBusy)} on:click={manualPushToServer}>
              {manualSyncBusy === 'push' ? 'Pushing' : 'Push'}
            </button>
            <button type="button" disabled={isLocalRuntime || Boolean(manualSyncBusy)} on:click={manualPullFromServer}>
              {manualSyncBusy === 'pull' ? 'Pulling' : 'Pull'}
            </button>
          </div>
        </div>
        <div class="note-status-section">
          <div class="note-status-section-title">
            <strong>{mode === 'suite' ? 'Primary server' : 'Backup targets'}</strong>
            {#if mode !== 'suite'}
              <button type="button" on:click={() => onOpenServerManager?.()}>Manage</button>
            {/if}
          </div>
          {#if mode === 'suite'}
            <p>This browser session reflects the primary server only. Local-device note storage is disabled in Suite mode.</p>
            <div class="note-status-server-row active">
              <span>{activeServerLabel}</span>
              <div>
                <strong>Active</strong>
              </div>
            </div>
          {:else if connectedServers.length === 0}
            <p>No servers connected. This note is stored on this device until you connect a server.</p>
          {:else}
            {#each connectedServers as server}
              <div class:active={server.active} class="note-status-server-row">
                <span>{server.url}</span>
                <div>
                  <strong>{server.active ? 'Active' : 'Available'}</strong>
                  <button
                    type="button"
                    disabled={!selectedNote || backingUpServerId === server.id}
                    on:click={() => backupSelectedNoteToServer(server)}
                  >
                    {backingUpServerId === server.id ? 'Backing up' : 'Back up'}
                  </button>
                </div>
              </div>
            {/each}
          {/if}
        </div>
        <div class="note-status-section">
          <div class="note-status-section-title">
            <strong>Recent activity</strong>
            <button type="button" on:click={copySyncLog}>{logCopyStatus || 'Copy log'}</button>
          </div>
          <textarea class="note-sync-log-export" readonly value={syncLogText}></textarea>
          <div class="note-sync-log">
            {#each syncLog as item}
              <div>
                <span>{item.at}</span>
                <p>{item.message}</p>
              </div>
            {/each}
          </div>
        </div>
      </section>
    </div>
  {/if}
</main>
