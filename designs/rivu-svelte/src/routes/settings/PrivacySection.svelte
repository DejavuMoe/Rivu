<!-- 隐私与保留：地理精度、原始数据保留天数，一组字段，有改动时出现「保存」。 -->
<script lang="ts">
  import { untrack } from "svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import { t } from "../../lib/i18n";
  import { toast } from "../../lib/state.svelte";
  import type { Site } from "../../lib/data/fixtures";
  import SaveBar from "./SaveBar.svelte";
  import { cfgs, INSTANCE_RETENTION } from "./store.svelte";
  import "./strings";

  let { site, readOnly = false }: { site: Site; readOnly?: boolean } = $props();
  const cfg = $derived(cfgs[site.id]);

  let geo = $state<"country" | "region" | "city">(untrack(() => cfgs[site.id].geo));
  let retention = $state(untrack(() => cfgs[site.id].retention));

  const n = $derived(retention.trim() === "" ? null : Number(retention));
  const bad = $derived(n !== null && (!Number.isInteger(n) || n < 1 || n > INSTANCE_RETENTION));
  const dirty = $derived(geo !== cfg.geo || retention.trim() !== cfg.retention);

  function save() {
    cfg.geo = geo;
    cfg.retention = retention.trim();
    toast(t("common.saved"), "good");
  }
  function reset() {
    geo = cfg.geo;
    retention = cfg.retention;
  }
</script>

<Card title={t("set.priv.title")}>
  <Field layout="row" label={t("set.priv.geo")}>
    <Segmented
      label={t("set.priv.geo")}
      bind:value={geo}
      options={[
        { value: "country", label: t("set.priv.geo.country"), disabled: readOnly },
        { value: "region", label: t("set.priv.geo.region"), disabled: readOnly },
        { value: "city", label: t("set.priv.geo.city"), disabled: readOnly },
      ]}
    />
  </Field>
  <Field
    layout="row"
    label={t("set.priv.retention")}
    for="s-ret"
    hint={t("set.priv.retentionHint", { d: INSTANCE_RETENTION })}
    error={bad ? t("set.priv.retentionBad", { d: INSTANCE_RETENTION }) : undefined}
  >
    <Input
      id="s-ret"
      type="text"
      inputmode="numeric"
      bind:value={retention}
      disabled={readOnly}
      invalid={bad}
      placeholder={t("set.priv.retentionPlaceholder", { d: INSTANCE_RETENTION })}
    >
      {#snippet suffix()}{t("set.priv.days")}{/snippet}
    </Input>
  </Field>
  <SaveBar {dirty} invalid={bad} {readOnly} onsave={save} onreset={reset} />
</Card>
