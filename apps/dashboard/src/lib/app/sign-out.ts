import { goto } from "$app/navigation";
import { api } from "../api";
import { clearSession } from "../session.svelte";

/** 退出登录：服务端没有会话也算成功；无论结果如何都回到登录页。 */
export async function signOut() {
  try {
    await api.logout();
  } catch {
    /* 网络错误时 cookie 可能还在，但界面上已经退出；下次请求会再要求登录 */
  }
  clearSession();
  await goto("/login");
}
