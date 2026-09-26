export type IsoDateTime = string

export type AppCapability = 'offline' | 'remoteSave' | 'collaboration' | 'files' | 'media'

export type UserProfile = {
  id: string
  displayName: string
  username?: string
  roles: string[]
  mustChangePassword: boolean
}

export type WorkspaceProfile = {
  id: string
  name: string
}

export type AuthSession = {
  user: UserProfile
  workspace: WorkspaceProfile
  accessToken: string
  refreshToken: string
  expiresAt: IsoDateTime
}

export type CurrentSession = {
  user: UserProfile
  workspace: WorkspaceProfile
  expiresAt: IsoDateTime
}

export type RegisterProfileRequest = {
  username: string
  displayName: string
  password: string
}

export type LoginRequest = {
  username: string
  password: string
}

export type RefreshSessionRequest = {
  refreshToken: string
}

export type CompleteSetupRequest = {
  username: string
  displayName: string
  password: string
  confirmPassword: string
}

export type AppToolScope = {
  feed: boolean
  notes: boolean
  files: boolean
  audio: boolean
  admin: boolean
}

export type AdminRolePolicy = {
  name: string
  appScopes: AppToolScope
  adminPanel: boolean
  manageUsers: boolean
  manageStorage: boolean
  manageAuth: boolean
  manageDeployment: boolean
  manageDatabase: boolean
  viewAudits: boolean
}

export type CreateAdminRoleRequest = AdminRolePolicy

export type AdminUserSummary = {
  id: string
  username: string
  displayName: string
  roles: string[]
  mustChangePassword: boolean
  storageUsedBytes: number
  storageLimitMb: number
  appScopes: AppToolScope
  createdAt: IsoDateTime
  updatedAt: IsoDateTime
}

export type CreateAdminUserRequest = {
  username: string
  displayName: string
  password: string
  roles: string[]
  storageLimitMb: number
  appScopes: AppToolScope
}

export type UpdateAdminUserAccessRequest = {
  roles: string[]
  storageLimitMb: number
  appScopes: AppToolScope
}

export type ResetAdminUserPasswordRequest = {
  password: string
  confirmPassword: string
}

export type AdminStorageOverview = {
  totalUsedBytes: number
  totalLimitMb: number
  userCount: number
  notesBytes: number
  audioBytes: number
  filesBytes: number
}

export type AdminDatabaseTable = {
  key: string
  label: string
  rowCount: number
  columns: string[]
  rows: Record<string, unknown>[]
}

export type AdminDatabaseOverview = {
  backend: string
  generatedAt: IsoDateTime
  tables: AdminDatabaseTable[]
}

export type AdminAuditEntry = {
  id: string
  occurredAt: IsoDateTime
  actorId: string
  actorLabel: string
  action: string
  targetKind: string
  targetLabel: string
  details?: Record<string, unknown>
}

export type AdminAuthSettings = {
  defaultAdminEnabled: boolean
  localPasswordEnabled: boolean
  requireSetupPasswordChange: boolean
}

export type AdminDeploymentSettings = {
  serverVersion: string
  buildDate: string
  apiCompatibilityVersion: string
  releaseChannel: string
}

export type AdminSummary = {
  generatedAt: IsoDateTime
  users: AdminUserSummary[]
  rolePolicies: AdminRolePolicy[]
  storage: AdminStorageOverview
  authentication: AdminAuthSettings
  deployment: AdminDeploymentSettings
  database: AdminDatabaseOverview
  audits: AdminAuditEntry[]
}

export type SystemVersion = {
  backendVersion: string
  apiCompatibilityVersion: string
  minimumClientVersion: string
  buildDate: string
  capabilities: string[]
  authRequired: boolean
  authModes: string[]
}

export type AppRegistryEntry = {
  id: string
  name: string
  route: string
  standaloneRoute: string
  capabilities: AppCapability[]
}

export type FeedActivityAction =
  | 'note_created'
  | 'note_edited'
  | 'note_metadata_updated'
  | 'note_deleted'
  | 'folder_created'
  | 'folder_deleted'
  | 'document_edited'
  | 'favorite_added'
  | 'favorite_removed'
  | 'audio_recording_created'
  | 'audio_recording_renamed'
  | 'audio_uploaded'
  | 'audio_transcript_queued'
  | 'audio_transcript_generated'

