<!--
  添加 / 修改筛选：选维度 → 选条件 → 勾选值（带搜索，也可以直接输入）。
  一个维度只有一条筛选；多个值之间是「或」。可选的值来自当前范围内该维度的排行（不含这个维度自己的筛选）。
-->
<script lang="ts">
  import { Search } from "@lucide/svelte";
  import Dialog from "../ui/Dialog.svelte";
  import Button from "../ui/Button.svelte";
  import Select from "../ui/Select.svelte";
  import Segmented from "../ui/Segmented.svelte";
  import Input from "../ui/Input.svelte";
  import Checkbox from "../ui/Checkbox.svelte";
  import Field from "../ui/Field.svelte";
  import { t } from "../i18n";
  import { app } from "../state.svelte";
  import type { Filter, Op } from "./spec";
  import { isAbort } from "./source";
  import { displayValue } from "./values";

  interface Props {
    open: boolean;
    initial: Filter | null;
    dims: string[];
    suggest: (dim: string, signal: AbortSignal) => Promise<string[]>;
    onsave: (f: Filter) => void;
  }
  let { open = $bindable(), initial, dims, suggest, onsave }: Props = $props();

  let dim = $state("country");
  let op = $state<Op>("is");
  let picked: string[] = $state([]);
  let query = $state("");
  let options: string[] = $state([]);
  let loading = $state(false);

  $effect(() => {
    if (open) {
      dim = initial?.dim ?? (dims.includes("country") ? "country" : dims[0]);
      op = initial?.op ?? "is";
      picked = [...(initial?.values ?? [])];
      query = "";
    }
  });

  $effect(() => {
    if (!open) return;
    const ctl = new AbortController();
    loading = true;
    options = [];
    suggest(dim, ctl.signal).then(
      (vs) => {
        options = vs;
        loading = false;
      },
      (e) => {
        if (!isAbort(e)) loading = false;
      },
    );
    return () => ctl.abort();
  });

  const isMatch = $derived(op === "contains" || op === "contains_not");
  const label = (v: string) => displayValue(dim, v, app.lang);
  // 已选但不在排行里的值（比如修改筛选时）也要列出来
  const all = $derived([...picked.filter((v) => !options.includes(v)), ...options]);
  const suggestions = $derived(
    all.filter((v) => {
      const s = query.toLowerCase();
      return v.toLowerCase().includes(s) || label(v).toLowerCase().includes(s);
    }),
  );
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
          options={dims.map((d) => ({ value: d, label: t(`fdim.${d}`) }))}
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
      <div class="values" role="group" aria-label={t("filter.value")} aria-busy={loading}>
        {#each suggestions as v (v)}
          <div class="val">
            <Checkbox checked={picked.includes(v)} onchange={(on) => toggle(v, on)}>{label(v)}</Checkbox>
          </div>
        {:else}
          {#if !loading}<p class="none">{t("filter.noMatch")}</p>{/if}
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
    min-height: 48px;
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
