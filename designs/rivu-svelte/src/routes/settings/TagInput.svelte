<!--
  标签式输入：回车或逗号添加，退格删除最后一个。用于「允许的事件属性」。
  外观与 Input 一致（描边、聚焦环）；标签是中性灰底，不占品牌色。
-->
<script lang="ts">
  import { X } from "@lucide/svelte";
  import { t } from "../../lib/i18n";
  import "./strings";

  interface Props {
    tags: string[];
    id?: string;
    placeholder?: string;
    disabled?: boolean;
    max?: number;
    /** 返回错误文案则拒绝添加 */
    validate?: (tag: string, all: string[]) => string | null;
    label: string;
  }
  let { tags = $bindable(), id, placeholder, disabled = false, max = 30, validate, label }: Props = $props();

  let draft = $state("");
  let error = $state("");

  function commit() {
    const v = draft.trim().replace(/,$/, "").trim();
    if (!v) return;
    const err = validate?.(v, tags) ?? (tags.length >= max ? t("set.tag.max", { n: max }) : null);
    if (err) {
      error = err;
      return;
    }
    error = "";
    tags = [...tags, v];
    draft = "";
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === ",") {
      e.preventDefault();
      commit();
    } else if (e.key === "Backspace" && !draft && tags.length && !disabled) {
      tags = tags.slice(0, -1);
    }
  }
</script>

<div class="wrap">
  <div class="tags" class:disabled>
    {#each tags as tag (tag)}
      <span class="tag">
        <code>{tag}</code>
        {#if !disabled}
          <button type="button" aria-label="{t('common.remove')} {tag}" onclick={() => (tags = tags.filter((x) => x !== tag))}>
            <X size={12} strokeWidth={2} />
          </button>
        {/if}
      </span>
    {/each}
    <input
      {id}
      aria-label={label}
      {disabled}
      placeholder={tags.length ? "" : placeholder}
      bind:value={draft}
      {onkeydown}
      onblur={commit}
      oninput={() => (error = "")}
    />
  </div>
  {#if error}<p class="error" role="alert">{error}</p>{/if}
</div>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .tags {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px;
    min-height: var(--control-md);
    padding: 3px 6px;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-sm);
    background: var(--bg-app);
  }
  .tags:hover {
    border-color: var(--border-hover);
  }
  .tags:focus-within {
    border-color: var(--accent-focus);
    box-shadow: var(--focus-ring);
  }
  .tags.disabled {
    background: var(--bg-subtle);
  }
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    height: 22px;
    padding: 0 2px 0 7px;
    border-radius: var(--r-xs);
    background: var(--bg-element);
    font-size: var(--fs-sm);
  }
  .tag button {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: var(--r-xs);
    background: none;
    color: var(--text-muted);
  }
  .tag button:hover {
    color: var(--text);
  }
  input {
    flex: 1;
    min-width: 96px;
    height: 24px;
    padding: 0 4px;
    border: 0;
    outline: 0;
    background: transparent;
    box-shadow: none !important;
    font-family: var(--font-mono);
    font-size: var(--fs-md);
  }
  input::placeholder {
    color: var(--text-placeholder);
    font-family: var(--font-sans);
  }
  .error {
    color: var(--bad-text);
    font-size: var(--fs-sm);
  }
</style>
