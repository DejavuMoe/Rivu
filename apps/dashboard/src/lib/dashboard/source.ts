// 看板的数据来源：登录用户按站点 id 查询；共享链接与公开看板按 site_key 查询，共享链接还要带上 slug。

import { api, ApiError, type QueryResult } from "../api";
import type { Spec } from "./spec";

export interface Source {
  /** 路径里的站点：数字 id 或 site_key */
  site: number | string;
  share?: string;
}

export function runQuery(src: Source, spec: Spec, signal?: AbortSignal): Promise<QueryResult> {
  return api.query(src.site, spec, { share: src.share, signal });
}

/** 属性键（只要键名，按事件数排好序）。 */
export function runPropertyKeys(src: Source, spec: Spec, signal?: AbortSignal): Promise<string[]> {
  return api.propertyKeys(src.site, spec, { share: src.share, signal }).then((r) => r.keys.map((k) => k.key));
}

export function runRealtime(src: Source, signal?: AbortSignal): Promise<QueryResult> {
  return api.realtime(src.site, { share: src.share, signal });
}

export type Failure = "error" | "busy" | "timeout";

export function failureOf(e: unknown): Failure {
  if (e instanceof ApiError) {
    if (e.code === "query_timeout") return "timeout";
    if (e.code === "resource_limit" || e.code === "rate_limited") return "busy";
  }
  return "error";
}

export function isAbort(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}
