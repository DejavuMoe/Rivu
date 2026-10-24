// 站点设置的加载与更新：每个站点加载一次 GET settings，之后用 PATCH 的响应整体替换。
import { getContext, setContext } from "svelte";
import { api, ApiError, type SettingsBody, type SitePatch } from "#lib/api.ts";
import { t } from "#lib/i18n.ts";
import { refreshSites } from "#lib/session.svelte.ts";
import { toast } from "#lib/state.svelte.ts";
import "./strings";

const KEY = Symbol("site-settings");

/** 服务端给用户看的错误（400、409）直接显示，其余用通用文案。 */
export function errText(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.status === 400 || e.status === 409) return e.message;
    if (e.status === 403) return t("set.err.forbidden");
  }
  return t("set.err.generic");
}

export class SettingsStore {
  data = $state<SettingsBody | null>(null);
  status = $state<"idle" | "loading" | "ready" | "error">("idle");

  constructor(readonly id: number) {}

  async load(): Promise<void> {
    if (this.status === "loading" || this.status === "ready") return;
    this.status = "loading";
    try {
      this.data = await api.siteSettings(this.id);
      this.status = "ready";
    } catch {
      this.status = "error";
    }
  }

  /** 保存成功返回 true；失败弹出错误并返回 false。名称、时区、公开状态变了要刷新侧栏用的站点列表。 */
  async patch(body: SitePatch): Promise<boolean> {
    try {
      this.data = await api.patchSite(this.id, body);
      if (body.name !== undefined || body.timezone !== undefined || body.is_public !== undefined) await refreshSites();
      return true;
    } catch (e) {
      toast(errText(e), "bad");
      return false;
    }
  }
}

export function provideSettings(get: () => SettingsStore) {
  setContext(KEY, get);
}

/** 分区组件取当前站点的设置（布局保证进入分区前已加载完成）。 */
export function useSettings(): () => SettingsStore {
  return getContext<() => SettingsStore>(KEY);
}