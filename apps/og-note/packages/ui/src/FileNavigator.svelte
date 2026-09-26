<script lang="ts" context="module">
  export type FileNavigatorItem = {
    id: string
    title: string
    path: string
    meta?: string
  }

  export type FileNavigatorFolder = {
    id: string
    path: string
    name: string
  }
</script>

<script lang="ts">
  import Icon from './Icon.svelte'

  export let folders: FileNavigatorFolder[] = []
  export let items: FileNavigatorItem[] = []
  export let selectedItemId = ''
  export let activeFolderPath = '/'
  export let selectedFolderPath = ''
  export let collapsedFolderPaths: string[] = []
  export let itemLabel = 'file'
  export let onSelectItem: (id: string) => void = () => {}
  export let onSelectFolder: (path: string) => void = () => {}
  export let onMoveItem: (id: string, path: string) => void | Promise<void> = () => {}
  export let onMoveFolder: (sourcePath: string, targetPath: string) => void | Promise<void> = () => {}
  export let onToggleFolder: (path: string) => void = () => {}
  export let favoriteItemIds: string[] = []
  export let onToggleFavorite: ((id: string) => void) | undefined = undefined

  let draggedItemId = ''
  let draggedFolderPath = ''
  let dragTargetPath = ''

  $: itemsByPath = items.reduce((groups, item) => {
    const path = normalizeFolderPath(item.path)
    const group = groups.get(path) ?? []
    group.push(item)
    groups.set(path, group)
    return groups
  }, new Map<string, FileNavigatorItem[]>())
  $: collapsedFolderSet = new Set(collapsedFolderPaths.map((path) => normalizeFolderPath(path)))
  $: favoriteItemSet = new Set(favoriteItemIds)
  $: folderPaths = Array.from(
    new Set([
      ...folders.map((folder) => normalizeFolderPath(folder.path)).filter((path) => path !== '/'),
      ...items.map((item) => normalizeFolderPath(item.path)).filter((path) => path !== '/'),
    ]),
  ).sort((left, right) => left.localeCompare(right))
  $: rootItems = itemsByPath.get('/') ?? []
  $: folderGroups = folderPaths.map((path) => ({
    path,
    items: itemsByPath.get(path) ?? [],
    collapsed: collapsedFolderSet.has(path),
  }))

  function normalizeFolderPath(path: string) {
    const trimmed = path.trim()
    if (!trimmed || trimmed === '/') return '/'
    return `/${trimmed.replace(/^\/+|\/+$/g, '')}`
  }

  function folderName(path: string) {
    const normalized = normalizeFolderPath(path)
    return normalized === '/' ? 'Root' : normalized.split('/').filter(Boolean).at(-1) ?? normalized
  }

  function startItemDrag(event: DragEvent, id: string) {
    draggedItemId = id
    draggedFolderPath = ''
    event.dataTransfer?.setData('application/x-og-file-item-id', id)
    if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move'
  }

  function startFolderDrag(event: DragEvent, path: string) {
    draggedFolderPath = normalizeFolderPath(path)
    draggedItemId = ''
    event.dataTransfer?.setData('application/x-og-file-folder-path', draggedFolderPath)
    if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move'
  }

  function allowDrop(event: DragEvent, path: string) {
    const targetPath = normalizeFolderPath(path)
    if (!draggedItemId && !draggedFolderPath) return
    if (draggedFolderPath && (targetPath === draggedFolderPath || targetPath.startsWith(`${draggedFolderPath}/`))) return
    event.preventDefault()
    dragTargetPath = targetPath
    if (event.dataTransfer) event.dataTransfer.dropEffect = 'move'
  }

  async function dropOnFolder(event: DragEvent, path: string) {
    event.preventDefault()
    event.stopPropagation()
    const targetPath = normalizeFolderPath(path)
    const itemId = event.dataTransfer?.getData('application/x-og-file-item-id') || draggedItemId
    const folderPath = event.dataTransfer?.getData('application/x-og-file-folder-path') || draggedFolderPath
    draggedItemId = ''
    draggedFolderPath = ''
    dragTargetPath = ''
    if (itemId) await onMoveItem(itemId, targetPath)
    if (folderPath) await onMoveFolder(folderPath, targetPath)
  }

  function endDrag() {
    draggedItemId = ''
    draggedFolderPath = ''
    dragTargetPath = ''
  }

  function toggleFavorite(event: MouseEvent | KeyboardEvent, id: string) {
    event.preventDefault()
    event.stopPropagation()
    onToggleFavorite?.(id)
  }
