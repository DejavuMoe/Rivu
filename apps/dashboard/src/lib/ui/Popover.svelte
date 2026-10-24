<!--
  浮层：触发器 + 内容。点外面、按 Esc 关闭并把焦点还给触发器。菜单、下拉选择、日期范围都基于它。
  trigger 片段拿到 { open, toggle, props }，props 要展开到触发按钮上（aria 属性）。
-->
<script lang="ts">
  import type { Snippet } from "svelte";

  interface TriggerArgs {
    open: boolean;
    toggle: () => void;
    props: Record<string, string | boolean>;
  }

  interface Props {
    trigger: Snippet<[TriggerArgs]>;
    children: Snippet<[{ close: () => void }]>;
    align?: "start" | "end";
    side?: "bottom" | "top";
    width?: number | "trigger";
    /** 内容区的 ARIA 角色：menu / listbox / dialog */
    role?: "menu" | "listbox" | "dialog";
    label?: string;
    open?: boolean;
  }

  let {
    trigger,
    children,
    align = "start",
    side = "bottom",
    width = 240,
    role = "menu",
    label,
    open = $bindable(false),
  }: Props = $props();

  let root: HTMLDivElement;
  let panel: HTMLDivElement | undefined = $state();
  const id = `pop-${Math.random().toString(36).slice(2, 8)}`;

  function toggle() {
    open = !open;
  }
  function close() {
    open = false;
    (root.querySelector("[aria-haspopup]") as HTMLElement | null)?.focus();
  }

  $effect(() => {
    if (!open) return;
    // 打开后把焦点放到第一个可选项上，方向键在项之间移动
    queueMicrotask(() => {
      const first = panel?.querySelector<HTMLElement>(
        "[data-autofocus], [role=menuitem]:not([aria-disabled=true]), [role=option], [role=menuitemradio], input, button",
      );
      first?.focus();
    });
    const onDown = (e: PointerEvent) => {
      if (!root.contains(e.target as Node)) open = false;
    };
    document.addEventListener("pointerdown", onDown, true);
    return () => document.removeEventListener("pointerdown", onDown, true);
  });

  function onKeydown(e: KeyboardEvent) {
    if (!open) return;
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      const items = [
        ...(panel?.querySelectorAll<HTMLElement>("[role=menuitem], [role=menuitemradio], [role=option]") ?? []),
      ].filter((el) => el.getAttribute("aria-disabled") !== "true");
      if (!items.length) return;
      e.preventDefault();
      const i = items.indexOf(document.activeElement as HTMLElement);
      const next = e.key === "ArrowDown" ? (i + 1) % items.length : (i - 1 + items.length) % items.length;
      items[next].focus();
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="popover" bind:this={root} onkeydown={onKeydown}>
  {@render trigger({
    open,
    toggle,
    props: { "aria-haspopup": role, "aria-expanded": open, "aria-controls": id },
  })}
  {#if open}
    <div
      bind:this={panel}
      {id}
      class="panel {align} {side}"
      class:match={width === "trigger"}
      style:width={typeof width === "number" ? `${width}px` : undefined}
      role={role === "dialog" ? "dialog" : undefined}
      aria-label={label}
    >
      <div role={role === "dialog" ? undefined : role} aria-label={label}>
        {@render children({ close })}
      </div>
    </div>
  {/if}
</div>

<style>
  .popover {
    position: relative;
    display: inline-flex;
  }
  .panel {
    position: absolute;
    z-index: 60;
    min-width: 180px;
    max-height: min(480px, 70vh);
    overflow: auto;
    padding: 4px;
    background: var(--bg-app);
    border-radius: var(--r-md);
    border: var(--hairline) solid var(--border-subtle);
    box-shadow: var(--shadow-pop);
  }
  .bottom {
    top: calc(100% + 6px);
  }
  .top {
    bottom: calc(100% + 6px);
  }
  .start {
    left: 0;
  }
  .end {
    right: 0;
  }
  .match {
    width: 100%;
  }
</style>
