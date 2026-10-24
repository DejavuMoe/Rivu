<!--
  数据采集：开关（启用采集、Hash 路由）立即生效；每分钟上限是文本字段，有改动时才出现「保存」。
  上限留空 = 用实例默认值（placeholder 显示默认值）。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Card from "#lib/ui/Card.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Switch from "#lib/ui/Switch.svelte";
  import { full } from "#lib/format.ts";
  import { t } from "#lib/i18n.ts";
  import { app, toast } from "#lib/state.svelte.ts";
  import SaveBar from "./SaveBar.svelte";
  import { useSettings } from "./settings.svelte.ts";
  import "./strings";

  const getStore = useSettings();
  const store = $derived(getStore());
  const data = $derived(store.data!);

  let enabled = $state(untrack(() => store.data!.settings.collect_enabled));
  let hash = $state(untrack(() => store.data!.settings.tracker_hash));
  let rate = $state(untrack(() => String(store.data!.settings.ingest_rate_limit_per_min ?? "")));
  let busy = $state(false);

  const max = $derived(data.limits.ingest_rate_limit_max_per_min);
  const current = $derived(String(data.settings.ingest_rate_limit_per_min ?? ""));
  const rateNum = $derived(rate.trim() === "" ? null : Number(rate));
  const rateBad = $derived(rateNum !== null && (!Number.isInteger(rateNum) || rateNum < 1 || rateNum > max));
  const dirty = $derived(rate.trim() !== current);

  async function toggleEnabled(v: boolean) {
    if (await store.patch({ collect_enabled: v })) toast(v ? t("set.coll.on") : t("set.coll.off"), "good");
    else enabled = !v;
  }
  async function toggleHash(v: boolean) {
    if (await store.patch({ tracker_hash: v })) toast(t("common.saved"), "good");
    else hash = !v;
  }
  async function save() {
    busy = true;
    const ok = await store.patch({ ingest_rate_limit_per_min: rateNum });
    busy = false;
    if (ok) toast(t("common.saved"), "good");
  }
  function reset() {
    rate = current;
  }
</script>

<Card title={t("set.coll.title")}>
  <Field layout="row" label={t("set.coll.enabled")} for="s-enabled">
    <div class="sw">
      <Switch id="s-enabled" label={t("set.coll.enabled")} bind:checked={enabled} onchange={toggleEnabled} />
    </div>
    {#if !enabled}
      <Callout tone="warn">{t("set.coll.offWarn")}</Callout>
    {/if}
  </Field>
  <Field layout="row" label={t("set.coll.hash")} for="s-hash">
    <div class="sw">
      <Switch id="s-hash" label={t("set.coll.hash")} bind:checked={hash} onchange={toggleHash} />
    </div>
  </Field>
</Card>

<Card title={t("set.coll.limits")}>
  <Field layout="row" label={t("set.coll.rate")} for="s-rate" error={rateBad ? t("set.coll.rateBad", { max: full(max, app.lang) }) : undefined}>
    <Input
      id="s-rate"
      type="text"
      inputmode="numeric"
      bind:value={rate}
      invalid={rateBad}
      placeholder={data.limits.ingest_rate_limit_default_per_min === null
        ? t("set.coll.rateNone")
        : t("set.coll.ratePlaceholder", { n: full(data.limits.ingest_rate_limit_default_per_min, app.lang) })}
    >
      {#snippet suffix()}{t("set.coll.perMin")}{/snippet}
    </Input>
  </Field>
  <SaveBar {dirty} invalid={rateBad} {busy} onsave={save} onreset={reset} />
</Card>

<style>
  /* 20px 高的开关与 14px 文字（行高 21）的首行居中对齐 */
  .sw {
    display: flex;
    padding: 6px 0;
  }
</style>
