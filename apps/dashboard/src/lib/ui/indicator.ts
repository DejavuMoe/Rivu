// 选中指示块：标签栏的下划线、分段控件的滑块、设置分区的底色都是同一个绝对定位元素，
// 选中项变化时它从旧位置滑到新位置（只动 transform 与 width）。
// 容器要 position: relative；指示块要在选项之下（选项自己 position: relative）。
//
// 换页时页头会重新创建：传入 memory 键后，记住上一次的位置，新的指示块先放在旧位置再滑到新位置，
// 所以「看板 → 接入诊断」这种跨页面的切换也能看到下划线移动。没有记忆（第一次打开）时直接就位，不做动画。

interface Pos {
  x: number;
  y: number;
  w: number;
  h: number;
}

const memory = new Map<string, Pos>();

/**
 * 让 `ind` 跟随 `container` 里匹配 `selector` 的元素。返回清理函数，在 $effect 里调用：
 *   $effect(() => trackIndicator(box, ind, '[aria-selected="true"]'));
 * mode 为 "underline" 时只跟随横向位置与宽度（高度与纵向位置由 CSS 决定）。
 */
export function trackIndicator(
  container: HTMLElement,
  ind: HTMLElement,
  selector: string,
  mode: "fill" | "underline" = "fill",
  memoryKey?: string,
): () => void {
  let first = true;

  function apply(p: Pos) {
    ind.style.width = `${p.w}px`;
    if (mode === "fill") {
      ind.style.height = `${p.h}px`;
      ind.style.transform = `translate(${p.x}px, ${p.y}px)`;
    } else {
      ind.style.transform = `translateX(${p.x}px)`;
    }
  }

  function place() {
    const el = container.querySelector<HTMLElement>(selector);
    if (!el) {
      ind.style.opacity = "0";
      return;
    }
    ind.style.opacity = "1";
    const next = { x: el.offsetLeft, y: el.offsetTop, w: el.offsetWidth, h: el.offsetHeight };
    if (first) {
      first = false;
      const prev = memoryKey ? memory.get(memoryKey) : undefined;
      // 先无动画地放到起点（上次的位置，没有就是终点），再打开过渡移到终点
      ind.style.transition = "none";
      apply(prev ?? next);
      void ind.offsetWidth;
      ind.style.transition = "";
      if (prev) requestAnimationFrame(() => apply(next));
    } else {
      apply(next);
    }
    if (memoryKey) memory.set(memoryKey, next);
  }

  const mo = new MutationObserver(place);
  mo.observe(container, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ["aria-selected", "aria-checked", "aria-current"],
  });
  const ro = new ResizeObserver(place);
  ro.observe(container);
  // 字体加载完成会改变文字宽度
  document.fonts?.ready.then(place);
  place();

  return () => {
    mo.disconnect();
    ro.disconnect();
  };
}
