<script lang="ts">
  import type { AuthSession, CurrentSession } from '@og-suite/contracts'
  import { createHttpApiClient } from '@og-suite/runtime'
  import type { RuntimeServices } from '@og-suite/runtime'
  import NotesApp from './NotesApp.svelte'
  import { createLocalOnlyRuntime, createStandaloneRuntime } from './runtime'

  const localModeKey = 'og-suite:notes:local-only'
  const connectedServersKey = 'og-suite:notes:connected-servers'
  // Local-only mode ("Continue without signing in") is normally desktop/
  // mobile (Tauri) only — the go-server web UI build wants to require a
  // real sign-in. The GitHub Pages demo build opts in via
  // VITE_OG_NOTES_DEMO_MODE (see vite.config.ts): the underlying runtime
  // already works in a plain browser, this is purely a UI gate.
  const canUseLocalOnly =
    (typeof window !== 'undefined' && ('__TAURI_INTERNALS__' in window || '__TAURI__' in window))
    || import.meta.env.VITE_OG_NOTES_DEMO_MODE === true

  const defaultServerUrl = (() => {
    if (typeof window === 'undefined') return 'http://127.0.0.1:8080'
    // Tauri's WebView serves the app itself from tauri://localhost /
    // http://tauri.localhost — that's the app's own origin, never a real
    // note server, so falling back to it (as window.location.origin would)
    // pre-fills the sign-in form with a URL that can never work. Desktop
    // dev server (vite on localhost:5174) is the one case where the app's
    // own origin doubles as a real backend during local development.
    if (canUseLocalOnly) return ''
    if (window.location.protocol === 'file:') return 'http://127.0.0.1:8080'
    if (window.location.hostname === 'localhost') return 'http://127.0.0.1:8080'
    return window.location.origin
  })()

  type ConnectedServer = {
    id: string
    url: string
    username: string
    displayName: string
    workspaceName: string
    accessToken: string
    refreshToken: string
    expiresAt: string
    connectedAt: string
    active: boolean
  }

  let serverUrl = localStorage.getItem('og-suite:server-url') ?? defaultServerUrl
  let username = ''
  let password = ''
  let error = ''
  let backupStatus = ''
  let checking = true
  let session: CurrentSession | null = null
  let localOnly = canUseLocalOnly && localStorage.getItem(localModeKey) === 'true'
  let backupDialogOpen = false
  let serversDialogOpen = false
  let runtimeKey = 0
  let connectedServers: ConnectedServer[] = loadConnectedServers()
  let services: RuntimeServices = localOnly ? createLocalOnlyRuntime() : createStandaloneRuntime(serverUrl)

  if (!canUseLocalOnly) localStorage.removeItem(localModeKey)

  $: activeServer = connectedServers.find((server) => server.active) ?? null

  void loadSession()

  function createId(prefix: string) {
    if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID()
    return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
  }

  function loadConnectedServers(): ConnectedServer[] {
    try {
      const parsed = JSON.parse(localStorage.getItem(connectedServersKey) ?? '[]') as ConnectedServer[]
      if (Array.isArray(parsed)) return parsed.filter((server) => server.url && server.accessToken)
    } catch {
      // Ignore invalid legacy local storage.
    }
    return []
  }

  function saveConnectedServers(nextServers = connectedServers) {
    localStorage.setItem(connectedServersKey, JSON.stringify(nextServers))
  }

  function applyActiveServer(server: ConnectedServer) {
    localStorage.setItem('og-suite:server-url', server.url)
    localStorage.setItem('og-suite:auth:access-token', server.accessToken)
    localStorage.setItem('og-suite:auth:refresh-token', server.refreshToken)
    localStorage.setItem('og-suite:auth:expires-at', server.expiresAt)
  }

  function clearActiveServerTokens() {
    localStorage.removeItem('og-suite:auth:access-token')
    localStorage.removeItem('og-suite:auth:refresh-token')
    localStorage.removeItem('og-suite:auth:expires-at')
  }

  function withTimeout<T>(promise: Promise<T>, milliseconds: number): Promise<T> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Session check timed out.')), milliseconds)
      promise.then(
        (value) => {
          clearTimeout(timer)
          resolve(value)
        },
        (timeoutError) => {
          clearTimeout(timer)
          reject(timeoutError)
        },
      )
    })
  }

  function createLocalSession(): CurrentSession {
    return {
      user: {
        id: 'local-user',
        displayName: 'Local Notes',
        username: 'local',
        roles: ['owner'],
        mustChangePassword: false,
      },
      workspace: {
        id: 'local-workspace',
        name: 'Local Notes',
      },
      expiresAt: '9999-12-31T23:59:59.999Z',
    }
  }

  async function loadSession() {
    checking = true
    if (localOnly) {
      services = createLocalOnlyRuntime()
      session = createLocalSession()
      checking = false
      return
    }
    const storedActiveServer = connectedServers.find((server) => server.active)
    if (storedActiveServer) {
      serverUrl = storedActiveServer.url
      applyActiveServer(storedActiveServer)
    }
    if (!localStorage.getItem('og-suite:auth:access-token')) {
      session = null
      checking = false
      return
    }
    services = createStandaloneRuntime(serverUrl)
    try {
      session = await withTimeout(services.api.get<CurrentSession>('/api/v1/auth/session'), 2500)
    } catch {
      session = null
    } finally {
      checking = false
    }
  }

  function continueLocally() {
    if (!canUseLocalOnly) return
    error = ''
    backupStatus = ''
    localOnly = true
    backupDialogOpen = false
    serversDialogOpen = false
    localStorage.setItem(localModeKey, 'true')
    services = createLocalOnlyRuntime()
    session = createLocalSession()
    runtimeKey += 1
  }

  function openBackupDialog() {
    error = ''
    backupStatus = ''
    backupDialogOpen = true
  }

  function openServersDialog() {
    error = ''
    backupStatus = ''
    backupDialogOpen = false
    serversDialogOpen = true
  }

  async function signIn() {
    error = ''
    backupStatus = ''
    const normalizedServerUrl = serverUrl.trim().replace(/\/$/, '')
    if (!normalizedServerUrl || !username.trim() || !password) {
      error = 'Server, username, and password are required.'
      return
    }
    try {
      const api = createHttpApiClient(normalizedServerUrl)
      const authSession = await api.post<AuthSession>('/api/v1/auth/login', {
        username: username.trim(),
        password,
      })
      if (authSession.user.mustChangePassword) {
        error = 'This account must finish first setup in OG Suite before mobile sign-in.'
        return
      }
      const signedInUsername = authSession.user.username ?? username.trim()
      const nextServer: ConnectedServer = {
        id: connectedServers.find((server) => server.url === normalizedServerUrl && server.username === signedInUsername)?.id ?? createId('server'),
        url: normalizedServerUrl,
        username: signedInUsername,
        displayName: authSession.user.displayName,
        workspaceName: authSession.workspace.name,
        accessToken: authSession.accessToken,
        refreshToken: authSession.refreshToken,
        expiresAt: authSession.expiresAt,
        connectedAt: new Date().toISOString(),
        active: true,
      }
      connectedServers = [
        nextServer,
        ...connectedServers
          .filter((server) => server.id !== nextServer.id)
          .map((server) => ({ ...server, active: false })),
      ]
      saveConnectedServers()
      localStorage.setItem('og-suite:server-url', normalizedServerUrl)
      localStorage.setItem('og-suite:auth:access-token', authSession.accessToken)
      localStorage.setItem('og-suite:auth:refresh-token', authSession.refreshToken)
      localStorage.setItem('og-suite:auth:expires-at', authSession.expiresAt)
      localStorage.removeItem(localModeKey)
      serverUrl = normalizedServerUrl
      password = ''
      localOnly = false
      backupDialogOpen = false
      serversDialogOpen = false
      services = createStandaloneRuntime(normalizedServerUrl)
      session = {
        user: authSession.user,
        workspace: authSession.workspace,
        expiresAt: authSession.expiresAt,
      }
      runtimeKey += 1
      backupStatus = 'Signed in. Local notes queued on this device will back up to the server.'
      setTimeout(() => {
        backupStatus = ''
      }, 6000)
    } catch (requestError) {
      error = requestError instanceof Error ? requestError.message : 'Sign in failed.'
    }
  }

  function switchServer(serverId: string) {
    const nextActiveServer = connectedServers.find((server) => server.id === serverId)
    if (!nextActiveServer) return
    connectedServers = connectedServers.map((server) => ({ ...server, active: server.id === serverId }))
    saveConnectedServers()
    applyActiveServer(nextActiveServer)
    serverUrl = nextActiveServer.url
    localOnly = false
    localStorage.removeItem(localModeKey)
    services = createStandaloneRuntime(nextActiveServer.url)
    session = {
      user: {
        id: nextActiveServer.username,
        displayName: nextActiveServer.displayName,
        username: nextActiveServer.username,
        roles: ['owner'],
        mustChangePassword: false,
      },
      workspace: {
        id: nextActiveServer.workspaceName,
        name: nextActiveServer.workspaceName,
      },
      expiresAt: nextActiveServer.expiresAt,
    }
    runtimeKey += 1
    backupStatus = `Connected to ${nextActiveServer.url}`
  }

  function disconnectServer(serverId: string) {
    const wasActive = connectedServers.find((server) => server.id === serverId)?.active
    connectedServers = connectedServers.filter((server) => server.id !== serverId)
    saveConnectedServers()
    if (!wasActive) return
    clearActiveServerTokens()
    continueLocally()
    serversDialogOpen = true
  }
