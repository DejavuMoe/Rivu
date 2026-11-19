<!-- 开关：立即生效的布尔设置。需要「保存」才生效的表单用 Checkbox。 -->
<script lang="ts">
  interface Props {
    checked: boolean;
    label: string;
    disabled?: boolean;
    onchange?: (v: boolean) => void;
    id?: string;
  }
  let { checked = $bindable(), label, disabled = false, onchange, id }: Props = $props();
</script>

<button
  {id}
  type="button"
  role="switch"
  class="switch"
  aria-checked={checked}
  aria-label={label}
  {disabled}
  onclick={() => {
    checked = !checked;
    onchange?.(checked);
  }}
>
  <span class="thumb"></span>
</button>

<style>
  .switch {
    position: relative;
    flex: none;
    width: 32px;
    height: 20px;
    padding: 0;
    border: 0;
    border-radius: var(--r-full);
    background: var(--bg-active);
  }
  .switch[aria-checked="true"] {
    background: var(--accent-solid);
  }
  .switch:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .thumb {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--accent-contrast);
    transition: transform var(--dur) var(--ease);
  }
  [aria-checked="true"] .thumb {
    transform: translateX(12px);
  }
</style>
