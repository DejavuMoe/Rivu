<!--
  自定义范围：两个原生日期输入（键盘可用，手机上弹出系统的日期选择器），按站点本地日、含两端。
  打开时填入当前范围的起止日；应用后写进 URL（range=custom&from=…&to=…），和预设一样可以分享、刷新不丢。
-->
<script lang="ts">
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import { t } from "../../lib/i18n";
  import { parseYmd } from "./period";

  interface Props {
    open: boolean;
    /** 打开时的起止日（YYYY-MM-DD） */
    from: string;
    to: string;
    /** 站点本地的今天（YYYY-MM-DD），日期选择器的上限 */
    today: string;
    /** 站点时区；合并看板传空串（每个站点按自己的时区） */
    timezone: string;
    onapply: (from: string, to: string) => void;
  }
  let { open = $bindable(), from: initialFrom, to: initialTo, today, timezone, onapply }: Props = $props();

  // 每次打开都从当前范围开始（关闭时不保留改了一半的值）
  let from = $state("");
  let to = $state("");
  let tried = $state(false);
  $effect.pre(() => {
    if (open) {
      from = initialFrom;
      to = initialTo;
      tried = false;
    }
  });

  const fromErr = $derived.by(() => {
    if (!from) return t("range.err.missing");
    if (!parseYmd(from)) return t("range.err.missing");
    if (from < "1970-01-03") return t("range.err.tooOld");
    if (from > today) return t("range.err.future");
    return "";
  });
  const toErr = $derived.by(() => {
    if (!to || !parseYmd(to)) return t("range.err.missing");
    if (!fromErr && to < from) return t("range.err.order");
    return "";
  });

  function apply(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (fromErr || toErr) return;
    onapply(from, to);
    open = false;
  }
</script>

<Dialog bind:open title={t("range.customTitle")} size="sm">
  <form id="range-form" class="form" onsubmit={apply} novalidate>
    <div class="pair">
      <Field label={t("range.from")} for="range-from" error={tried && fromErr ? fromErr : undefined}>
        <Input id="range-from" type="date" bind:value={from} min="1970-01-03" max={today} invalid={tried && !!fromErr} required />
      </Field>
      <Field label={t("range.to")} for="range-to" error={tried && toErr ? toErr : undefined}>
        <Input id="range-to" type="date" bind:value={to} min={from || "1970-01-03"} max={today} invalid={tried && !!toErr} required />
      </Field>
    </div>
    <p class="hint">{timezone ? t("range.tzHint", { tz: timezone }) : t("range.tzHintEach")}</p>
  </form>
  {#snippet footer()}
    <Button onclick={() => (open = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" type="submit" form="range-form">{t("common.apply")}</Button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .pair {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-3);
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  @media (max-width: 480px) {
    .pair {
      grid-template-columns: 1fr;
    }
  }
</style>
