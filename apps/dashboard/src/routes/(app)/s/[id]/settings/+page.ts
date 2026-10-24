// 设置的入口：默认进「常规」（没有该分区权限的角色由布局改到第一个可见分区）。
import { redirect } from "@sveltejs/kit";
import type { PageLoad } from "./$types";

export const load: PageLoad = ({ params }) => {
  redirect(307, `/s/${params.id}/settings/general`);
};