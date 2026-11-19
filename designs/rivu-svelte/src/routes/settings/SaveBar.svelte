<!--
  卡片底部的保存行：只在有改动时出现，右对齐「还原」与「保存」。放在 Card 的内容区末尾，不使用 footer。
-->
<script lang="ts">
  import Button from "../../lib/ui/Button.svelte";
  import { t } from "../../lib/i18n";
  import "./strings";

  interface Props {
    dirty: boolean;
    onsave: () => void;
    onreset: () => void;
    /** 有校验错误时保存不可点 */
    invalid?: boolean;
    busy?: boolean;
  }
  let { dirty, onsave, onreset, invalid = false, busy = false }: Props = $props();
</script>

{#if dirty}
  <div class="bar">
    <Button variant="ghost" onclick={onreset} disabled={busy}>{t("set.reset")}</Button>
    <Button variant="primary" disabled={invalid} loading={busy} onclick={onsave}>{t("common.save")}</Button>
  </div>
{/if}

<style>
  .bar {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
    padding-top: var(--sp-4);
    border-top: var(--hairline) solid var(--border-subtle);
  }
</style>
