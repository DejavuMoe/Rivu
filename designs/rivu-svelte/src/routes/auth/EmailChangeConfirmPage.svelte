<!--
  确认新的登录邮箱（发到新邮箱的确认邮件里的链接）。同周报确认页：独立页，不需要登录；打开链接只显示页面，点「确认修改」才生效。
  生效后：登录邮箱改成新邮箱；只保留点确认的这台浏览器的会话（如果它登录着这个账号），其他会话全部撤销；
  原邮箱收到一封「登录邮箱已修改」的通知；已确认的周报邮箱跟着改成新邮箱。
  所以文案按这台浏览器是否登录着这个账号分两种：登录着 → 「其他设备会退出」，完成后回到账号页；
  没登录（例如在手机的邮件 App 里打开） → 「所有设备都会退出」，完成后去登录。
  令牌无效（过期、用过、修改已取消、不存在）时说明并指回账号页。
  原型里令牌为 expired 演示失效；评审面板的「改邮箱链接」切换这台浏览器是否登录着，「数据状态」为加载时是检查令牌的骨架，
  「登录结果」演示提交时连不上服务器与其他失败。
-->
<script lang="ts">
  import { CircleCheck, Link2Off } from "@lucide/svelte";
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { href } from "../../lib/router.svelte";
  import { account, DEMO_NEW_EMAIL } from "../account/email.svelte";
  import { weekly } from "../account/weekly.svelte";
  import "./strings";

  let { token }: { token: string } = $props();
  const invalid = $derived(token === "expired" || !token);
  const signedIn = $derived(app.review.emailLink === "signedIn");

  // 直接打开链接（账号页没有发起修改）时用演示的新邮箱
  const next = $derived(account.pending || DEMO_NEW_EMAIL);
  let changed = $state("");

  let busy = $state(false);
  let failed = $state<"" | "network" | "failed">("");

  function confirm() {
    if (busy) return;
    busy = true;
    failed = "";
    setTimeout(() => {
      busy = false;
      const r = app.review.login;
      if (r === "network") failed = "network";
      else if (r === "failed") failed = "failed";
      else {
        changed = next;
        account.email = next;
        account.pending = "";
        account.othersSignedOut = true;
        if (weekly.email === "pending") weekly.email = "unconfirmed";
      }
    }, 500);
  }
</script>

<AuthShell>
  {#if app.review.dataState === "loading"}
    <div class="col" aria-busy="true"><Skeleton width="50%" height={20} /><Skeleton height={40} radius={6} /></div>
  {:else if invalid}
    <div class="col">
      <span class="icon bad" aria-hidden="true"><Link2Off size={20} strokeWidth={1.75} /></span>
      <h1>{t("econfirm.invalidTitle")}</h1>
      <p class="muted">{t("econfirm.invalidBody")}</p>
      <Button variant="primary" size="lg" block href={href(signedIn ? "/account" : "/login")}>{t(signedIn ? "wconfirm.toAccount" : "econfirm.toLogin")}</Button>
    </div>
  {:else if changed}
    <div class="col done">
      <span class="icon good" aria-hidden="true"><CircleCheck size={20} strokeWidth={1.75} /></span>
      <h1>{t("econfirm.doneTitle")}</h1>
      <div class="text">
        <p class="muted">{t(signedIn ? "econfirm.doneBody" : "econfirm.doneBodySignedOut", { email: changed })}</p>
        {#if weekly.email === "confirmed"}<p class="muted">{t("econfirm.reportsMoved")}</p>{/if}
      </div>
      <Button variant="primary" size="lg" block href={href(signedIn ? "/account" : "/login")}>{t(signedIn ? "econfirm.toAccount" : "econfirm.toLogin")}</Button>
    </div>
  {:else}
    <div class="col">
      <h1>{t("econfirm.title")}</h1>
      <p class="muted">{t(signedIn ? "econfirm.body" : "econfirm.bodySignedOut", { email: next, old: account.email })}</p>
      {#if failed}<Callout tone="bad">{t(failed === "network" ? "login.network" : "econfirm.failed")}</Callout>{/if}
      <Button variant="primary" size="lg" block loading={busy} onclick={confirm}>{t("econfirm.submit")}</Button>
    </div>
  {/if}
</AuthShell>

<style>
  .col {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .done {
    animation: fade var(--dur-pop) var(--ease);
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  h1 {
    font-size: var(--fs-lg);
  }
  .muted {
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .icon {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    border-radius: 50%;
  }
  .icon.good {
    background: var(--good-bg);
    color: var(--good-text);
  }
  .icon.bad {
    background: var(--bg-element);
    color: var(--text-muted);
  }
</style>
