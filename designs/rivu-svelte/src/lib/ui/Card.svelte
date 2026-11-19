<!--
  卡片：0.5px 描边、无阴影。header 区放标题和右侧动作（不放描述）；footer 区放保存按钮或必要的署名。
  flush 去掉内容区内边距（表格、列表直接贴边）。
-->
<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    title?: string;
    flush?: boolean;
    actions?: Snippet;
    header?: Snippet;
    footer?: Snippet;
    children?: Snippet;
    tone?: "default" | "danger";
    id?: string;
  }

  let { title, flush = false, actions, header, footer, children, tone = "default", id }: Props = $props();
</script>

<section class="card {tone}" {id} aria-label={title}>
  {#if header}
    {@render header()}
  {:else if title || actions}
    <header>
      <div class="titles">
        {#if title}<h2>{title}</h2>{/if}
      </div>
      {#if actions}<div class="actions">{@render actions()}</div>{/if}
    </header>
  {/if}
  {#if children}<div class="body" class:flush>{@render children()}</div>{/if}
  {#if footer}<footer>{@render footer()}</footer>{/if}
</section>

<style>
  .card {
    display: flex;
    flex-direction: column;
    min-width: 0;
    background: var(--bg-app);
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .danger {
    border-color: var(--bad-border);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
    min-height: 52px;
    padding: var(--sp-3) var(--sp-4) 0 var(--sp-5);
  }
  .titles {
    min-width: 0;
  }
  h2 {
    font-size: var(--fs-md);
    font-weight: var(--fw-semibold);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex: none;
  }
  .body {
    flex: 1;
    min-height: 0;
    padding: var(--sp-3) var(--sp-5) var(--sp-5);
  }
  .body.flush {
    padding: 0;
  }
  header + .body.flush {
    margin-top: var(--sp-3);
  }
  footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-4) var(--sp-3) var(--sp-5);
    border-top: var(--hairline) solid var(--border-subtle);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
</style>
