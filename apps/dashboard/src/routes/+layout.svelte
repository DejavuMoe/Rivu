<!-- 根布局：全局样式，主题与语言落到 <html>，轻提示。 -->
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
  import { app } from "#lib/state.svelte.ts";

  let { children } = $props();

  $effect(() => {
    const root = document.documentElement;
    if (app.theme === "system") root.removeAttribute("data-theme");
    else root.dataset.theme = app.theme;
    root.lang = app.lang === "zh" ? "zh-CN" : "en";
  });
</script>

{@render children()}
<Toaster />
