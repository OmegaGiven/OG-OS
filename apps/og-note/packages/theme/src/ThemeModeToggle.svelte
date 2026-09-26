<script lang="ts">
  import type { ThemeMode } from './index'

  export let mode: ThemeMode
  export let systemThemeAvailable: boolean
  export let onChange: (mode: ThemeMode) => void
</script>

<div class="theme-mode-toggle">
  <label>
    <input
      type="radio"
      name="og-theme-mode"
      value="system"
      checked={mode === 'system'}
      disabled={!systemThemeAvailable}
      on:change={() => onChange('system')}
    />
    <span>Follow system theme</span>
    {#if !systemThemeAvailable}
      <span class="theme-mode-hint">(no og-settings theme found on this device)</span>
    {/if}
  </label>
  <label>
    <input
      type="radio"
      name="og-theme-mode"
      value="independent"
      checked={mode === 'independent'}
      on:change={() => onChange('independent')}
    />
    <span>Independent theme</span>
  </label>
</div>

<style>
  .theme-mode-toggle {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  label {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
  }
  .theme-mode-hint {
    font-size: 0.85em;
    opacity: 0.6;
  }
</style>
