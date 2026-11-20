<!--
  会落定的数字：第一次出现时从 0 数到目标值，之后数值变了就从旧值数到新值。
  只用于一页里少数几个重要的数（指标数值、此刻在线）。系统要求减少动效时直接显示目标值。
  units：把数字里的中文单位（万、分、秒）排小一号，数字本身才是主角。
-->
<script lang="ts">
  import { Tween, prefersReducedMotion } from "svelte/motion";
  import { cubicOut } from "svelte/easing";

  interface Props {
    value: number;
    format: (n: number) => string;
    /** 毫秒；默认与 --dur-arrive 一致 */
    duration?: number;
    units?: boolean;
  }
  let { value, format, duration = 700, units = false }: Props = $props();

  const tween = new Tween(0, { easing: cubicOut });
  $effect(() => {
    tween.set(value, { duration: prefersReducedMotion.current ? 0 : duration });
  });

  const text = $derived(format(tween.current));
  /** 拆成「数字与拉丁字符」和「中文单位」两种片段 */
  const parts = $derived(
    units
      ? (text.match(/[一-鿿]+|[^一-鿿]+/g) ?? []).map((s) => ({ s: s.trim(), unit: /[一-鿿]/.test(s) })).filter((p) => p.s)
      : [],
  );
</script>

{#if units}{#each parts as p, i (i)}<span class:unit={p.unit} class:num={!p.unit}>{p.s}</span>{/each}{:else}{text}{/if}

<style>
  .unit {
    margin: 0 0.18em 0 0.12em;
    font-family: var(--font-sans);
    font-size: var(--fs-md);
    font-weight: var(--fw-medium);
    letter-spacing: 0;
  }
  .unit:last-child {
    margin-right: 0;
  }
</style>