</script>

{#if checking}
  <main class="standalone-auth-screen">
    <section class="standalone-auth-card">
      <p>Checking session...</p>
    </section>
  </main>
{:else if !session}
  <main class="standalone-auth-screen">
    <form class="standalone-auth-card" on:submit|preventDefault={signIn}>
      <div>
        <p class="eyebrow">OG Notes</p>
        <h1>Sign in to a server</h1>
      </div>
      <label>
        <span>Server URL</span>
        <input bind:value={serverUrl} autocomplete="url" />
      </label>
      <label>
        <span>Username</span>
        <input bind:value={username} autocomplete="username" />
      </label>
      <label>
        <span>Password</span>
        <input bind:value={password} type="password" autocomplete="current-password" />
      </label>
      {#if error}
        <p class="standalone-auth-error">{error}</p>
      {/if}
      <button type="submit">Sign in</button>
      {#if canUseLocalOnly}
        <button class="standalone-auth-secondary" type="button" on:click={continueLocally}>
          Continue without signing in
        </button>
      {/if}
      {#if import.meta.env.VITE_OG_NOTES_DEMO_MODE === true}
        <p class="standalone-auth-demo-note">
          This is a live demo — "Continue without signing in" stores notes only in this
          browser (no account, no server). Clearing site data erases them.
        </p>
      {/if}
    </form>
  </main>
{:else}
  <div class="standalone-notes-shell">
    {#if !localOnly && backupStatus}
      <div class="local-mode-banner synced">
        <div>
          <strong>Server backup active</strong>
          <span>{backupStatus}</span>
        </div>
      </div>
    {/if}

    {#key runtimeKey}
      <NotesApp
        {services}
        mode="standalone"
        connectedServers={connectedServers}
        activeServerUrl={activeServer?.url ?? ''}
        onBackupToServer={localOnly ? openBackupDialog : undefined}
        onOpenServerManager={openServersDialog}
      />
    {/key}
  </div>

  {#if backupDialogOpen || serversDialogOpen}
    <div class="local-backup-overlay">
      <button
        class="local-backup-backdrop"
        type="button"
        aria-label="Close server dialog"
        on:click={() => {
          backupDialogOpen = false
          serversDialogOpen = false
        }}
      ></button>
      <form class="standalone-auth-card local-backup-dialog" on:submit|preventDefault={signIn}>
        <div>
          <p class="eyebrow">{serversDialogOpen ? 'Connected servers' : 'Server backup'}</p>
          <h1>{serversDialogOpen ? 'Manage note servers' : 'Sign in to back up notes'}</h1>
          <p class="standalone-auth-help">
            {serversDialogOpen
              ? 'Choose the active server for syncing this device, add another homelab server, or disconnect a saved server.'
              : 'Your local notes stay on this device until you sign in. After sign-in, queued local changes upload to that server and are visible in the signed-in workspace.'}
          </p>
        </div>
        {#if serversDialogOpen}
          <div class="connected-server-list">
            {#if connectedServers.length === 0}
              <p>No connected servers yet.</p>
            {:else}
              {#each connectedServers as server}
                <div class:active={server.active} class="connected-server-row">
                  <div>
                    <strong>{server.url}</strong>
                    <span>{server.displayName} · {server.workspaceName}</span>
                  </div>
                  <button class="standalone-auth-secondary" type="button" on:click={() => switchServer(server.id)} disabled={server.active}>
                    {server.active ? 'Active' : 'Use'}
                  </button>
                  <button class="standalone-auth-secondary danger" type="button" on:click={() => disconnectServer(server.id)}>
                    Disconnect
                  </button>
                </div>
              {/each}
            {/if}
          </div>
        {/if}
        <label>
          <span>Server URL</span>
          <input bind:value={serverUrl} autocomplete="url" />
        </label>
        <label>
          <span>Username</span>
          <input bind:value={username} autocomplete="username" />
        </label>
        <label>
          <span>Password</span>
          <input bind:value={password} type="password" autocomplete="current-password" />
        </label>
        {#if error}
          <p class="standalone-auth-error">{error}</p>
        {/if}
        <div class="local-backup-actions">
          <button
            class="standalone-auth-secondary"
            type="button"
            on:click={() => {
              backupDialogOpen = false
              serversDialogOpen = false
            }}
          >
            Cancel
          </button>
          <button type="submit">{serversDialogOpen ? 'Connect server' : 'Back up to server'}</button>
        </div>
      </form>
    </div>
  {/if}
{/if}
