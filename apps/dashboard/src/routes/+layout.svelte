<!-- 根布局：全局样式，主题与语言落到 <html>，轻提示，换页的淡入淡出。 -->
<script lang="ts">
  // 只打包拉丁子集：中文走系统字体（苹方 / 微软雅黑 / 思源黑体），见 tokens.css 的 --font-sans。
  import "@fontsource/ibm-plex-sans/latin-400.css";
  import "@fontsource/ibm-plex-sans/latin-500.css";
  import "@fontsource/ibm-plex-sans/latin-600.css";
  import "@fontsource/ibm-plex-mono/latin-400.css";
  import "@fontsource/ibm-plex-mono/latin-500.css";
  import "#lib/styles/radix.css";
  import "#lib/styles/tokens.css";
  import "#lib/styles/base.css";
  import Toaster from "#lib/ui/Toaster.svelte";
  import { onNavigate } from "$app/navigation";
  import { app } from "#lib/state.svelte.ts";

  let { children } = $props();

  // 换页（路径变化）时用 View Transitions 做一次很短的淡入淡出（时长与页头的排除规则见 base.css、PageHeader）；
  // 只改查询参数（筛选、范围）时不做，数据就地刷新。浏览器不支持或要求减少动效时直接切换。
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");
  onNavigate((nav) => {
    if (!document.startViewTransition || reduceMotion.matches) return;
    if (!nav.to || nav.from?.url.pathname === nav.to.url.pathname) return;
    return new Promise((resolve) => {
      document.startViewTransition(async () => {
        resolve();
        await nav.complete;
      });
    });
  });

  $effect(() => {
    const root = document.documentElement;
    if (app.theme === "system") root.removeAttribute("data-theme");
    else root.dataset.theme = app.theme;
    root.lang = app.lang === "zh" ? "zh-CN" : "en";
  });
</script>

{@render children()}
<Toaster />
