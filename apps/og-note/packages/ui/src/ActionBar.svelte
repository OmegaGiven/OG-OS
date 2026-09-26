<script lang="ts">
  export let ariaLabel = 'Actions'
  export let className = ''
  export let align: 'start' | 'center' | 'end' | 'between' = 'start'
  export let wrap: 'wrap' | 'nowrap' = 'wrap'
  export let attached: 'none' | 'top' | 'bottom' = 'none'
  export let onMouseDown: ((event: MouseEvent) => void) | undefined = undefined
</script>

<div
  class={`og-action-bar align-${align} ${wrap} attached-${attached} ${className}`}
  role="toolbar"
  tabindex="-1"
  aria-label={ariaLabel}
  on:mousedown={onMouseDown}
>
  <slot />
</div>

<style>
  .og-action-bar {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 6px 8px;
    border: 1px solid color-mix(in srgb, var(--border, var(--og-border)) 84%, transparent);
    border-radius: var(--field-radius, var(--og-field-radius));
    background: var(--action-bar-bg, var(--og-action-bar-bg));
    background-image: var(--nav-texture, var(--og-nav-texture, none));
    background-blend-mode: soft-light;
    color: var(--text, var(--og-text));
    backdrop-filter: blur(14px);
    min-width: 0;
  }

  .og-action-bar.align-start {
    justify-content: flex-start;
  }

  .og-action-bar.align-center {
    justify-content: center;
  }

  .og-action-bar.align-end {
    justify-content: flex-end;
  }

  .og-action-bar.align-between {
    justify-content: space-between;
  }

  .og-action-bar.wrap {
    flex-wrap: wrap;
  }

  .og-action-bar.nowrap {
    flex-wrap: nowrap;
    overflow-x: auto;
  }

  .og-action-bar.attached-top {
    border-bottom: 0;
    border-bottom-left-radius: 0;
    border-bottom-right-radius: 0;
  }

  .og-action-bar.attached-bottom {
    border-top: 0;
    border-top-left-radius: 0;
    border-top-right-radius: 0;
  }

  .og-action-bar :global(button),
  .og-action-bar :global(select),
  .og-action-bar :global(.og-action-control) {
    min-width: 34px;
    height: 34px;
    min-height: 34px;
    border: 1px solid transparent;
    border-radius: calc(var(--field-radius, var(--og-field-radius)) - 2px);
    background: transparent;
    color: var(--text, var(--og-text));
    font-weight: 700;
  }

  .og-action-bar :global(button) {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    padding: 0 9px;
  }

  .og-action-bar :global(button.icon-only),
  .og-action-bar :global(.icon-only button),
  .og-action-bar :global(label.icon-select-tool),
  .og-action-bar :global(label.color-tool) {
    width: 34px;
    padding: 0;
  }

  .og-action-bar :global(button:hover:not(:disabled)),
  .og-action-bar :global(select:hover:not(:disabled)),
  .og-action-bar :global(button:focus-visible),
  .og-action-bar :global(select:focus-visible),
  .og-action-bar :global(.og-action-control:hover),
  .og-action-bar :global(.og-action-control:focus-within) {
    background: color-mix(in srgb, var(--surface-strong, var(--og-surface-strong)) 72%, transparent);
    outline: none;
  }

  .og-action-bar :global(button.active-action),
  .og-action-bar :global(.og-action-control.active-action),
  .og-action-bar :global(label.active-action) {
    background: color-mix(in srgb, var(--accent, var(--og-accent)) 12%, transparent);
    outline: 1px solid color-mix(in srgb, var(--accent, var(--og-accent)) 76%, transparent);
    outline-offset: -2px;
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent, var(--og-accent)) 76%, transparent);
  }

  .og-action-bar :global(button.danger-action),
  .og-action-bar :global(.danger-action button) {
    border-color: var(--danger-border, var(--og-danger-border));
    background: var(--danger-soft, var(--og-danger-soft));
    color: var(--danger, var(--og-danger));
  }

  .og-action-bar :global(button.primary-action),
  .og-action-bar :global(.primary-action button) {
    border-color: color-mix(in srgb, var(--accent, var(--og-accent)) 42%, var(--border, var(--og-border)));
    background: var(--accent, var(--og-accent));
    color: var(--accent-contrast, var(--og-accent-contrast));
  }
</style>
