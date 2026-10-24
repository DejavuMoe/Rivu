<!--
  头像 / 站点标记：取名字首字，中性灰底。shape="square" 用于站点与组织，"circle" 用于人。
  字号只在 12 与 14 两档里取，跟随尺寸。
-->
<script lang="ts">
  interface Props {
    name: string;
    size?: number;
    shape?: "circle" | "square";
  }
  let { name, size = 24, shape = "circle" }: Props = $props();

  const initial = $derived([...name.replace(/^(https?:\/\/)?(www\.)?/, "")][0]?.toUpperCase() ?? "?");
</script>

<span
  class="av {shape}"
  class:large={size >= 28}
  style:width="{size}px"
  style:height="{size}px"
  aria-hidden="true">{initial}</span
>

<style>
  .av {
    display: inline-grid;
    place-items: center;
    flex: none;
    background: var(--bg-active);
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-weight: var(--fw-semibold);
    line-height: 1;
    user-select: none;
  }
  .large {
    font-size: var(--fs-md);
  }
  .circle {
    border-radius: 50%;
  }
  .square {
    border-radius: var(--r-xs);
  }
</style>
