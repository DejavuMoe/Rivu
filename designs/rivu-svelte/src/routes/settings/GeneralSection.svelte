<!-- 常规：名称、时区、每周起始日（有改动时出现「保存」）；公开看板开关（立即生效）。 -->
<script lang="ts">
  import { untrack } from "svelte";
  import TimezoneSelect from "../../lib/app/TimezoneSelect.svelte";
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

  let { site }: { site: Site } = $props();

  const cfg = $derived(cfgs[site.id]);

  let name = $state(untrack(() => site.name));
  let timezone = $state(untrack(() => site.timezone));
  let weekStart = $state(untrack(() => cfgs[site.id].weekStart));

  const days = ["1", "2", "3", "4", "5", "6", "0"];

  const nameBad = $derived(name.trim().length === 0);
  const dirty = $derived(name.trim() !== site.name || timezone !== site.timezone || weekStart !== cfg.weekStart);

  // 名称、时区、公开状态存在站点上：改了以后侧栏、页头等处跟着变
  function save() {
    site.name = name.trim();
    site.timezone = timezone;
    cfg.weekStart = weekStart;
    toast(t("common.saved"), "good");
  }
  function reset() {
    name = site.name;
    timezone = site.timezone;
    weekStart = cfg.weekStart;
  }
  function togglePublic(v: boolean) {
    site.isPublic = v;
    toast(v ? t("set.general.publicOn") : t("set.general.publicOff"), "good");
  }

  const publicUrl = $derived(`${INSTANCE}/public/${site.siteKey}`);
</script>

<Card title={t("set.general.title")}>
  <Field layout="row" label={t("set.general.key")}>
    <CopyField value={site.siteKey} label={t("set.general.key")} />
  </Field>
  <Field layout="row" label={t("set.general.name")} for="s-name" error={nameBad ? t("set.general.nameEmpty") : undefined}>
    <Input id="s-name" bind:value={name} maxlength={100} invalid={nameBad} />
  </Field>
  <Field layout="row" label={t("set.general.tz")} for="s-tz">
    <div class="tz"><TimezoneSelect id="s-tz" bind:value={timezone} /></div>
  </Field>
  <Field layout="row" label={t("set.general.week")}>
    <Select label={t("set.general.week")} block bind:value={weekStart} width={200} options={days.map((d) => ({ value: d, label: t(`set.day.${d}`) }))} />
  </Field>
  <SaveBar {dirty} invalid={nameBad} onsave={save} onreset={reset} />
</Card>

<Card>
  <Field layout="row" label={t("set.general.public")} hint={t("set.general.publicHint")} for="s-public">
    <div class="sw">
      <Switch id="s-public" label={t("set.general.public")} checked={site.isPublic} onchange={togglePublic} />
    </div>
  </Field>
  {#if site.isPublic}
    <Field layout="row" label={t("set.general.publicUrl")}>
      <CopyField value={publicUrl} label={t("set.general.publicUrl")} />
    </Field>
  {/if}
</Card>

<style>
  .tz {
    max-width: 300px;
  }
  /* 20px 高的开关与 14px 文字（行高 21）的首行居中对齐 */
  .sw {
    display: flex;
    padding: 6px 0;
  }
</style>
