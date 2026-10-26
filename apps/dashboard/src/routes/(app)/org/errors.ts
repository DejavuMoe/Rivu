import { ApiError } from "#lib/api.ts";
import { t } from "#lib/i18n.ts";
import "./strings";

/** 服务端给用户看的错误（400、409）直接显示，其余用通用文案。 */
export function orgErrText(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.status === 400 || e.status === 409) return e.message;
    if (e.status === 403) return t("org.err.forbidden");
  }
  return t("org.err.generic");
}
