<!-- 常规：名称、时区、每周起始日（有改动时出现「保存」）；公开看板开关（立即生效）。 -->
<script lang="ts">
  import { untrack } from "svelte";
  import TimezoneSelect from "#lib/app/TimezoneSelect.svelte";
  import Card from "#lib/ui/Card.svelte";
  import CopyField from "#lib/ui/CopyField.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Select from "#lib/ui/Select.svelte";
  import Switch from "#lib/ui/Switch.svelte";
  import type { SitePatch } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { toast } from "#lib/state.svelte.ts";
  import SaveBar from "./SaveBar.svelte";
  import { useSettings } from "./settings.svelte.ts";
  import "./strings";

  const getStore = useSettings();
  const store = $derived(getStore());
  const data = $derived(store.data!);

  let name = $state(untrack(() => store.data!.site.name));
  let timezone = $state(untrack(() => store.data!.site.timezone));
  let week = $state(untrack(() => String(store.data!.settings.week_starts_on)));
  let isPublic = $state(untrack(() => store.data!.site.is_public));
  let busy = $state(false);

  const days = ["1", "2", "3", "4", "5", "6", "0"];
  const nameBad = $derived(name.trim().length === 0);
  const dirty = $derived(
    name.trim() !== data.site.name || timezone !== data.site.timezone || week !== String(data.settings.week_starts_on),
  );
  const publicUrl = $derived(`${location.origin}/public/${data.site.site_key}`);

  async function save() {
    const patch: SitePatch = {};
    if (name.trim() !== data.site.name) patch.name = name.trim();
    if (timezone !== data.site.timezone) patch.timezone = timezone;
    if (week !== String(data.settings.week_starts_on)) patch.week_starts_on = Number(week);
    busy = true;
    const ok = await store.patch(patch);
    busy = false;
    if (ok) toast(t("common.saved"), "good");
  }
  function reset() {
    name = data.site.name;
    timezone = data.site.timezone;
    week = String(data.settings.week_starts_on);
  }
  async function togglePublic(v: boolean) {
    if (await store.patch({ is_public: v })) toast(v ? t("set.general.publicOn") : t("set.general.publicOff"), "good");
    else isPublic = !v;
  }
</script>

<Card title={t("set.general.title")}>
  <Field layout="row" label={t("set.general.key")}>
    <CopyField value={data.site.site_key} label={t("set.general.key")} />
  </Field>
  <Field layout="row" label={t("set.general.name")} for="s-name" error={nameBad ? t("set.general.nameEmpty") : undefined}>
    <Input id="s-name" bind:value={name} maxlength={100} invalid={nameBad} />
  </Field>
  <Field layout="row" label={t("set.general.tz")} for="s-tz">
    <div class="tz"><TimezoneSelect id="s-tz" bind:value={timezone} /></div>
  </Field>
  <Field layout="row" label={t("set.general.week")}>
    <Select label={t("set.general.week")} block bind:value={week} width={200} options={days.map((d) => ({ value: d, label: t(`set.day.${d}`) }))} />
  </Field>
  <SaveBar {dirty} invalid={nameBad} {busy} onsave={save} onreset={reset} />
</Card>

<Card>
  <Field layout="row" label={t("set.general.public")} hint={t("set.general.publicHint")} for="s-public">
    <div class="sw">
      <Switch id="s-public" label={t("set.general.public")} bind:checked={isPublic} onchange={togglePublic} />
    </div>
  </Field>
  {#if isPublic}
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
