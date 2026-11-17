<script lang="ts">
  let {
    checked,
    onchange,
    label,
    disabled = false,
  }: { checked: boolean; onchange: (v: boolean) => void; label: string; disabled?: boolean } = $props();
</script>

<button
  type="button"
  role="switch"
  class="toggle"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  onclick={() => onchange(!checked)}
>
  <span class="knob"></span>
</button>

<style>
  .toggle {
    position: relative;
    width: 44px;
    height: 24px;
    flex: none;
    border-radius: 99px;
    border: 1px solid var(--border-strong);
    background: var(--bg-input);
    cursor: pointer;
    transition: background 0.18s, border-color 0.18s;
  }
  .toggle[aria-checked="true"] {
    background: var(--accent-grad);
    border-color: transparent;
  }
  .toggle:focus-visible {
    outline: none;
    box-shadow: 0 0 0 3px var(--focus);
  }
  .toggle:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .knob {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.4);
    transition: transform 0.18s;
  }
  .toggle[aria-checked="true"] .knob {
    transform: translateX(20px);
  }
</style>
