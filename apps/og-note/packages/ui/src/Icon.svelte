<script lang="ts">
  export let name:
    | 'search'
    | 'filter'
    | 'new-note'
    | 'new-diagram'
    | 'folder'
    | 'new-folder'
    | 'upload'
    | 'rename'
    | 'visibility'
    | 'refresh'
    | 'delete'
    | 'microphone'
    | 'download'
    | 'open'
    | 'expand'
    | 'collapse'
    | 'share'
    | 'list'
    | 'settings'
    | 'sync'
    | 'save'
    | 'star'
    | 'bold'
    | 'italic'
    | 'header'
    | 'indent'
    | 'outdent'
    | 'divider'
    | 'quote'
    | 'code'
    | 'align-left'
    | 'align-center'
    | 'align-right'
    | 'table'
    | 'image'
    | 'link'
    | 'text-color'
    | 'highlight'
    | 'strikethrough'
    | 'underline'
    | 'font'
    | 'phone'
    | 'video'
    | 'camera'
    | 'send'
    | 'chat'
    | 'add-list'
    | 'participants'
    | 'undo'
    | 'redo'
  export let size = 24
  export let label = ''
  export let className = ''

  $: ariaHidden = label ? undefined : true
</script>

<svg
  viewBox="0 0 24 24"
  width={size}
  height={size}
  class={`og-icon ${className}`}
  role={label ? 'img' : undefined}
  aria-label={label || undefined}
  aria-hidden={ariaHidden}
