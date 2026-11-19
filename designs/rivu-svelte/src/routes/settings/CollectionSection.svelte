<!--
  数据采集：开关（启用采集、脚本功能）立即生效；每分钟上限与允许的事件属性是一组文本字段，有改动时才出现「保存」。
  上限留空 = 用实例默认值（placeholder 显示默认值）。脚本功能开了 Hash 路由以外的任一项，站点脚本就是 full 版。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Switch from "../../lib/ui/Switch.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { full } from "../../lib/format";
  import type { Site } from "../../lib/data/fixtures";
  import SaveBar from "./SaveBar.svelte";
  import TagInput from "./TagInput.svelte";
  import { cfgs, GLOBAL_RATE, RATE_MAX, type ScriptFeatures } from "./store.svelte";
  import "./strings";

  let { site }: { site: Site } = $props();
  const cfg = $derived(cfgs[site.id]);

  let rate = $state(untrack(() => cfgs[site.id].rateLimit));
  let evProps = $state<string[]>(untrack(() => [...cfgs[site.id].eventProps]));

  const rateNum = $derived(rate.trim() === "" ? null : Number(rate));
  const rateBad = $derived(rateNum !== null && (!Number.isInteger(rateNum) || rateNum < 1 || rateNum > RATE_MAX));
  const dirty = $derived(rate.trim() !== cfg.rateLimit || evProps.join("\n") !== cfg.eventProps.join("\n"));

  const features: (keyof ScriptFeatures)[] = ["outbound", "downloads", "forms", "hash", "tagged", "engagement"];

  function save() {
    cfg.rateLimit = rate.trim();
    cfg.eventProps = [...evProps];
    toast(t("common.saved"), "good");
  }
  function reset() {
    rate = cfg.rateLimit;
    evProps = [...cfg.eventProps];
  }

  function validateProp(tag: string, all: string[]) {
    if (!/^[A-Za-z0-9_.:-]+$/.test(tag)) return t("set.coll.propsChars");
    if (tag.length > 300) return t("set.coll.propsLen");
    if (all.includes(tag)) return t("set.coll.propsDup");
    return null;
  }
</script>

<Card title={t("set.coll.title")}>
  <Field layout="row" label={t("set.coll.enabled")} for="s-enabled">
    <div class="sw">
      <Switch
        id="s-enabled"
        label={t("set.coll.enabled")}
        checked={cfg.collectEnabled}
        onchange={(v) => {
          cfg.collectEnabled = v;
          toast(v ? t("set.coll.on") : t("set.coll.off"), "good");
        }}
      />
    </div>
    {#if !cfg.collectEnabled}
      <Callout tone="warn">{t("set.coll.offWarn")}</Callout>
    {/if}
  </Field>
</Card>

<Card title={t("set.coll.script")}>
  {#each features as f (f)}
    <Field layout="row" label={t(`set.coll.script.${f}`)} for="s-f-{f}">
      <div class="sw">
        <Switch
          id="s-f-{f}"
          label={t(`set.coll.script.${f}`)}
          checked={cfg.script[f]}
          onchange={(v) => {
            cfg.script[f] = v;
            toast(t("common.saved"), "good");
          }}
        />
      </div>
    </Field>
  {/each}
</Card>

<Card title={t("set.coll.limits")}>
  <Field layout="row" label={t("set.coll.rate")} for="s-rate" error={rateBad ? t("set.coll.rateBad", { max: full(RATE_MAX, app.lang) }) : undefined}>
    <Input
      id="s-rate"
      type="text"
      inputmode="numeric"
      bind:value={rate}
      invalid={rateBad}
      placeholder={t("set.coll.ratePlaceholder", { n: full(GLOBAL_RATE, app.lang) })}
    >
      {#snippet suffix()}{t("set.coll.perMin")}{/snippet}
    </Input>
  </Field>
  <Field layout="row" label={t("set.coll.props")} for="s-props" hint={t("set.coll.propsHint")}>
    <TagInput id="s-props" label={t("set.coll.props")} bind:tags={evProps} placeholder={t("set.coll.propsPlaceholder")} validate={validateProp} />
  </Field>
  <SaveBar {dirty} invalid={rateBad} onsave={save} onreset={reset} />
</Card>

<style>
  /* 20px 高的开关与 14px 文字（行高 21）的首行居中对齐 */
  .sw {
    display: flex;
    padding: 6px 0;
  }
</style>
