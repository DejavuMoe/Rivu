<!-- 常规：名称、时区、每周起始日（保存后生效）；公开看板开关（立即生效）。 -->
<script lang="ts">
  import { untrack } from "svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import CopyField from "../../lib/ui/CopyField.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Select from "../../lib/ui/Select.svelte";
  import Switch from "../../lib/ui/Switch.svelte";
  import { t } from "../../lib/i18n";
  import { toast } from "../../lib/state.svelte";
  import type { Site } from "../../lib/data/fixtures";
  import SaveBar from "./SaveBar.svelte";
  import { cfgs, INSTANCE } from "./store.svelte";
  import "./strings";

  let { site, readOnly = false }: { site: Site; readOnly?: boolean } = $props();

  const cfg = $derived(cfgs[site.id]);

  let name = $state(untrack(() => cfgs[site.id].name));
  let timezone = $state(untrack(() => cfgs[site.id].timezone));
  let weekStart = $state(untrack(() => cfgs[site.id].weekStart));

  const ZONES = [
    "UTC",
    "Asia/Shanghai",
    "Asia/Hong_Kong",
    "Asia/Tokyo",
    "Asia/Singapore",
    "Asia/Kolkata",
    "Europe/London",
    "Europe/Berlin",
    "Europe/Moscow",
    "America/New_York",
    "America/Chicago",
    "America/Los_Angeles",
    "Australia/Sydney",
  ];
  const zoneOptions = $derived((ZONES.includes(cfg.timezone) ? ZONES : [cfg.timezone, ...ZONES]).map((z) => ({ value: z, label: z })));
  const days = ["1", "2", "3", "4", "5", "6", "0"];

  const nameBad = $derived(name.trim().length === 0);
  const tzChanged = $derived(timezone !== cfg.timezone);
  const dirty = $derived(name.trim() !== cfg.name || tzChanged || weekStart !== cfg.weekStart);

  function save() {
    cfg.name = name.trim();
    cfg.weekStart = weekStart;
    const rebuild = tzChanged;
    cfg.timezone = timezone;
    toast(rebuild ? t("set.general.savedRebuild") : t("common.saved"), "good");
  }
  function reset() {
    name = cfg.name;
    timezone = cfg.timezone;
    weekStart = cfg.weekStart;
  }

  const publicUrl = $derived(`${INSTANCE}/share/public/${site.siteKey}`);
</script>

<Card title={t("set.general.title")}>
  <Field layout="row" label={t("set.general.key")}>
    <CopyField value={site.siteKey} label={t("set.general.key")} />
  </Field>
  <Field layout="row" label={t("set.general.name")} for="s-name" error={nameBad ? t("set.general.nameEmpty") : undefined}>
    <Input id="s-name" bind:value={name} disabled={readOnly} invalid={nameBad} />
  </Field>
  <Field layout="row" label={t("set.general.tz")}>
    <Select label={t("set.general.tz")} block width={300} bind:value={timezone} options={zoneOptions} />
    {#if tzChanged}
      <Callout tone="warn">{t("set.general.tzRebuild")}</Callout>
    {/if}
  </Field>
  <Field layout="row" label={t("set.general.week")}>
    <Select
      label={t("set.general.week")}
      block
      bind:value={weekStart}
      width={200}
      options={days.map((d) => ({ value: d, label: t(`set.day.${d}`) }))}
    />
  </Field>
  <SaveBar {dirty} invalid={nameBad} {readOnly} onsave={save} onreset={reset} />
</Card>

<Card>
  <Field layout="row" label={t("set.general.public")} hint={t("set.general.publicHint")} for="s-public">
    <div class="sw">
      <Switch
        id="s-public"
        label={t("set.general.public")}
        checked={cfg.isPublic}
        disabled={readOnly}
        onchange={(v) => {
          cfg.isPublic = v;
          toast(v ? t("set.general.publicOn") : t("set.general.publicOff"), "good");
        }}
      />
    </div>
  </Field>
  {#if cfg.isPublic}
    <Field layout="row" label={t("set.general.publicUrl")}>
      <CopyField value={publicUrl} label={t("set.general.publicUrl")} />
    </Field>
  {/if}
</Card>

<style>
  /* 20px 高的开关与 14px 文字（行高 21）的首行居中对齐 */
  .sw {
    display: flex;
    padding: 6px 0;
  }
</style>
