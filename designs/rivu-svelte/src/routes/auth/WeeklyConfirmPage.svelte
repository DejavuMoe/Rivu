<!--
  确认周报邮箱（确认邮件里的链接）。和重设密码、接受邀请一样，是「邮件里的链接 → 一个动作」的独立页，不需要登录。
  打开链接只显示页面，点「确认邮箱」才生效：邮件客户端预览链接时不会替用户确认。
  令牌无效（过期、用过、不存在）时说明并指回账号页重新发送。
  原型里令牌为 expired 演示失效；评审面板的「数据状态」为加载时是检查令牌的骨架，「登录结果」演示提交时连不上服务器与其他失败。
-->
<script lang="ts">
  import { CircleCheck, Link2Off } from "@lucide/svelte";
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import { USER } from "../../lib/data/fixtures";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { href } from "../../lib/router.svelte";
  import { weekly } from "../account/weekly.svelte";
  import "./strings";

  let { token }: { token: string } = $props();
  const invalid = $derived(token === "expired" || !token);

  let busy = $state(false);
  let done = $state(false);
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
        weekly.email = "confirmed";
        weekly.delivered = true;
        done = true;
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
      <h1>{t("wconfirm.invalidTitle")}</h1>
      <p class="muted">{t("wconfirm.invalidBody")}</p>
      <Button variant="primary" size="lg" block href={href("/account")}>{t("wconfirm.toAccount")}</Button>
    </div>
  {:else if done}
    <div class="col done">
      <span class="icon good" aria-hidden="true"><CircleCheck size={20} strokeWidth={1.75} /></span>
      <h1>{t("wconfirm.doneTitle")}</h1>
      <p class="muted">{t("wconfirm.doneBody")}</p>
      <Button variant="primary" size="lg" block href={href("/account")}>{t("wconfirm.choose")}</Button>
    </div>
  {:else}
    <div class="col">
      <h1>{t("wconfirm.title")}</h1>
      <p class="muted">{t("wconfirm.body", { email: USER.email })}</p>
      {#if failed}<Callout tone="bad">{t(failed === "network" ? "login.network" : "wconfirm.failed")}</Callout>{/if}
      <Button variant="primary" size="lg" block loading={busy} onclick={confirm}>{t("wconfirm.submit")}</Button>
    </div>
  {/if}
</AuthShell>

<style>
  .col {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
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
