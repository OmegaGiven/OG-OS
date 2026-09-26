<script lang="ts" context="module">
  export type { MobileSuiteNavItem } from './MobileSuiteTopBar.svelte'
</script>

<script lang="ts">
  import MobileSuiteTopBar from './MobileSuiteTopBar.svelte'
  import type { MobileSuiteNavItem } from './MobileSuiteTopBar.svelte'

  export let navItems: MobileSuiteNavItem[] = []
  export let activeAppId = ''
  export let onSelectApp: (appId: string) => void = () => {}
  export let onOpenSettings: (() => void) | undefined = undefined
  export let title = 'Menu'
  export let align: 'inline' | 'right' = 'right'

  let open = false

  function selectApp(appId: string) {
    onSelectApp(appId)
    open = false
  }

  function openSettings() {
    onOpenSettings?.()
    open = false
  }
</script>

<div class:open class={`mobile-suite-menu mobile-suite-menu-${align}`}>
  <button
    class="mobile-suite-menu-trigger"
    aria-label={open ? 'Close menu' : 'Open menu'}
    aria-expanded={open}
    title="Menu"
    on:click={() => open = !open}
  >
    <span aria-hidden="true"></span>
  </button>

  {#if open}
    <button class="mobile-suite-menu-backdrop" aria-label="Close menu" on:click={() => open = false}></button>
    <aside class="mobile-suite-menu-drawer" aria-label={`${title} navigation`}>
      <MobileSuiteTopBar {navItems} {activeAppId} onSelectApp={selectApp} onOpenSettings={onOpenSettings ? openSettings : undefined} onClose={() => open = false} />

      <div class="mobile-suite-menu-tools">
        <slot />
      </div>
    </aside>
  {/if}
</div>

<style>
  .mobile-suite-menu {
    display: none;
  }

  @media (max-width: 760px) {
    .mobile-suite-menu {
      position: relative;
      z-index: 90;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      flex: 0 0 auto;
    }

    .mobile-suite-menu.open {
      position: fixed;
      inset: 0;
      z-index: 2147483000;
      display: block;
      pointer-events: none;
    }

    :global(.feed-app:has(.mobile-suite-menu.open)),
    :global(.notes-app:has(.mobile-suite-menu.open)),
    :global(.audio-app:has(.mobile-suite-menu.open)),
    :global(.files-app:has(.mobile-suite-menu.open)),
    :global(.suite-content:has(.mobile-suite-menu.open)),
    :global(.feed-hero:has(.mobile-suite-menu.open)),
    :global(.recorder-panel:has(.mobile-suite-menu.open)) {
      position: relative;
      z-index: 2147483000;
    }

    :global(.feed-hero:has(.mobile-suite-menu.open)),
    :global(.files-library:has(.mobile-suite-menu.open)),
    :global(.og-action-bar:has(.mobile-suite-menu.open)),
    :global(.recorder-panel:has(.mobile-suite-menu.open)) {
      backdrop-filter: none !important;
    }

    .mobile-suite-menu-trigger {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      width: 34px;
      min-width: 34px;
      height: 34px;
      min-height: 34px;
      padding: 0;
      border: 1px solid var(--border, var(--og-border));
      border-radius: var(--field-radius, var(--og-field-radius));
      background: var(--surface-subtle, var(--og-surface-subtle));
      color: var(--text, var(--og-text));
    }

    .mobile-suite-menu-trigger span {
      display: block;
      position: relative;
      width: 15px;
      height: 2px;
      border-radius: 999px;
      background: currentColor;
    }

    .mobile-suite-menu-trigger span::before,
    .mobile-suite-menu-trigger span::after {
      content: '';
      position: absolute;
      left: 0;
      width: 15px;
      height: 2px;
      border-radius: 999px;
      background: currentColor;
    }

    .mobile-suite-menu-trigger span::before {
      top: -5px;
    }

    .mobile-suite-menu-trigger span::after {
      top: 5px;
    }

    .mobile-suite-menu.open .mobile-suite-menu-trigger {
      display: none;
    }

    .mobile-suite-menu-backdrop {
      position: fixed;
      inset: 0;
      z-index: 2147483001;
      width: auto;
      height: auto;
      border: 0;
      border-radius: 0;
      background: color-mix(in srgb, var(--bg, var(--og-bg)) 58%, transparent);
      backdrop-filter: blur(8px);
      pointer-events: auto;
    }

    .mobile-suite-menu-drawer {
      position: fixed;
      inset: 0;
      z-index: 2147483002;
      display: grid;
      grid-template-rows: auto minmax(0, 1fr) auto;
      align-content: stretch;
      gap: 8px;
      width: 100vw;
      height: 100vh;
      max-height: none;
      padding: max(10px, env(safe-area-inset-top)) 10px calc(10px + env(safe-area-inset-bottom));
      border: 0;
      background: var(--panel-surface, var(--og-surface));
      background-image: var(--panel-texture, var(--og-panel-texture, none));
      background-blend-mode: soft-light;
      color: var(--text, var(--og-text));
      box-shadow: var(--shadow, var(--og-shadow));
      backdrop-filter: blur(18px);
      overflow: auto;
      pointer-events: auto;
    }

    .mobile-suite-menu-tools {
      display: flex;
      align-items: center;
      gap: 6px;
      grid-row: 2;
      align-self: start;
      flex-wrap: wrap;
      border: 1px solid color-mix(in srgb, var(--border, var(--og-border)) 78%, transparent);
      border-radius: var(--field-radius, var(--og-field-radius));
      background: var(--action-bar-bg, var(--og-action-bar-bg));
      background-image: var(--nav-texture, var(--og-nav-texture, none));
      background-blend-mode: soft-light;
      padding: 4px;
    }

    .mobile-suite-menu-tools > :global(button) {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      gap: 6px;
      min-height: 28px;
      height: 28px;
      min-width: 72px;
      border: 0;
      border-radius: calc(var(--field-radius, var(--og-field-radius)) - 2px);
      background: transparent;
      color: var(--muted, var(--og-muted));
      padding: 0 10px;
      font-size: 0.78rem;
      font-weight: 900;
      letter-spacing: 0;
      flex: 0 0 auto;
    }

    .mobile-suite-menu-tools > :global(button:hover:not(:disabled)),
    .mobile-suite-menu-tools > :global(button:focus-visible) {
      background: color-mix(in srgb, var(--accent-soft, var(--og-accent-soft)) 64%, transparent);
      color: var(--text, var(--og-text));
      outline: none;
    }
  }
</style>
