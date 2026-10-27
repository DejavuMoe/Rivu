<!--
  地理与保留。地理精度：只列出已加载的地理库支持的级别（国家级库只能选国家，城市级库可选国家、地区、城市），
  没有地理库时不记录位置，只给一行说明；单选立即生效。原始数据保留天数是文本字段，有改动时才出现「保存」，
  留空 = 实例默认（RIVU_RETENTION_DAYS，也是上限）。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import Card from "#lib/ui/Card.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Segmented from "#lib/ui/Segmented.svelte";
  import { full } from "#lib/format.ts";
  import { t } from "#lib/i18n.ts";
  import { app, toast } from "#lib/state.svelte.ts";
  import SaveBar from "./SaveBar.svelte";
  import { useSettings } from "./settings.svelte.ts";
  import "./strings";

  type Level = "country" | "region" | "city";

  const getStore = useSettings();
  const store = $derived(getStore());
  const data = $derived(store.data!);
  const cap = $derived(data.geo_capability);

  // 国家级库下，已存的城市 / 地区设置实际只能到国家
  let geo = $state<Level>(untrack(() => (store.data!.geo_capability === "country" ? "country" : store.data!.settings.geo_level)));
  const levels = $derived<Level[]>(cap === "city" ? ["country", "region", "city"] : ["country"]);

  async function change(v: Level) {
    const prev = store.data!.settings.geo_level;
    if (await store.patch({ geo_level: v })) toast(t("common.saved"), "good");
    else geo = prev;
  }

  let retention = $state(untrack(() => String(store.data!.settings.retention_days ?? "")));
  let busy = $state(false);
  const max = $derived(data.limits.retention_days_max);
  const current = $derived(String(data.settings.retention_days ?? ""));
  const days = $derived(retention.trim() === "" ? null : Number(retention));
  const bad = $derived(days !== null && (!Number.isInteger(days) || days < 30 || days > max));
  const dirty = $derived(retention.trim() !== current);

  async function save() {
    busy = true;
    const ok = await store.patch({ retention_days: days });
    busy = false;
    if (ok) toast(t("common.saved"), "good");
  }
  function reset() {
    retention = current;
  }
</script>

<Card title={t("set.priv.title")}>
  {#if cap === null}
    <p class="muted">{t("set.priv.noDb")}</p>
  {:else}
    <Field layout="row" label={t("set.priv.geo")}>
      <Segmented label={t("set.priv.geo")} bind:value={geo} onchange={change} options={levels.map((l) => ({ value: l, label: t(`set.priv.geo.${l}`) }))} />
    </Field>
  {/if}
  <Field
    layout="row"
    label={t("set.priv.retention")}
    for="s-ret"
    hint={t("set.priv.retentionHint")}
    error={bad ? t("set.priv.retentionBad", { d: full(max, app.lang) }) : undefined}
  >
    <Input
      id="s-ret"
      type="text"
      inputmode="numeric"
      bind:value={retention}
      invalid={bad}
      placeholder={t("set.priv.retentionPlaceholder", { d: full(max, app.lang) })}
    >
      {#snippet suffix()}{t("set.priv.days")}{/snippet}
    </Input>
  </Field>
  <SaveBar {dirty} invalid={bad} {busy} onsave={save} onreset={reset} />
</Card>
