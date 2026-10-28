<!--
  账号页的「两步验证」卡片（GET /api/internal/me/totp）：未开启、已开启、不可用（服务器没配 RIVU_SECRET_KEY）。
  开启在一个对话框里分三步：确认当前密码（服务端同时生成密钥）→ 扫码并输入第一个验证码 → 保存恢复码
  （只显示这一次，「我已保存」才关闭）。换恢复码：密码 → 恢复码。关闭：密码。
  放弃设置（第二步关掉对话框）时密钥留在「设置中」，下次开启会重新生成，不影响登录。
-->
<script lang="ts">
  import { CircleAlert, Copy, Download } from "@lucide/svelte";
  import Badge from "#lib/ui/Badge.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import CopyField from "#lib/ui/CopyField.svelte";
  import Dialog from "#lib/ui/Dialog.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import { api, ApiError, type TotpStatus } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { toast } from "#lib/state.svelte.ts";
  import "./strings";

  let { unauthorized }: { unauthorized: (e: unknown) => Promise<boolean> } = $props();

  let st = $state<TotpStatus | null>(null);
  let failed = $state(false);

  async function load() {
    failed = false;
    try {
      st = await api.totpStatus();
    } catch (e) {
      if (!(await unauthorized(e))) failed = true;
    }
  }
  void load();

  // ---- 对话框 ----
  let open = $state(false);
  let mode = $state<"enable" | "disable" | "regenerate">("enable");
  let step = $state<"password" | "scan" | "codes">("password");
  let password = $state("");
  let code = $state("");
  let error = $state("");
  let busy = $state(false);
  let setup = $state<{ secret: string; qr_svg: string } | null>(null);
  let codes = $state<string[]>([]);

  function start(m: typeof mode) {
    mode = m;
    step = "password";
    password = code = error = "";
    setup = null;
    codes = [];
    open = true;
  }

  const title = $derived(
    step === "codes" ? t("tf.codesTitle") : mode === "enable" ? t("tf.dlg.enable") : mode === "disable" ? t("tf.dlg.disable") : t("tf.dlg.regenerate"),
  );
  // 密钥每 4 个字符一组，手动输入时好对照
  const grouped = $derived(setup?.secret.replace(/(.{4})/g, "$1 ").trim() ?? "");

  async function fail(e: unknown) {
    if (await unauthorized(e)) return;
    const c = e instanceof ApiError ? e.code : "";
    if (c === "wrong_password") error = t("ac.pw.wrong");
    else if (c === "invalid_code") {
      error = t("tf.codeInvalid");
      code = "";
    } else if (e instanceof ApiError && e.status === 429) error = t("ac.pw.rate");
    else {
      open = false;
      toast(t("ac.failed"), "bad");
      void load();
    }
  }

  async function next(e?: SubmitEvent) {
    e?.preventDefault();
    if (busy) return;
    busy = true;
    error = "";
    try {
      if (step === "password") {
        if (mode === "enable") {
          setup = await api.totpSetup(password);
          step = "scan";
        } else if (mode === "regenerate") {
          codes = (await api.totpRecoveryCodes(password)).recovery_codes;
          step = "codes";
        } else {
          await api.totpDisable(password);
          open = false;
          toast(t("tf.disabled"));
          void load();
        }
        password = "";
      } else if (step === "scan") {
        codes = (await api.totpEnable(code.trim())).recovery_codes;
        step = "codes";
      }
    } catch (err) {
      await fail(err);
    } finally {
      busy = false;
    }
  }

  function finish() {
    open = false;
    toast(mode === "enable" ? t("tf.enabled") : t("tf.regenerated"), "good");
    void load();
  }

  async function copyCodes() {
    try {
      await navigator.clipboard.writeText(codes.join("\n"));
      toast(t("tf.copied"), "good");
    } catch {
      toast(t("ac.failed"), "bad");
    }
  }

  function downloadCodes() {
    const url = URL.createObjectURL(new Blob([codes.join("\n") + "\n"], { type: "text/plain" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = "rivu-recovery-codes.txt";
    a.click();
    URL.revokeObjectURL(url);
  }
</script>

<Card title={t("tf.title")}>
  {#snippet actions()}
    {#if st?.enabled}<Badge tone="good">{t("tf.onBadge")}</Badge>{/if}
  {/snippet}
  {#if failed}
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("tf.loadFailed")}>
      {#snippet actions()}<Button size="sm" onclick={load}>{t("common.retry")}</Button>{/snippet}
    </EmptyState>
  {:else if !st}
    <div class="sk" aria-busy="true"><Skeleton width="60%" height={14} /></div>
  {:else if !st.available && !st.enabled}
    <p class="muted">{t("tf.unavailable")}</p>
  {:else if !st.enabled}
    <div class="row">
      <p>{t("tf.off")}</p>
      <Button onclick={() => start("enable")}>{t("tf.enable")}</Button>
    </div>
  {:else}
    {@const n = st.recovery_remaining ?? 0}
    <div class="row">
      <div class="text">
        <p>{t("tf.on")}</p>
        <p class="muted" class:warn={n <= 3}>{n <= 3 ? t("tf.remainingLow", { n }) : t("tf.remaining", { n })}</p>
      </div>
      <div class="acts">
        <Button size="sm" onclick={() => start("regenerate")}>{t("tf.regenerate")}</Button>
        <Button size="sm" variant="ghost" onclick={() => start("disable")}>{t("tf.disable")}</Button>
      </div>
    </div>
  {/if}
</Card>

<Dialog bind:open {title} size={mode === "enable" ? "md" : "sm"}>
  {#key step}
    <div class="step">
      {#if step === "password"}
        <form id="tf-form" class="form" onsubmit={next} novalidate>
          {#if mode === "disable"}<p>{t("tf.disableBody")}</p>{:else if mode === "regenerate"}<p>{t("tf.regenerateBody")}</p>{/if}
          <Field label={t("tf.password")} for="tf-pw" hint={mode === "enable" ? t("tf.passwordHint") : undefined} error={error || undefined}>
            <Input id="tf-pw" type="password" autocomplete="current-password" bind:value={password} invalid={!!error} />
          </Field>
        </form>
      {:else if step === "scan" && setup}
        <form id="tf-form" class="form" onsubmit={next} novalidate>
          <p>{t("tf.scan")}</p>
          <div class="qr-row">
            <!-- 服务端生成的二维码（白底黑码，暗色主题下也能扫） -->
            <div class="qr">{@html setup.qr_svg}</div>
            <div class="manual">
              <CopyField label={t("tf.manual")} value={grouped} />
              <Field label={t("tf.code")} for="tf-code" error={error || undefined}>
                <Input
                  id="tf-code"
                  mono
                  inputmode="numeric"
                  autocomplete="one-time-code"
                  maxlength={6}
                  placeholder="000000"
                  bind:value={code}
                  invalid={!!error}
                />
              </Field>
            </div>
          </div>
        </form>
      {:else}
        <div class="form">
          <p>{t("tf.codesBody")}</p>
          <ol class="codes">
            {#each codes as c (c)}<li>{c}</li>{/each}
          </ol>
          <div class="code-acts">
            <Button size="sm" icon={Copy} onclick={copyCodes}>{t("tf.copy")}</Button>
            <Button size="sm" icon={Download} onclick={downloadCodes}>{t("tf.download")}</Button>
          </div>
        </div>
      {/if}
    </div>
  {/key}
  {#snippet footer()}
    {#if step === "codes"}
      <Button variant="primary" onclick={finish}>{t("tf.saved")}</Button>
    {:else}
      <Button variant="ghost" onclick={() => (open = false)}>{t("common.cancel")}</Button>
      <Button
        type="submit"
        form="tf-form"
        variant={mode === "disable" ? "danger" : "primary"}
        loading={busy}
        disabled={step === "password" ? !password : !/^\d{6}$/.test(code.trim())}
      >
        {step === "scan" ? t("tf.verify") : mode === "disable" ? t("tf.disable") : t("tf.continue")}
      </Button>
    {/if}
  {/snippet}
</Dialog>

<style>
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .warn {
    color: var(--warn-text);
  }
  .acts {
    display: flex;
    gap: var(--sp-2);
    flex: none;
  }
  .sk {
    padding: var(--sp-1) 0;
  }
  .step {
    animation: fade var(--dur-pop) var(--ease);
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .qr-row {
    display: flex;
    gap: var(--sp-5);
    align-items: flex-start;
  }
  .qr {
    flex: none;
    width: 168px;
    height: 168px;
    overflow: hidden;
    border-radius: var(--r-sm);
    background: #fff;
    box-shadow: 0 0 0 var(--hairline) var(--border);
  }
  .qr :global(svg) {
    display: block;
    width: 100%;
    height: 100%;
  }
  .manual {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    flex: 1;
    min-width: 0;
  }
  .codes {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: var(--sp-2) var(--sp-6);
    margin: 0;
    padding: var(--sp-4) var(--sp-5);
    border-radius: var(--r-sm);
    background: var(--bg-subtle);
    box-shadow: inset 0 0 0 var(--hairline) var(--border-subtle);
    font-family: var(--font-mono);
    list-style: none;
  }
  .code-acts {
    display: flex;
    gap: var(--sp-2);
  }
  @media (max-width: 560px) {
    .row {
      flex-direction: column;
      align-items: flex-start;
    }
    .qr-row {
      flex-direction: column;
      align-items: center;
    }
    .manual {
      width: 100%;
    }
  }
</style>
