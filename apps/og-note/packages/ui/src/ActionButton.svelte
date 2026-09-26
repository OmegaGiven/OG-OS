<script lang="ts">
  import type { ComponentProps } from 'svelte'
  import Icon from './Icon.svelte'

  type IconName = ComponentProps<Icon>['name']

  export let icon: IconName | undefined = undefined
  export let label = ''
  export let ariaLabel = label
  export let title = label
  export let tone: 'default' | 'primary' | 'danger' = 'default'
  export let iconOnly = false
  export let disabled = false
  export let type: 'button' | 'submit' | 'reset' = 'button'
  export let active = false
</script>

<button
  class:icon-only={iconOnly}
  class:active-action={active}
  class:primary-action={tone === 'primary'}
  class:danger-action={tone === 'danger'}
  {disabled}
  {type}
  aria-label={ariaLabel || title || label || undefined}
  {title}
  on:click
>
  {#if icon}
    <Icon name={icon} size={18} />
  {/if}
  {#if !iconOnly && label}
    <span>{label}</span>
  {/if}
</button>

<style>
  button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 7px;
    min-width: 34px;
    min-height: 34px;
    border: 1px solid transparent;
    border-radius: calc(var(--field-radius, var(--og-field-radius)) - 2px);
    background: color-mix(in srgb, var(--surface-subtle, var(--og-surface-subtle)) 84%, transparent);
    color: var(--text, var(--og-text));
    padding: 0 9px;
    font: inherit;
    font-weight: 700;
    cursor: pointer;
    transition: background 120ms ease, border-color 120ms ease, color 120ms ease, box-shadow 120ms ease;
  }

  button.icon-only {
    width: 34px;
    padding: 0;
  }

  button:hover:not(:disabled),
  button:focus-visible {
    background: color-mix(in srgb, var(--surface-strong, var(--og-surface-strong)) 72%, transparent);
    outline: none;
  }

  button:disabled {
    cursor: default;
    opacity: 0.5;
  }

  button.active-action {
    background: color-mix(in srgb, var(--accent, var(--og-accent)) 12%, transparent);
    outline: 1px solid color-mix(in srgb, var(--accent, var(--og-accent)) 76%, transparent);
    outline-offset: -2px;
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent, var(--og-accent)) 76%, transparent);
  }

  button.primary-action {
    border-color: color-mix(in srgb, var(--accent, var(--og-accent)) 42%, var(--border, var(--og-border)));
    background: var(--accent, var(--og-accent));
    color: var(--accent-contrast, var(--og-accent-contrast));
  }

  button.primary-action:hover:not(:disabled),
  button.primary-action:focus-visible {
    background: color-mix(in srgb, var(--accent, var(--og-accent)) 86%, #ffffff 14%);
  }

  button.danger-action {
    border-color: var(--danger-border, var(--og-danger-border));
    background: var(--danger-soft, var(--og-danger-soft));
    color: var(--danger, var(--og-danger));
  }

  button.danger-action:hover:not(:disabled),
  button.danger-action:focus-visible {
    background: color-mix(in srgb, var(--danger-soft, var(--og-danger-soft)) 82%, var(--danger, var(--og-danger)) 18%);
  }
</style>