export type FeedActivityEvent = {
  id: string
  appId: string
  action: FeedActivityAction
  summary: string
  targetKind: 'note' | 'folder' | 'document' | 'tool' | 'settings' | 'recording'
  targetId: string
  targetLabel: string
  actorId: string
  actorName: string
  workspaceId: string
  isPublic: boolean
  createdAt: IsoDateTime
}

export type FeedFavorite = {
  id: string
  targetKind: 'note' | 'folder' | 'document' | 'tool' | 'recording'
  targetId: string
  label: string
  appId: string
  actorId: string
  workspaceId: string
  createdAt: IsoDateTime
}

export type CreateFeedFavoriteRequest = {
  targetKind: FeedFavorite['targetKind']
  targetId: string
  label: string
  appId: string
}

export type AudioRecordingStatus = 'local' | 'uploading' | 'uploaded' | 'transcribing' | 'transcribed' | 'failed'

export type AudioRecording = {
  id: string
  title: string
  path: string
  mimeType: string
  durationMs: number
  sizeBytes: number
  status: AudioRecordingStatus
  assetRef?: string | null
  ownerId: string
  workspaceId: string
  createdAt: IsoDateTime
  updatedAt: IsoDateTime
  deletedAt?: IsoDateTime | null
}

export type AudioFolder = {
  id: string
  path: string
  name: string
  ownerId: string
  workspaceId: string
  createdAt: IsoDateTime
  updatedAt: IsoDateTime
  deletedAt?: IsoDateTime | null
}

export type AudioTranscriptSegment = {
  id: string
  recordingId: string
  channel?: number | null
  speakerLabel?: string | null
  startMs: number
  endMs: number
  text: string
}

export type AudioTranscript = {
  recordingId: string
  status: 'queued' | 'processing' | 'ready' | 'failed'
  segments: AudioTranscriptSegment[]
  updatedAt: IsoDateTime
}

export type AudioTranscriptionStatus = {
  recordingId: string
  status: AudioTranscript['status']
  engine: 'disabled' | 'command' | 'whisper_cpp' | string
  updatedAt: IsoDateTime
}

export type CreateAudioRecordingRequest = {
  title: string
  path?: string
  mimeType: string
  durationMs: number
  sizeBytes: number
}

export type UploadAudioRequest = {
  dataUrl: string
  mimeType: string
  sizeBytes: number
}

export type UpdateAudioRecordingRequest = {
  title?: string
  path?: string
}

export type CreateAudioFolderRequest = {
  path: string
}

export type DriveFile = {
  id: string
  name: string
  path: string
  mimeType: string
  sizeBytes: number
  ownerId: string
  workspaceId: string
  createdAt: IsoDateTime
  updatedAt: IsoDateTime
  deletedAt?: IsoDateTime | null
}

export type DriveFolder = {
  id: string
  path: string
  name: string
  ownerId: string
  workspaceId: string
  createdAt: IsoDateTime
  updatedAt: IsoDateTime
  deletedAt?: IsoDateTime | null
}

export type CreateDriveFileRequest = {
  name: string
  path?: string
  mimeType: string
  sizeBytes: number
  dataUrl: string
}

export type UpdateDriveFileRequest = {
  name?: string
  path?: string
}

export type CreateDriveFolderRequest = {
  path: string
}

export type UpdateDriveFolderRequest = {
  path?: string
}

export type DesignTokens = {
  colorBackground: string
  colorBackgroundGradient: string
  backgroundGradients: BackgroundGradient[]
  backgroundImage: string
  backgroundImageOpacity: number
  backgroundTexture: string
  panelTexture: string
  navTexture: string
  panelOpacity: number
  colorSection: string
  colorPanel: string
  colorSurface: string
  colorSurfaceSubtle: string
  colorSurfaceStrong: string
  colorToolBackground: string
  colorActionBarBackground: string
  colorText: string
  colorMuted: string
  colorTextInverse: string
  colorAccent: string
  colorAccentSoft: string
  colorAccentBorder: string
  colorAccentContrast: string
  colorDanger: string
  colorDangerSoft: string
  colorDangerBorder: string
  colorWarning: string
  colorSuccess: string
  colorOverlay: string
  colorBorder: string
  colorNav: string
  shadow: string
  margin: number
  innerMargin: number
  radius: number
  density: 'compact' | 'comfortable'
  fontFamily: string
  confirmDelete: boolean
}

