<!--
  确认周报邮箱（确认邮件里的链接）。和重设密码、接受邀请一样，是「邮件里的链接 → 一个动作」的独立页，不需要登录。
  打开链接只检查令牌（GET，不改变任何状态），点「确认邮箱」才生效（POST）：邮件客户端预览链接时不会替用户确认。
  令牌无效（过期、用过、不存在、账号邮箱已变更）服务端一律返回 404，这里只显示一种状态并指回账号页重新发送。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import { CircleCheck, Link2Off } from "@lucide/svelte";
  import { page } from "$app/state";
  import AuthShell from "#lib/app/AuthShell.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import { api, ApiError } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import "../../../strings";

  const token = $derived(page.params.token ?? "");
  let status = $state<"loading" | "ready" | "invalid" | "done">("loading");
  let email = $state("");
  /** 检查令牌时连不上或服务端出错：不能断定链接失效，显示确认页，按钮可重试 */
  let checkFailed = $state(false);

  $effect(() => {
    const tk = token;
    untrack(() => {
      status = "loading";
      checkFailed = false;
      api.checkWeeklyConfirm(tk).then(
        (r) => {
          email = r.email;
          status = "ready";
        },
        (e) => {
          if (e instanceof ApiError && e.status === 404) status = "invalid";
          else {
            checkFailed = true;
            status = "ready";
          }
        },
      );
    });
  });

  let busy = $state(false);
  let failed = $state<"" | "network" | "failed">("");

  async function confirm() {
    if (busy) return;
    busy = true;
    failed = "";
    try {
      const r = await api.confirmWeekly(token);
      email = r.email;
      status = "done";
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) status = "invalid";
      else failed = e instanceof ApiError && e.status === 0 ? "network" : "failed";
    } finally {
      busy = false;
    }
  }
</script>

<AuthShell>
  {#if status === "loading"}
    <div class="col" aria-busy="true"><Skeleton width="50%" height={20} /><Skeleton height={40} radius={6} /></div>
  {:else if status === "invalid"}
    <div class="col">
      <span class="icon bad" aria-hidden="true"><Link2Off size={20} strokeWidth={1.75} /></span>
      <h1>{t("wconfirm.invalidTitle")}</h1>
      <p class="muted">{t("wconfirm.invalidBody")}</p>
      <Button variant="primary" size="lg" block href="/account">{t("wconfirm.toAccount")}</Button>
    </div>
  {:else if status === "done"}
    <div class="col done">
      <span class="icon good" aria-hidden="true"><CircleCheck size={20} strokeWidth={1.75} /></span>
      <h1>{t("wconfirm.doneTitle")}</h1>
      <p class="muted">{t("wconfirm.doneBody")}</p>
      <Button variant="primary" size="lg" block href="/account">{t("wconfirm.choose")}</Button>
    </div>
  {:else}
    <div class="col">
      <h1>{t("wconfirm.title")}</h1>
      {#if email}<p class="muted">{t("wconfirm.body", { email })}</p>{/if}
      {#if failed}
        <Callout tone="bad">{t(failed === "network" ? "login.network" : "wconfirm.failed")}</Callout>
      {:else if checkFailed}
        <Callout tone="bad">{t("login.network")}</Callout>
      {/if}
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