</script>

<div class="shared-file-tree" aria-label={`${itemLabel} folders`}>
  <section
    role="group"
    class="folder-group"
    data-folder-drop-target="/"
    on:dragover={(event) => allowDrop(event, '/')}
    on:dragleave={() => dragTargetPath = ''}
    on:drop={(event) => dropOnFolder(event, '/')}
  >
    <button
      class:active={activeFolderPath === '/'}
      class:drop-target={dragTargetPath === '/'}
      class="folder-row"
      on:click={() => onSelectFolder('/')}
    >
      <span class="folder-icon"><Icon name="folder" size={15} /></span>
      <span>Root</span>
      {#if rootItems.length === 0}<span class="folder-empty">Empty</span>{/if}
    </button>
    {#each rootItems as item}
      <button
        class:active={selectedItemId === item.id}
        class:dragging={draggedItemId === item.id}
        class="file-row"
        draggable="true"
        on:click={() => onSelectItem(item.id)}
        on:dragstart={(event) => startItemDrag(event, item.id)}
        on:dragend={endDrag}
      >
        <span>{item.title}</span>
        {#if item.meta}<small>{item.meta}</small>{/if}
        {#if onToggleFavorite}
          <span
            class:favorited={favoriteItemSet.has(item.id)}
            class="file-favorite"
            role="button"
            tabindex="0"
            aria-label={favoriteItemSet.has(item.id) ? `Remove ${item.title} from favorites` : `Add ${item.title} to favorites`}
            title={favoriteItemSet.has(item.id) ? 'Remove favorite' : 'Add favorite'}
            on:click={(event) => toggleFavorite(event, item.id)}
            on:keydown={(event) => {
              if (event.key === 'Enter' || event.key === ' ') toggleFavorite(event, item.id)
            }}
          >
            {favoriteItemSet.has(item.id) ? '★' : '☆'}
          </span>
        {/if}
      </button>
    {/each}
  </section>

  {#each folderGroups as folder}
    <section
      role="group"
      class="folder-group"
      data-folder-drop-target={folder.path}
      on:dragover={(event) => allowDrop(event, folder.path)}
      on:dragleave={() => dragTargetPath = ''}
      on:drop={(event) => dropOnFolder(event, folder.path)}
    >
      <button
        class:active={activeFolderPath === folder.path}
        class:selected-folder={selectedFolderPath === folder.path}
        class:drop-target={dragTargetPath === folder.path}
        class:dragging={draggedFolderPath === folder.path}
        class="folder-row"
        draggable="true"
        on:click={() => onSelectFolder(folder.path)}
        on:dragstart={(event) => startFolderDrag(event, folder.path)}
        on:dragend={endDrag}
      >
        <span class="folder-icon"><Icon name="folder" size={15} /></span>
        <span>{folder.path}</span>
        {#if folder.items.length === 0}<span class="folder-empty">Empty</span>{/if}
        <span class="folder-spacer"></span>
        <span
          class="folder-minimize"
          role="button"
          tabindex="0"
          aria-label={folder.collapsed ? 'Expand folder' : 'Minimize folder'}
          on:click|stopPropagation={() => onToggleFolder(folder.path)}
          on:keydown={(event) => {
            if (event.key === 'Enter' || event.key === ' ') {
              event.preventDefault()
              onToggleFolder(folder.path)
            }
          }}
        >
          {folder.collapsed ? '+' : '-'}
        </span>
      </button>
      {#if !folder.collapsed}
        {#each folder.items as item}
          <button
            class:active={selectedItemId === item.id}
            class:dragging={draggedItemId === item.id}
            class="file-row nested"
            draggable="true"
            on:click={() => onSelectItem(item.id)}
            on:dragstart={(event) => startItemDrag(event, item.id)}
            on:dragend={endDrag}
          >
            <span>{item.title}</span>
            {#if item.meta}<small>{item.meta}</small>{/if}
            {#if onToggleFavorite}
              <span
                class:favorited={favoriteItemSet.has(item.id)}
                class="file-favorite"
                role="button"
                tabindex="0"
                aria-label={favoriteItemSet.has(item.id) ? `Remove ${item.title} from favorites` : `Add ${item.title} to favorites`}
                title={favoriteItemSet.has(item.id) ? 'Remove favorite' : 'Add favorite'}
                on:click={(event) => toggleFavorite(event, item.id)}
                on:keydown={(event) => {
                  if (event.key === 'Enter' || event.key === ' ') toggleFavorite(event, item.id)
                }}
              >
                {favoriteItemSet.has(item.id) ? '★' : '☆'}
              </span>
            {/if}
          </button>
        {/each}
      {/if}
    </section>
  {/each}
</div>

<style>
  .shared-file-tree {
    display: grid;
    align-content: start;
    grid-auto-rows: max-content;
    gap: 8px;
    min-width: 0;
  }

  .folder-group {
    display: grid;
    align-content: start;
    grid-auto-rows: max-content;
    gap: 4px;
    min-width: 0;
  }

  .folder-row,
  .file-row {
    box-sizing: border-box;
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    min-width: 0;
    min-height: 34px;
    border: 1px solid transparent;
    border-radius: max(6px, calc(var(--field-radius, var(--og-field-radius)) - 2px));
    background: transparent;
    color: var(--text, var(--og-text));
    padding: 7px 9px;
    text-align: left;
    cursor: pointer;
  }

  .folder-row:hover,
  .file-row:hover,
  .folder-row.active,
  .file-row.active {
    background: color-mix(in srgb, var(--surface-subtle, var(--og-surface-subtle)) 78%, transparent);
  }

  .folder-row.selected-folder,
  .folder-row.drop-target {
    border-color: color-mix(in srgb, var(--accent, var(--og-accent)) 44%, var(--border, var(--og-border)));
    background: color-mix(in srgb, var(--accent-soft, var(--og-accent-soft)) 70%, transparent);
  }

  .file-row.nested {
    margin-left: 18px;
    width: calc(100% - 18px);
  }

  .file-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    column-gap: 8px;
    align-items: center;
  }

  .file-row span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .folder-row > span:not(.folder-icon):not(.folder-empty):not(.folder-spacer):not(.folder-minimize) {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .file-row small,
  .folder-empty {
    color: var(--muted, var(--og-muted));
    font-size: 11px;
  }

  .file-row small {
    grid-column: 1;
  }

  .file-favorite {
    grid-column: 2;
    grid-row: 1 / span 2;
    align-self: center;
    justify-self: end;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    border-radius: var(--field-radius, var(--og-field-radius));
    color: var(--muted, var(--og-muted));
    font-size: 18px;
    line-height: 1;
    opacity: 0;
    transition: opacity 120ms ease, color 120ms ease, background 120ms ease;
  }

  .file-row:hover .file-favorite,
  .file-row:focus-visible .file-favorite,
  .file-favorite.favorited,
  .file-favorite:focus-visible {
    opacity: 1;
  }

  .file-favorite.favorited {
    color: var(--accent, var(--og-accent));
  }

  .file-favorite:hover,
  .file-favorite:focus-visible {
    background: color-mix(in srgb, var(--surface-strong, var(--og-surface-strong)) 78%, transparent);
    color: var(--accent, var(--og-accent));
    outline: none;
  }

  .folder-icon {
    color: var(--accent, var(--og-accent));
    font-weight: 700;
  }

  .folder-spacer {
    flex: 1 1 auto;
  }

  .folder-minimize {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: var(--field-radius, var(--og-field-radius));
    color: var(--muted, var(--og-muted));
  }

  .folder-minimize:hover {
    background: color-mix(in srgb, var(--surface-subtle, var(--og-surface-subtle)) 84%, transparent);
    color: var(--text, var(--og-text));
  }

  .dragging {
    opacity: 0.52;
  }
</style>