>
  {#if name === 'search'}
    <circle cx="10.5" cy="10.5" r="5.1" fill="none" stroke="currentColor" stroke-width="1.9" />
    <path d="M14.35 14.35 18.8 18.8" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'filter'}
    <path d="M4.5 6.25h15l-5.65 6.35v5.15l-3.7 1.65V12.6Z" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round" />
  {:else if name === 'new-note' || name === 'new-diagram'}
    <path d="M6 3.75h8.7l3.3 3.35v13.15H6z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <path d="M14.7 3.75V7.1H18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <path d="M12 9.75v6.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
    <path d="M8.75 13h6.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
  {:else if name === 'new-folder'}
    <path d="M3.75 7.25A2.25 2.25 0 0 1 6 5h4.15l1.55 1.7H18A2.25 2.25 0 0 1 20.25 9v7.75A2.25 2.25 0 0 1 18 19H6a2.25 2.25 0 0 1-2.25-2.25Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <path d="M15.75 10.25v5.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
    <path d="M13 13h5.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
  {:else if name === 'folder'}
    <path d="M3.75 7.25A2.25 2.25 0 0 1 6 5h4.15l1.55 1.7H18A2.25 2.25 0 0 1 20.25 9v7.75A2.25 2.25 0 0 1 18 19H6a2.25 2.25 0 0 1-2.25-2.25Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
  {:else if name === 'upload'}
    <path d="M12 4.75v10.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M8.6 8.35 12 4.75l3.4 3.6" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M6 15.75v1.5c0 .83.67 1.5 1.5 1.5h9c.83 0 1.5-.67 1.5-1.5v-1.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'download'}
    <path d="M12 4.75v10.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M8.6 11.65 12 15.25l3.4-3.6" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M6 17.25v1c0 .83.67 1.5 1.5 1.5h9c.83 0 1.5-.67 1.5-1.5v-1" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'rename'}
    <path d="M4.75 19.25h4.1l9.35-9.35-4.1-4.1-9.35 9.35v4.1Z" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round" />
    <path d="m12.95 6.95 4.1 4.1" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'visibility'}
    <path d="M2.75 12s3.35-5.5 9.25-5.5 9.25 5.5 9.25 5.5-3.35 5.5-9.25 5.5S2.75 12 2.75 12Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <circle cx="12" cy="12" r="2.9" fill="none" stroke="currentColor" stroke-width="1.8" />
  {:else if name === 'refresh' || name === 'sync'}
    <path d="M19.25 7.75V4.75h-3" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M18.85 12a6.85 6.85 0 1 1-2-4.85l2.4-2.4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'undo'}
    <path d="M8.75 8.25H4.75V4.25" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M4.9 8.2a7.2 7.2 0 1 1 2.1 5.1" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'redo'}
    <path d="M15.25 8.25h4V4.25" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M19.1 8.2a7.2 7.2 0 1 0-2.1 5.1" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'delete'}
    <path d="M5.75 7.25h12.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
    <path d="M9.25 7.25v-1.5A1.75 1.75 0 0 1 11 4h2a1.75 1.75 0 0 1 1.75 1.75v1.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <path d="M7.25 7.25 8 19h8l.75-11.75" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <path d="M10 10.25v5.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
    <path d="M14 10.25v5.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
  {:else if name === 'microphone'}
    <path d="M12 15.2a3.8 3.8 0 0 1-3.8-3.8V6.9a3.8 3.8 0 1 1 7.6 0v4.5a3.8 3.8 0 0 1-3.8 3.8Z" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round" />
    <path d="M6.7 10.9v.6a5.3 5.3 0 0 0 10.6 0v-.6" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M12 16.8v3.1" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M9 19.9h6" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'open'}
    <path d="M14 5h5v5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M13.25 10.75 19 5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M11 6H6.75A1.75 1.75 0 0 0 5 7.75v9.5C5 18.22 5.78 19 6.75 19h9.5c.97 0 1.75-.78 1.75-1.75V13" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'expand'}
    <path d="M9 5H5v4" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M15 5h4v4" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M9 19H5v-4" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M15 19h4v-4" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'collapse'}
    <path d="M9 3H3v6" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M15 3h6v6" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M9 21H3v-6" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M15 21h6v-6" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M8.5 8.5 3 3M15.5 8.5 21 3M8.5 15.5 3 21M15.5 15.5 21 21" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'share'}
    <path d="M8.7 13.25 15.3 17" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M15.3 7 8.7 10.75" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <circle cx="6.75" cy="12" r="2.25" fill="none" stroke="currentColor" stroke-width="1.9" />
    <circle cx="17.25" cy="5.9" r="2.25" fill="none" stroke="currentColor" stroke-width="1.9" />
    <circle cx="17.25" cy="18.1" r="2.25" fill="none" stroke="currentColor" stroke-width="1.9" />
  {:else if name === 'list'}
    <circle cx="5.75" cy="7" r="1.05" fill="currentColor" />
    <circle cx="5.75" cy="12" r="1.05" fill="currentColor" />
    <circle cx="5.75" cy="17" r="1.05" fill="currentColor" />
    <path d="M9 7h10M9 12h8M9 17h9.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'settings'}
    <circle cx="12" cy="12" r="2.8" fill="none" stroke="currentColor" stroke-width="1.75" />
    <circle cx="12" cy="12" r="6.15" fill="none" stroke="currentColor" stroke-width="1.45" opacity="0.95" />
    <path d="M12 3.8v2.05M12 18.15v2.05M3.8 12h2.05M18.15 12h2.05M6.2 6.2l1.45 1.45M16.35 16.35l1.45 1.45M17.8 6.2l-1.45 1.45M7.65 16.35 6.2 17.8" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" />
  {:else if name === 'save'}
    <path d="M5 4.75h11.1L19 7.65v11.6H5z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <path d="M8 4.75v5h7v-5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <path d="M8.25 19.25v-5.5h7.5v5.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
  {:else if name === 'star'}
    <path d="m12 4.25 2.18 4.65 5.07.78-3.67 3.63.86 5.12L12 16l-4.44 2.43.86-5.12-3.67-3.63 5.07-.78Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
  {:else if name === 'bold'}
    <path d="M8 5h5.2a3 3 0 0 1 0 6H8zM8 11h6a3.5 3.5 0 0 1 0 7H8z" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round" />
  {:else if name === 'italic'}
    <path d="M10 5h7M7 19h7M14.5 5 9.5 19" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'header'}
    <path d="M5.5 5.5v13M18.5 5.5v13M5.5 12h13" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'indent'}
    <path d="M4.75 6.5h14.5M10.75 11.5h8.5M10.75 16.5h8.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M4.75 10 8 12l-3.25 2" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'outdent'}
    <path d="M4.75 6.5h14.5M10.75 11.5h8.5M10.75 16.5h8.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M8 10 4.75 12 8 14" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'divider'}
    <path d="M4.75 12h14.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M7 8h10M7 16h10" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" opacity="0.5" />
  {:else if name === 'quote'}
    <path d="M7.7 7.25h4.05l-2.3 9.5H5.4l2.3-9.5ZM16.3 7.25h4.05l-2.3 9.5H14l2.3-9.5Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
  {:else if name === 'code'}
    <path d="m8.5 8-4 4 4 4M15.5 8l4 4-4 4M13.25 5.75l-2.5 12.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'align-left'}
    <path d="M5 7h14M5 12h10M5 17h12" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'align-center'}
    <path d="M5 7h14M7 12h10M6 17h12" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'align-right'}
    <path d="M5 7h14M9 12h10M7 17h12" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'table'}
    <path d="M4.75 5.5h14.5v13H4.75zM4.75 9.75h14.5M4.75 14h14.5M9.6 5.5v13M14.4 5.5v13" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linejoin="round" />
  {:else if name === 'image'}
    <path d="M5.5 5.75h13A1.75 1.75 0 0 1 20.25 7.5v9A1.75 1.75 0 0 1 18.5 18.25h-13A1.75 1.75 0 0 1 3.75 16.5v-9A1.75 1.75 0 0 1 5.5 5.75Z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <path d="m5.5 16.25 4.4-4.4 2.65 2.65 2-2 3.95 3.75" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
    <circle cx="15.8" cy="9" r="1.1" fill="currentColor" />
  {:else if name === 'link'}
    <path d="M9.7 14.3a3.2 3.2 0 0 1 0-4.55l2.1-2.1a3.22 3.22 0 0 1 4.55 4.55l-.85.85" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
    <path d="M14.3 9.7a3.2 3.2 0 0 1 0 4.55l-2.1 2.1a3.22 3.22 0 0 1-4.55-4.55l.85-.85" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'text-color'}
    <path d="M6.5 18.5 11.5 5.5h1l5 13M8.6 14h6.8" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
    <path d="M5.5 20.5h13" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'highlight'}
    <path d="M7.5 14.2 14.2 7.5l2.3 2.3-6.7 6.7H7.5z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" />
    <path d="M13.4 6.7 15 5.1c.35-.35.91-.35 1.26 0l2.64 2.64c.35.35.35.91 0 1.26l-1.6 1.6M5.5 19h13" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'strikethrough'}
    <path d="M7.3 7.1c.86-1.07 2.25-1.6 4.05-1.6 2.3 0 3.92.77 4.85 2.3M5 12h14M16.1 16.7c-.88 1.2-2.42 1.8-4.6 1.8-2.1 0-3.72-.64-4.85-1.93" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'underline'}
    <path d="M7.5 5.5v6.3a4.5 4.5 0 0 0 9 0V5.5M6 20h12" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'font'}
    <path d="M5 6h14M8 6v12M16 6v12M6.75 18h4.5M13.75 18h4.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'phone'}
    <path d="M8.45 5.6c.52-.52 1.34-.61 1.96-.21l2.12 1.36c.68.44.87 1.35.43 2.03l-.8 1.23c-.23.35-.18.81.11 1.1l1.9 1.9c.29.29.75.34 1.1.11l1.23-.8c.68-.44 1.59-.25 2.03.43l1.36 2.12c.4.62.31 1.44-.21 1.96l-1.1 1.1c-.84.84-2.09 1.16-3.24.82-2-.59-3.96-1.81-5.76-3.61-1.8-1.8-3.02-3.76-3.61-5.76-.34-1.15-.02-2.4.82-3.24Z" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'video'}
    <path d="M5.75 7h7.5A2.75 2.75 0 0 1 16 9.75v4.5A2.75 2.75 0 0 1 13.25 17h-7.5A2.75 2.75 0 0 1 3 14.25v-4.5A2.75 2.75 0 0 1 5.75 7Z" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round" />
    <path d="M16 10.1 20 7.8v8.4l-4-2.3" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'camera'}
    <path d="M5.5 6.5h4l1.2-1.5h2.6l1.2 1.5h4a1.75 1.75 0 0 1 1.75 1.75v8.5a1.75 1.75 0 0 1-1.75 1.75h-13A1.75 1.75 0 0 1 3.75 16.75v-8.5A1.75 1.75 0 0 1 5.5 6.5Z" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round" />
    <circle cx="12" cy="12.25" r="3.1" fill="none" stroke="currentColor" stroke-width="1.9" />
  {:else if name === 'send'}
    <path d="M4.75 19.25 20 12 4.75 4.75l2.05 6.1L13 12l-6.2 1.15-2.05 6.1Z" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'chat'}
    <path d="M5.5 7.25A2.75 2.75 0 0 1 8.25 4.5h7.5a2.75 2.75 0 0 1 2.75 2.75v5.5a2.75 2.75 0 0 1-2.75 2.75H11l-3.9 3.05c-.45.35-1.1.03-1.1-.54V15.5h-1A2.75 2.75 0 0 1 2.25 12.75v-5.5A2.75 2.75 0 0 1 5 4.5" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" />
  {:else if name === 'add-list'}
    <path d="M5.5 6.25h8M5.5 11h13M5.5 15.75h8M17.6 4.9v5.4M14.9 7.6h5.4" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {:else if name === 'participants'}
    <circle cx="8.9" cy="9.2" r="2.65" fill="none" stroke="currentColor" stroke-width="1.9" />
    <circle cx="15.15" cy="9.2" r="2.65" fill="none" stroke="currentColor" stroke-width="1.9" />
    <path d="M4.85 17.2c0-2.15 1.82-3.7 4.05-3.7s4.05 1.55 4.05 3.7M11.1 17.2c0-2.15 1.82-3.7 4.05-3.7s4.05 1.55 4.05 3.7" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" />
  {/if}
</svg>

<style>
  .og-icon {
    display: inline-block;
    flex: 0 0 auto;
    overflow: visible;
    vertical-align: middle;
  }
</style>
