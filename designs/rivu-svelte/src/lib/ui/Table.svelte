<!--
  数据表：表头灰底、行间细线、数字列右对齐等宽数字。单元格内容由 cell 片段渲染。
  列宽可选；sticky 表头用于对话框里的长表。
  fit：窄屏改用固定表格布局，按列宽分配，长内容在单元格里省略，不出现横向滚动（成员这类有固定宽度控件的表）。
-->
<script lang="ts" generics="R">
  import type { Snippet } from "svelte";
  import type { Column } from "./types";

  interface Props {
    columns: Column[];
    rows: R[];
    cell: Snippet<[R, Column, number]>;
    rowKey?: (r: R, i: number) => string | number;
    caption?: string;
    sticky?: boolean;
    empty?: Snippet;
    onrowclick?: (r: R) => void;
    fit?: boolean;
  }

  let { columns, rows, cell, rowKey = (_r, i) => i, caption, sticky = false, empty, onrowclick, fit = false }: Props = $props();
</script>

<div class="wrap">
  <table class:sticky class:fit>
    {#if caption}<caption class="sr-only">{caption}</caption>{/if}
    <thead>
      <tr>
        {#each columns as c (c.key)}
          <th scope="col" class={c.align ?? "left"} class:optional={c.optional} style:width={c.width}>{c.label}</th>
        {/each}
      </tr>
    </thead>
    <tbody>
      {#each rows as r, i (rowKey(r, i))}
        <tr class:clickable={!!onrowclick} onclick={onrowclick ? () => onrowclick(r) : undefined}>
          {#each columns as c (c.key)}
            <td class={c.align ?? "left"} class:optional={c.optional}>{@render cell(r, c, i)}</td>
          {/each}
        </tr>
      {/each}
    </tbody>
  </table>
  {#if rows.length === 0 && empty}{@render empty()}{/if}
</div>

<style>
  .wrap {
    width: 100%;
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: separate;
    border-spacing: 0;
  }
  th {
    height: 36px;
    padding: 0 var(--sp-4);
    border-bottom: var(--hairline) solid var(--border-subtle);
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
    white-space: nowrap;
  }
  /* 首尾列与卡片标题对齐 */
  th:first-child,
  td:first-child {
    padding-left: var(--sp-5);
  }
  th:last-child,
  td:last-child {
    padding-right: var(--sp-5);
  }
  .sticky th {
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--bg-app);
  }
  td {
    height: 40px;
    padding: 0 var(--sp-4);
    border-bottom: var(--hairline) solid var(--border-subtle);
    vertical-align: middle;
    transition: background-color var(--dur-fast) var(--ease);
  }
  tbody tr:last-child td {
    border-bottom: 0;
  }
  tbody tr:hover td {
    background: var(--bg-subtle);
  }
  .clickable {
    cursor: pointer;
  }
  .left {
    text-align: left;
  }
  .right {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .center {
    text-align: center;
  }
  @media (max-width: 720px) {
    .optional {
      display: none;
    }
    th,
    td {
      padding: 0 var(--sp-2);
    }
    th:first-child,
    td:first-child {
      padding-left: var(--sp-4);
    }
    th:last-child,
    td:last-child {
      padding-right: var(--sp-4);
    }
    .fit {
      table-layout: fixed;
    }
    .fit td {
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
  }
</style>
