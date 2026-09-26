<script lang="ts" context="module">
  export type MobileSuiteNavItem = {
    id: string
    name: string
    disabled?: boolean
  }
</script>

<script lang="ts">
  import Icon from './Icon.svelte'

  export let navItems: MobileSuiteNavItem[] = []
  export let activeAppId = ''
  export let onSelectApp: (appId: string) => void = () => {}
  export let onOpenSettings: (() => void) | undefined = undefined
  export let onClose: (() => void) | undefined = undefined
</script>

<nav class:with-close={onClose} class="mobile-suite-top-bar" aria-label="Suite apps">
  {#each navItems as item}
    <button
      class:active={activeAppId === item.id}
      disabled={item.disabled}
      aria-current={activeAppId === item.id ? 'page' : undefined}
      on:click={() => onSelectApp(item.id)}
    >
      {item.name}
    </button>
  {/each}
  {#if onOpenSettings}
    <button class="mobile-suite-settings-button" aria-label="Open settings" title="Settings" on:click={onOpenSettings}>
      <Icon name="settings" size={16} />
    </button>
  {/if}
  {#if onClose}
    <button class="mobile-suite-top-close" aria-label="Close menu" title="Close menu" on:click={onClose}>
      <span aria-hidden="true"></span>
    </button>
  {/if}
</nav>

<style>
  .mobile-suite-top-bar {
    display: none;
  }

  @media (max-width: 760px) {
    .mobile-suite-top-bar {
      display: flex;
      position: relative;
      align-items: center;
      flex-wrap: nowrap;
      gap: 3px;
      min-height: 52px;
      margin: 0 0 8px;
      padding: 4px 92px 4px 4px;
      overflow-x: auto;
      border: 1px solid color-mix(in srgb, var(--border, var(--og-border)) 78%, transparent);
      border-radius: var(--field-radius, var(--og-field-radius));
      background: var(--action-bar-bg, var(--og-action-bar-bg));
      background-image: var(--nav-texture, var(--og-nav-texture, none));
      background-blend-mode: soft-light;
      scroll-snap-type: x mandatory;
      scrollbar-width: none;
    }

    .mobile-suite-top-bar::-webkit-scrollbar {
      display: none;
    }

    .mobile-suite-top-bar > button:not(.mobile-suite-settings-button):not(.mobile-suite-top-close) {
      display: inline-flex;
      align-items: center;
      justify-content: center;
      gap: 5px;
      flex: 0 0 auto;
      min-width: 58px;
      min-height: 28px;
      height: 28px;
      padding: 0 8px;
      border: 0;
      border-radius: calc(var(--field-radius, var(--og-field-radius)) - 2px);
      background: transparent;
      color: var(--muted, var(--og-muted));
      font-size: 0.78rem;
      font-weight: 900;
      letter-spacing: 0;
      scroll-snap-align: start;
    }

    .mobile-suite-top-bar .mobile-suite-settings-button {
      position: absolute;
      top: 50%;
      right: 7px;
      z-index: 2;
      width: 34px;
      min-width: 34px;
      flex-basis: 34px;
      height: 34px;
      min-height: 34px;
      padding: 0;
      border: 1px solid var(--border, var(--og-border));
      border-radius: var(--field-radius, var(--og-field-radius));
      background: var(--action-bar-bg, var(--og-action-bar-bg));
      background-image: var(--nav-texture, var(--og-nav-texture, none));
      background-blend-mode: soft-light;
      color: var(--text, var(--og-text));
      transform: translateY(-50%);
    }

    .mobile-suite-top-bar.with-close .mobile-suite-settings-button {
      right: 47px;
    }

    .mobile-suite-top-bar .mobile-suite-top-close {
      position: absolute;
      top: 50%;
      right: 7px;
      z-index: 3;
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
      background: var(--action-bar-bg, var(--og-action-bar-bg));
      background-image: var(--nav-texture, var(--og-nav-texture, none));
      background-blend-mode: soft-light;
      color: var(--text, var(--og-text));
      transform: translateY(-50%);
    }

    .mobile-suite-top-bar .mobile-suite-top-close span,
    .mobile-suite-top-bar .mobile-suite-top-close span::before,
    .mobile-suite-top-bar .mobile-suite-top-close span::after {
      display: block;
      width: 15px;
      height: 2px;
      border-radius: 999px;
      background: currentColor;
    }

    .mobile-suite-top-bar .mobile-suite-top-close span {
      position: relative;
      background: transparent;
    }

    .mobile-suite-top-bar .mobile-suite-top-close span::before,
    .mobile-suite-top-bar .mobile-suite-top-close span::after {
      content: '';
      position: absolute;
      left: 0;
      top: 0;
    }

    .mobile-suite-top-bar .mobile-suite-top-close span::before {
      transform: rotate(45deg);
    }

    .mobile-suite-top-bar .mobile-suite-top-close span::after {
      transform: rotate(-45deg);
    }

    .mobile-suite-top-bar > button:not(.mobile-suite-settings-button):not(.mobile-suite-top-close).active,
    .mobile-suite-top-bar > button:not(.mobile-suite-settings-button):not(.mobile-suite-top-close):hover:not(:disabled),
    .mobile-suite-top-bar > button:not(.mobile-suite-settings-button):not(.mobile-suite-top-close):focus-visible {
      background: color-mix(in srgb, var(--accent-soft, var(--og-accent-soft)) 64%, transparent);
      color: var(--text, var(--og-text));
      outline: none;
    }
  }
</style>
