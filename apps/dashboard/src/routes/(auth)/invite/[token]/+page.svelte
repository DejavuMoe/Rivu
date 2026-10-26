<!--
  接受邀请（公开页，登录与否都能打开）。
  有账号：用邀请邮箱登录的人点「接受邀请」；没登录去登录页（登录后回到这里）；登录的是别人则提示并可退出。
  没有账号：填名字（可选）和密码，接受后服务端直接登录，随后进入站点列表或被邀请的站点。
  邀请无效、过期、已用、已撤销，服务端一律返回 404，这里只显示一种状态。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import AuthShell from "#lib/app/AuthShell.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import { api, ApiError, type InvitationInfo } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { clearSession, loadSession, setOrg } from "#lib/session.svelte.ts";
  import "../../strings";

  const token = $derived(page.params.token ?? "");

  let info = $state<InvitationInfo | null>(null);
  let status = $state<"loading" | "ready" | "invalid" | "failed">("loading");
  let name = $state("");
  let password = $state("");
  let tried = $state(false);
  let busy = $state(false);
  let server = $state({ name: "", password: "", form: "" });

  const pwLen = $derived([...password].length);
  const nameLen = $derived([...name.trim()].length);
  const errors = $derived({
    name: nameLen > 100 ? t("setup.name.long") : "",
    password: pwLen < 12 ? t("setup.password.short") : pwLen > 128 ? t("setup.password.long") : "",
  });

  const roleLabel = $derived.by(() => {
    const r = info?.site ? info.site_role : info?.role;
    return r ? t(`role.${r}`) : "";
  });
  const sameUser = $derived(!!info?.signed_in_as && info.signed_in_as.toLowerCase() === info.email.toLowerCase());
  const loginHref = $derived(`/login?next=${encodeURIComponent(`/invite/${token}`)}`);

  async function load(tk: string) {
    status = "loading";
    try {
      info = await api.invitation(tk);
      status = "ready";
    } catch (e) {
      info = null;
      status = e instanceof ApiError && e.status === 404 ? "invalid" : "failed";
    }
  }

  $effect(() => {
    const tk = token;
    untrack(() => void load(tk));
  });

  async function signOut() {
    busy = true;
    try {
      await api.logout();
    } catch {
      /* 没有会话也算退出 */
    }
    clearSession();
    busy = false;
    await load(token);
  }

  async function accept(e?: SubmitEvent) {
    e?.preventDefault();
    if (!info || busy) return;
    const creating = !info.account_exists;
    tried = true;
    if (creating && (errors.name || errors.password)) return;
    server = { name: "", password: "", form: "" };
    busy = true;
    try {
      const res = await api.acceptInvitation(token, creating ? { name: name.trim() || undefined, password } : {});
      await loadSession();
      setOrg(res.organization_id);
      await goto(res.site_id ? `/s/${res.site_id}` : "/sites", { replace: true });
    } catch (err) {
      busy = false;
      if (!(err instanceof ApiError)) throw err;
      if (err.status === 404) status = "invalid";
      else if (err.status === 401) {
        server.form = t("inv.acc.needSignIn");
        await load(token);
      } else if (err.status === 403) {
        server.form = t("inv.acc.mismatch");
        await load(token);
      } else if (err.status === 400 && /password/i.test(err.message)) server.password = err.message;
      else if (err.status === 400 && /^the name/i.test(err.message)) server.name = err.message;
      else if (err.status === 429) server.form = t("login.rate", { n: err.retryAfter ?? 60 });
      else server.form = err.status === 0 ? t("login.network") : err.message;
    }
  }
</script>

<AuthShell>
  {#if status === "loading"}
    <div class="stack" aria-busy="true">
      <Skeleton width="60%" height={20} />
      <Skeleton width="80%" height={14} />
      <Skeleton height={40} radius={6} />
    </div>
  {:else if status === "invalid"}
    <div class="stack">
      <h1>{t("inv.acc.invalid")}</h1>
      <Button href="/login" block>{t("inv.acc.toLogin")}</Button>
    </div>
  {:else if status === "failed" || !info}
    <div class="stack">
      <h1>{t("login.network")}</h1>
      <Button block onclick={() => load(token)}>{t("common.retry")}</Button>
    </div>
  {:else}
    <form class="stack" onsubmit={accept} novalidate>
      <h1>
        {info.site
          ? t("inv.acc.title.site", { org: info.organization.name, site: info.site.name })
          : t("inv.acc.title.org", { org: info.organization.name })}
      </h1>
      <p class="muted">{t("inv.acc.as", { email: info.email, role: roleLabel })}</p>
      {#if info.inviter}<p class="muted">{t("inv.acc.by", { name: info.inviter })}</p>{/if}

      {#if server.form}<Callout tone="bad">{server.form}</Callout>{/if}

      {#if info.account_exists}
        {#if sameUser}
          <Button type="submit" variant="primary" size="lg" block loading={busy}>{t("inv.acc.accept")}</Button>
        {:else if info.signed_in_as}
          <p>{t("inv.acc.other", { current: info.signed_in_as, email: info.email })}</p>
          <Button size="lg" block loading={busy} onclick={signOut}>{t("inv.acc.signOut")}</Button>
        {:else}
          <Button variant="primary" size="lg" block href={loginHref}>{t("inv.acc.signIn")}</Button>
        {/if}
      {:else}
        <Field label={t("setup.name")} for="inv-name" optional error={(tried && errors.name) || server.name}>
          <Input id="inv-name" autocomplete="name" bind:value={name} invalid={(tried && !!errors.name) || !!server.name} />
        </Field>
        <Field
          label={t("setup.password")}
          for="inv-password"
          hint={t("setup.password.hint")}
          error={((tried || pwLen > 128) && errors.password) || server.password}
        >
          <Input
            id="inv-password"
            type="password"
            autocomplete="new-password"
            bind:value={password}
            invalid={((tried || pwLen > 128) && !!errors.password) || !!server.password}
          />
        </Field>
        <Button type="submit" variant="primary" size="lg" block loading={busy}>{t("inv.acc.create")}</Button>
      {/if}
    </form>
  {/if}
</AuthShell>

<style>
  .stack {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  h1 {
    font-size: var(--fs-lg);
  }
  p {
    line-height: var(--lh);
  }
</style>
