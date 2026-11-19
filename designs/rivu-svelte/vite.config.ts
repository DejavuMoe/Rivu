import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { viteSingleFile } from "vite-plugin-singlefile";

// 原型构建成单个 HTML（字体、脚本、样式全部内联），便于离线评审和发布预览。
// 正式的 apps/dashboard 会换成 SvelteKit + adapter-static，不沿用这个配置。
export default defineConfig({
  plugins: [svelte(), viteSingleFile()],
  build: {
    target: "es2022",
    assetsInlineLimit: 100_000_000,
  },
});
