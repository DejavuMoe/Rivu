<!--
  添加 / 修改筛选：选维度 → 选条件 → 勾选值（带搜索，也可以直接输入）。
  一个维度只有一条筛选；多个值之间是「或」。
-->
<script lang="ts">
  import { Search } from "@lucide/svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Select from "../../lib/ui/Select.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Checkbox from "../../lib/ui/Checkbox.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import { t } from "../../lib/i18n";
  import { dimensionValues } from "../../lib/data/fixtures";
  import type { Filter, Op } from "./query";

  interface Props {
    open: boolean;
    initial: Filter | null;
    onsave: (f: Filter) => void;
  }
  let { open = $bindable(), initial, onsave }: Props = $props();

  const DIMS = [
    "page",
    "entry_page",
    "exit_page",
    "source",
    "channel",
    "referrer",
    "utm_campaign",
    "country",
    "region",
    "city",
    "device",
    "browser",
    "os",
    "goal",
  ];

  let dim = $state("country");
  let op = $state<Op>("is");
  let picked: string[] = $state([]);
  let query = $state("");

  $effect(() => {
    if (open) {
      dim = initial?.dim ?? "country";
      op = initial?.op ?? "is";
      picked = [...(initial?.values ?? [])];
      query = "";
    }
  });

  const isMatch = $derived(op === "contains" || op === "contains_not");
  const suggestions = $derived(dimensionValues(dim).filter((v) => v.toLowerCase().includes(query.toLowerCase())));
  const values = $derived(isMatch ? (query.trim() ? [query.trim()] : []) : picked);

  function toggle(v: string, on: boolean) {
    picked = on ? [...picked, v] : picked.filter((x) => x !== v);
  }

  function apply() {
    onsave({ dim, op, values });
    open = false;
  }
</script>

<Dialog bind:open title={initial ? t("filter.edit") : t("filter.title")} size="md">
  <div class="form">
    <div class="row2">
      <Field label={t("filter.dimension")}>
        <Select
          block
          label={t("filter.dimension")}
          value={dim}
          width={260}
          onchange={(v) => {
            dim = v;
            picked = [];
          }}
          options={DIMS.map((d) => ({ value: d, label: t(`fdim.${d}`) }))}
        />
      </Field>
      <Field label={t("filter.operator")}>
        <Segmented
          block
          label={t("filter.operator")}
          bind:value={op}
          options={[
            { value: "is", label: t("op.is") },
            { value: "is_not", label: t("op.is_not") },
            { value: "contains", label: t("op.contains") },
          ]}
        />
      </Field>
    </div>

    <Field label={t("filter.value")}>
      <Input icon={Search} bind:value={query} placeholder={t("filter.valuePlaceholder")} data-autofocus />
    </Field>

    {#if !isMatch}
      <div class="values" role="group" aria-label={t("filter.value")}>
        {#each suggestions as v (v)}
          <div class="val">
            <Checkbox checked={picked.includes(v)} onchange={(on) => toggle(v, on)}>{v}</Checkbox>
          </div>
        {:else}
          <p class="none">{t("filter.noMatch")}</p>
        {/each}
      </div>
    {/if}
  </div>

  {#snippet footer()}
    <Button onclick={() => (open = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" disabled={!values.length} onclick={apply}>{t("common.apply")}</Button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .row2 {
    display: grid;
    grid-template-columns: 1fr 1.2fr;
    gap: var(--sp-3);
  }
  .values {
    max-height: 240px;
    overflow: auto;
    margin-top: -4px;
    padding: 4px;
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .val {
    padding: 6px 8px;
    border-radius: var(--r-xs);
  }
  .val:hover {
    background: var(--bg-subtle);
  }
  .none {
    padding: 12px;
    color: var(--text-muted);
    font-size: var(--fs-md);
  }
  @media (max-width: 560px) {
    .row2 {
      grid-template-columns: 1fr;
    }
  }
</style>