export type CustomFont = {
  id: string
  name: string
  family: string
  dataUrl: string
  format: string
  createdAt: IsoDateTime
}

export type AppearanceTheme = {
  id: string
  name: string
  tokens: DesignTokens
  ownerId: string
  workspaceId: string
  isShared: boolean
  createdAt: IsoDateTime
  updatedAt: IsoDateTime
}

export type CreateAppearanceThemeRequest = {
  name: string
  tokens: DesignTokens
  isShared: boolean
}

export type UpdateAppearanceThemeRequest = {
  name?: string
  tokens?: DesignTokens
  isShared?: boolean
}

export type AppearanceSettings = {
  userId: string
  workspaceId: string
  tokens: DesignTokens
  updatedAt: IsoDateTime
}

export type UpdateAppearanceSettingsRequest = {
  tokens: DesignTokens
}

export type BackgroundGradient = {
  id: string
  name: string
  strength: number
  points: BackgroundGradientPoint[]
}

export type BackgroundGradientPoint = {
  id: string
  color: string
  strength: number
  x: number
  y: number
  stop: number
}

export type Note = {
  id: string
  documentId: string
  title: string
  path: string
  tags: string[]
  ownerId: string
  workspaceId: string
  createdAt: IsoDateTime
  updatedAt: IsoDateTime
  deletedAt?: IsoDateTime | null
}

export type NoteFolder = {
  id: string
  path: string
  name: string
  ownerId: string
  workspaceId: string
  createdAt: IsoDateTime
  updatedAt: IsoDateTime
  deletedAt?: IsoDateTime | null
}

export type CrdtDocumentState = {
  id: string
  kind: 'note'
  snapshot: string
  updates: CrdtUpdate[]
  version: number
  compactedAt?: IsoDateTime | null
}

export type DocumentVersionSummary = {
  id: string
  documentId: string
  createdAt: IsoDateTime
  createdBy: string
  reason: string
}

export type DocumentVersion = DocumentVersionSummary & {
  state: CrdtDocumentState
}

export type CrdtUpdate = {
  id: string
  documentId: string
  clientId: string
  sequence: number
  payload: string
  createdAt: IsoDateTime
  clientSchemaVersion?: number
}

export type PresencePeer = {
  clientId: string
  userId: string
  displayName: string
  cursor?: number | null
  color: string
  lastSeenAt: IsoDateTime
}

export type SyncCursorSet = {
  generatedAt: IsoDateTime
}

export type SyncTombstone = {
  entity: 'notes' | 'documents' | 'noteFolders' | 'audioRecordings'
  id: string
  deletedAt: IsoDateTime
}

export type SyncConflict = {
  entity: 'notes' | 'documents'
  id: string
  reason: string
}

export type SyncOperation =
  | { kind: 'create_note'; note: Note; document: CrdtDocumentState }
  | { kind: 'update_note_metadata'; note: Note }
  | { kind: 'delete_note'; id: string; deletedAt: IsoDateTime }
  | { kind: 'create_note_folder'; folder: NoteFolder }
  | { kind: 'delete_note_folder'; id: string; deletedAt: IsoDateTime }
  | { kind: 'append_document_update'; update: CrdtUpdate }

export type SyncEnvelope = {
  cursors: SyncCursorSet
  apps: AppRegistryEntry[]
  noteFolders: NoteFolder[]
  notes: Note[]
  documents: CrdtDocumentState[]
  documentUpdates: CrdtUpdate[]
  tombstones: SyncTombstone[]
  conflicts: SyncConflict[]
}

export type SyncBootstrapResponse = SyncEnvelope
export type SyncPullRequest = { cursors: SyncCursorSet }
export type SyncPullResponse = SyncEnvelope
export type SyncPushRequest = { operations: SyncOperation[] }
export type SyncPushResponse = SyncEnvelope & { acceptedOperationIds: string[] }

export type CreateNoteRequest = {
  title: string
  path?: string
  tags?: string[]
  initialText?: string
}

export type UpdateNoteMetadataRequest = {
  title?: string
  path?: string
  tags?: string[]
}

export type AppendDocumentUpdatesRequest = {
  updates: Omit<CrdtUpdate, 'id' | 'createdAt'>[]
}
