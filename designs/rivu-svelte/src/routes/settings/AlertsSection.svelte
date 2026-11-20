<!--
  通知：流量突增的提醒人数（site_settings 里的一个数字，默认空 = 不提醒），有改动时才出现「保存」。
  当前访客（最近 5 分钟的独立访客，同看板的「当前在线」）超过这个人数时，给在账号页订阅了这个站点「流量突增」的人各发一封邮件；
  一次突增只发一封，当前访客回落到人数以下满 30 分钟后才会再发下一封。
  只有组织的 owner / admin 看得到这个分区；实例没配置邮件时整个分区不显示（见 lib/data/sections.ts）。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import { full } from "../../lib/format";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { href } from "../../lib/router.svelte";
  import type { Site } from "../../lib/data/fixtures";
  import SaveBar from "./SaveBar.svelte";
  import { cfgs, SPIKE_MAX } from "./store.svelte";
  import "./strings";

  let { site }: { site: Site } = $props();
  const cfg = $derived(cfgs[site.id]);

  let value = $state(untrack(() => cfgs[site.id].spikeThreshold));
  const num = $derived(value.trim() === "" ? null : Number(value));
  const bad = $derived(num !== null && (!Number.isInteger(num) || num < 1 || num > SPIKE_MAX));
  const dirty = $derived(value.trim() !== cfg.spikeThreshold);

  function save() {
    cfg.spikeThreshold = value.trim();
    toast(t(cfg.spikeThreshold ? "set.alerts.on" : "set.alerts.off", { n: cfg.spikeThreshold }), "good");
  }
  function reset() {
    value = cfg.spikeThreshold;
  }
</script>

<Card title={t("set.alerts.title")}>
  <Field
    layout="row"
    label={t("set.alerts.threshold")}
    for="s-spike"
    hint={t("set.alerts.hint")}
    error={bad ? t("set.alerts.bad", { max: full(SPIKE_MAX, app.lang) }) : undefined}
  >
    <Input id="s-spike" type="text" inputmode="numeric" bind:value invalid={bad} placeholder={t("set.alerts.placeholder")}>
      {#snippet suffix()}{t("set.alerts.unit")}{/snippet}
    </Input>
  </Field>
  <Field layout="row" label={t("set.alerts.recipients")}>
    <p class="val">
      {t("set.alerts.recipientsBody")}
      <a class="link" href={href("/account")}>{t("set.alerts.toAccount")}</a>
    </p>
  </Field>
  <SaveBar {dirty} invalid={bad} onsave={save} onreset={reset} />
</Card>

<style>
  /* 与行布局里标签的 5px 上内边距对齐 */
  .val {
    padding-top: 5px;
    color: var(--text-muted);
  }
  .link {
    color: var(--accent-text);
  }
  .link:hover {
    text-decoration: underline;
  }
</style>
