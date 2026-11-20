<!--
  个人资料卡里的「邮箱」一行：显示登录邮箱，旁边「修改」打开对话框（新邮箱 + 当前密码；开了两步验证也不要验证码，与关闭两步验证一致）。
  - 配置了邮件：提交后给新邮箱发确认链接，邮箱暂时不变；这一行下面显示「确认链接已发到 …」，可以重新发送或取消。
    打开链接、在确认页点按钮后才生效（routes/auth/EmailChangeConfirmPage.svelte），其他设备随之退出，原邮箱收到一封通知。
  - 没配置邮件：校验密码后立即生效，对话框里提前说明原邮箱不能再用来登录。
  - 周报邮箱已确认时，周报跟着改发到新邮箱，对话框里说一句；周报邮箱还在待确认时，旧的确认链接作废，回到未确认。
  原型里：密码的校验结果由评审面板的「登录结果」演示，组织成员的邮箱（如 zhou.qiming@northwind.dev）演示已被使用，
  「确认邮件」演示发送失败，「改邮箱」直接切到待确认。
-->
<script lang="ts">
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import { t } from "../../lib/i18n";
  import { app, passwordResult, toast } from "../../lib/state.svelte";
  import { account, EMAIL_WAIT_MS, emailTaken, emailValid } from "./email.svelte";
  import { weekly } from "./weekly.svelte";
  import "./strings";

  const mail = $derived(app.review.mail);
  /** 没配置邮件时不会有待确认的修改 */
  const pending = $derived(mail ? account.pending : "");

  // ---- 对话框 ----
  let open = $state(false);
  let email = $state("");
  let password = $state("");
  let tried = $state(false);
  let busy = $state(false);
  /** 服务端返回的字段错误（输入变了就清掉） */
  let serverErr = $state<{ email?: string; password?: string }>({});
  let sendFailed = $state(false);

  const err = $derived({
    email:
      serverErr.email ??
      (!email.trim()
        ? t("ac.email.required")
        : !emailValid(email)
          ? t("ac.email.invalid")
          : email.trim().toLowerCase() === account.email.toLowerCase()
            ? t("ac.email.same")
            : ""),
    password: serverErr.password ?? (password ? "" : t("ac.pw.currentRequired")),
  });

  function start() {
    email = password = "";
    tried = false;
    serverErr = {};
    sendFailed = false;
    open = true;
  }

  function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (err.email || err.password || busy) return;
    busy = true;
    sendFailed = false;
    setTimeout(() => {
      busy = false;
      const r = passwordResult();
      if (r === "wrong") serverErr = { password: t("ac.pw.wrong") };
      else if (r === "rate") serverErr = { password: t("ac.pw.rate") };
      else if (r === "failed") {
        open = false;
        toast(t("ac.failed"), "bad");
      } else if (emailTaken(email)) serverErr = { email: t("ac.email.taken") };
      else if (mail && app.review.weeklyMail === "fails") sendFailed = true;
      else done(email.trim());
    }, 500);
  }

  function done(next: string) {
    open = false;
    if (mail) {
      account.pending = next;
      account.requestedAt = Date.now();
      account.delivered = true;
      toast(t("ac.email.sent"), "good");
      return;
    }
    account.email = next;
    account.othersSignedOut = true;
    if (weekly.email === "pending") weekly.email = "unconfirmed";
    toast(t("ac.email.changed"), "good");
  }

  // ---- 待确认：重新发送与取消 ----
  let rowBusy = $state<"" | "send" | "cancel">("");
  let now = $state(Date.now());
  const waitLeft = $derived(Math.max(0, Math.ceil((account.requestedAt + EMAIL_WAIT_MS - now) / 1000)));
  $effect(() => {
    if (account.requestedAt + EMAIL_WAIT_MS <= Date.now()) return;
    now = Date.now();
    const id = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(id);
  });

  function resend() {
    if (rowBusy || waitLeft > 0) return;
    rowBusy = "send";
    setTimeout(() => {
      rowBusy = "";
      account.requestedAt = Date.now();
      account.delivered = app.review.weeklyMail === "ok";
      if (account.delivered) toast(t("ac.email.resent"), "good");
    }, 280);
  }

  function cancel() {
    if (rowBusy) return;
    rowBusy = "cancel";
    setTimeout(() => {
      rowBusy = "";
      account.pending = "";
      account.delivered = true;
      toast(t("ac.email.cancelled"));
    }, 280);
  }
</script>

<div class="line">
  <span class="val">{account.email}</span>
  {#if !pending}
    <Button size="sm" onclick={start}>{t("ac.email.change")}</Button>
  {/if}
</div>

{#if pending}
  <div class="pending">
    <p>{t("ac.email.pendingBody", { email: pending })}</p>
    {#if !account.delivered}<Callout tone="bad">{t("ac.email.sendFailed")}</Callout>{/if}
    <div class="acts">
      <Button size="sm" loading={rowBusy === "send"} disabled={!!rowBusy || waitLeft > 0} onclick={resend}>{t("ac.weekly.resend")}</Button>
      <Button size="sm" variant="ghost" loading={rowBusy === "cancel"} disabled={!!rowBusy} onclick={cancel}>{t("ac.email.cancel")}</Button>
      <span class="wait" role="timer" aria-live="off">
        {#if waitLeft > 0 && !rowBusy}{t("ac.weekly.wait", { n: waitLeft })}{/if}
      </span>
    </div>
  </div>
{/if}

<Dialog bind:open title={t("ac.email.title")} size="sm" requireConfirmation={busy}>
  <form id="ec-form" class="form" onsubmit={submit} novalidate>
    <Field label={t("ac.email.new")} for="ec-email" error={tried ? err.email : ""}>
      <Input
        id="ec-email"
        type="email"
        autocomplete="email"
        bind:value={email}
        oninput={() => (serverErr = { ...serverErr, email: undefined })}
        invalid={tried && !!err.email}
      />
    </Field>
    <Field label={t("ac.pw.current")} for="ec-pw" error={tried ? err.password : ""}>
      <Input
        id="ec-pw"
        type="password"
        autocomplete="current-password"
        bind:value={password}
        oninput={() => (serverErr = { ...serverErr, password: undefined })}
        invalid={tried && !!err.password}
      />
    </Field>
    {#if sendFailed}<Callout tone="bad">{t("ac.email.sendFailed")}</Callout>{/if}
    {#if !mail || weekly.email === "confirmed"}
      <div class="notes">
        {#if !mail}<p>{t("ac.email.immediate", { email: account.email })}</p>{/if}
        {#if weekly.email === "confirmed"}<p>{t("ac.email.reportsMove")}</p>{/if}
      </div>
    {/if}
  </form>
  {#snippet footer()}
    <Button variant="ghost" disabled={busy} onclick={() => (open = false)}>{t("common.cancel")}</Button>
    <Button type="submit" form="ec-form" variant="primary" loading={busy}>{t(mail ? "ac.email.send" : "ac.email.submit")}</Button>
  {/snippet}
</Dialog>

<style>
  .line {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  /* 与行布局里标签的 5px 上内边距对齐（同个人资料的其他行） */
  .val {
    padding-top: 5px;
    overflow-wrap: anywhere;
  }
  .pending {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
    padding-top: var(--sp-3);
    border-top: var(--hairline) solid var(--border-subtle);
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--sp-2);
  }
  .wait {
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .notes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
</style>
