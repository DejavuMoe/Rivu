import { readFileSync } from "node:fs";
import adapter from "@sveltejs/adapter-static";
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vite";

const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"));

// 开发时把 /api 转给本地的 rivu（默认 http://127.0.0.1:8080，可用 RIVU_DEV_API 改）。
// 内部端点的 CSRF 检查要求 Origin 与 RIVU_BASE_URL 同源，所以代理把 Origin 改写成上游地址；
// 本地 rivu 的 RIVU_BASE_URL 应设为同一个地址。
const api = process.env.RIVU_DEV_API ?? "http://127.0.0.1:8080";

export default defineConfig({
  // 单页应用：rivu（api / all 角色）提供构建产物，没有对应文件的页面路径回落到 index.html（bin/rivu/src/dashboard.rs），
  // 页面在浏览器里渲染，数据全部来自 /api/internal/*。
  // CSP 由构建写进 index.html 的 <meta>：入口页的启动脚本是内联的，按哈希放行，其余脚本只许同源。
  // 反向代理不要再给页面另加 CSP（两份策略取交集，会把启动脚本挡掉）。
  // precompress：同时产出 .br 与 .gz，rivu 按 Accept-Encoding 直接发送压缩好的文件。
  plugins: [
    sveltekit({
      adapter: adapter({ fallback: "index.html", precompress: true }),
      csp: {
        mode: "hash",
        directives: {
          "default-src": ["self"],
          "script-src": ["self"],
          // SvelteKit 的路由播报区（aria-live）带内联 style；按哈希放行会随 Kit 版本失效，样式注入风险远低于脚本
          "style-src": ["self", "unsafe-inline"],
          "img-src": ["self", "data:"],
          "font-src": ["self"],
          "connect-src": ["self"],
          "object-src": ["none"],
          "base-uri": ["none"],
          "form-action": ["self"],
        },
      },
    }),
  ],
  build: { target: "es2022" },
  define: { __RIVU_VERSION__: JSON.stringify(process.env.RIVU_VERSION ?? pkg.version) },
  server: {
    proxy: {
      "/api": {
        target: api,
        changeOrigin: true,
        headers: { origin: api },
      },
    },
  },
});
