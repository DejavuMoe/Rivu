// SvelteKit 的全局类型。这个应用没有服务端渲染，App.Locals 等都用不到。
declare global {
  namespace App {}
  /** 构建时注入的版本号（vite.config.ts 的 define，可用 RIVU_VERSION 覆盖） */
  const __RIVU_VERSION__: string;
}

export {};
