<!--
  地理与保留。地理精度：只列出已加载的地理库支持的级别（国家级库只能选国家，城市级库可选国家、地区、城市），
  没有地理库时不记录位置，只给一行说明；单选立即生效。原始数据保留天数是文本字段，有改动时才出现「保存」，
  留空 = 实例默认（也是上限，最少 30 天）。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import { full } from "../../lib/format";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import type { Site } from "../../lib/data/fixtures";
  import SaveBar from "./SaveBar.svelte";
  import { cfgs, geoCapability, INSTANCE_RETENTION, MIN_RETENTION } from "./store.svelte";
  import "./strings";

  type Level = "country" | "region" | "city";

  let { site }: { site: Site } = $props();
  const cfg = $derived(cfgs[site.id]);
  const cap = $derived(geoCapability(site));

  // 国家级库下，已存的城市 / 地区设置实际只能到国家
  let geo = $state<Level>(untrack(() => (geoCapability(site) === "country" ? "country" : cfgs[site.id].geo)));
  const levels = $derived<Level[]>(cap === "city" ? ["country", "region", "city"] : ["country"]);

  function change(v: Level) {
    cfg.geo = v;
    toast(t("common.saved"), "good");
  }

  let retention = $state(untrack(() => cfgs[site.id].retention));
  const days = $derived(retention.trim() === "" ? null : Number(retention));
  const bad = $derived(days !== null && (!Number.isInteger(days) || days < MIN_RETENTION || days > INSTANCE_RETENTION));
  const dirty = $derived(retention.trim() !== cfg.retention);

  function save() {
    cfg.retention = retention.trim();
    toast(t("common.saved"), "good");
  }
  function reset() {
    retention = cfg.retention;
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
    error={bad ? t("set.priv.retentionBad", { d: full(INSTANCE_RETENTION, app.lang) }) : undefined}
  >
    <Input
      id="s-ret"
      type="text"
      inputmode="numeric"
      bind:value={retention}
      invalid={bad}
      placeholder={t("set.priv.retentionPlaceholder", { d: full(INSTANCE_RETENTION, app.lang) })}
    >
      {#snippet suffix()}{t("set.priv.days")}{/snippet}
    </Input>
  </Field>
  <SaveBar {dirty} invalid={bad} onsave={save} onreset={reset} />
</Card>
