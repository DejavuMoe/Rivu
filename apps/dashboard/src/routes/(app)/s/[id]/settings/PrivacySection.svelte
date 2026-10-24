<!--
  隐私：地理精度。只列出已加载的地理库支持的级别（国家级库只能选国家，城市级库可选国家、地区、城市）；
  没有地理库时不记录位置，只给一行说明。单选立即生效。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import Card from "#lib/ui/Card.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Segmented from "#lib/ui/Segmented.svelte";
  import { t } from "#lib/i18n.ts";
  import { toast } from "#lib/state.svelte.ts";
  import { useSettings } from "./settings.svelte.ts";
  import "./strings";

  type Level = "country" | "region" | "city";

  const getStore = useSettings();
  const store = $derived(getStore());
  const cap = $derived(store.data!.geo_capability);

  // 国家级库下，已存的城市 / 地区设置实际只能到国家
  let geo = $state<Level>(untrack(() => (store.data!.geo_capability === "country" ? "country" : store.data!.settings.geo_level)));
  const levels = $derived<Level[]>(cap === "city" ? ["country", "region", "city"] : ["country"]);

  async function change(v: Level) {
    const prev = store.data!.settings.geo_level;
    if (await store.patch({ geo_level: v })) toast(t("common.saved"), "good");
    else geo = prev;
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
</Card>
