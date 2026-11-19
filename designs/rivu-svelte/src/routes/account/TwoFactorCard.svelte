<!--
  账号页的「两步验证」卡片。状态来自评审面板 app.review.twofa：未开启、已开启、不可用（服务器没配 RIVU_SECRET_KEY）；
  「数据状态」为加载时是骨架，为出错时是「加载失败」加重试。
  开启在一个对话框里分三步：确认当前密码 → 扫码并输入第一个验证码 → 保存恢复码（只显示这一次，「我已保存」才关闭）。
  换恢复码：密码 → 恢复码。关闭：密码。原型里验证码 000000 演示错误，密码的校验结果由「登录结果」演示（密码错误、被限流、其他失败）。
  放弃设置（第二步关掉对话框）时不影响登录；处理中与保存恢复码这一步不能误关对话框。
-->
<script lang="ts">
  import { CircleAlert, Copy, Download } from "@lucide/svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import CopyField from "../../lib/ui/CopyField.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import { t } from "../../lib/i18n";
  import { app, passwordResult, setReview, toast } from "../../lib/state.svelte";
  import "./strings";

  const SECRET = "JBSW Y3DP EHPK 3PXP 4KQY 6ZQK LM3T 2NAB";
  const CODES = ["7k4m-q2xr", "h8wd-3nfa", "p6tz-c9ve", "m3yb-r7ks", "x5jn-t2hd", "c9ra-w4mp", "e2vs-k8fy", "q7nh-d3zt", "w4cp-y6gb", "r8fe-m5xa"];

  let remaining = $state(10);
  const status = $derived(app.review.twofa);
  const loading = $derived(app.review.dataState === "loading");
  const failed = $derived(["error", "busy", "timeout"].includes(app.review.dataState));

  // ---- 对话框 ----
  let open = $state(false);
  let mode = $state<"enable" | "disable" | "regenerate">("enable");
  let step = $state<"password" | "scan" | "codes">("password");
  let password = $state("");
  let code = $state("");
  let error = $state("");
  let busy = $state(false);
  let copyStatus = $state<"tf.copied" | "common.copyFailed" | null>(null);

  function start(m: typeof mode) {
    mode = m;
    step = "password";
    password = code = error = "";
    copyStatus = null;
    open = true;
  }

  const title = $derived(
    step === "codes" ? t("tf.codesTitle") : mode === "enable" ? t("tf.dlg.enable") : mode === "disable" ? t("tf.dlg.disable") : t("tf.dlg.regenerate"),
  );

  function wait(fn: () => void) {
    busy = true;
    setTimeout(() => {
      busy = false;
      fn();
    }, 500);
  }

  function next(e?: SubmitEvent) {
    e?.preventDefault();
    if (busy) return;
    if (step === "password") {
      if (!password) return;
      wait(() => {
        const r = passwordResult();
        if (r === "wrong") error = t("ac.pw.wrong");
        else if (r === "rate") error = t("ac.pw.rate");
        else if (r === "failed") {
          open = false;
          toast(t("ac.failed"), "bad");
        } else {
          error = "";
          if (mode === "enable") step = "scan";
          else if (mode === "regenerate") {
            remaining = 10;
            step = "codes";
          } else {
            open = false;
            setReview({ twofa: "off" });
            toast(t("tf.disabled"));
          }
        }
      });
    } else if (step === "scan") {
      if (!/^\d{6}$/.test(code.trim())) return;
      wait(() => {
        if (code.trim() === "000000") {
          error = t("tf.codeInvalid");
          code = "";
          return;
        }
        error = "";
        remaining = 10;
        step = "codes";
      });
    }
  }

  function finish() {
    open = false;
    if (mode === "enable") {
      setReview({ twofa: "on" });
      toast(t("tf.enabled"), "good");
    } else toast(t("tf.regenerated"), "good");
  }

  async function copyCodes() {
    try {
      await navigator.clipboard.writeText(CODES.join("\n"));
      copyStatus = "tf.copied";
    } catch {
      copyStatus = "common.copyFailed";
    }
  }

  function downloadCodes() {
    const url = URL.createObjectURL(new Blob([CODES.join("\n") + "\n"], { type: "text/plain" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = "rivu-recovery-codes.txt";
    a.click();
    URL.revokeObjectURL(url);
  }

  // 原型用的假二维码：25×25，三个定位角加伪随机模块（真实实现由服务端生成 SVG）
  const N = 25;
  const cells = (() => {
    const out: [number, number][] = [];
    let seed = 7;
    const rnd = () => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) % 100) / 100;
    const finder = (x: number, y: number) =>
      [0, 18].some((ox) => [0, 18].some((oy) => !(ox === 18 && oy === 18) && x >= ox && x < ox + 7 && y >= oy && y < oy + 7));
    for (let y = 0; y < N; y++)
      for (let x = 0; x < N; x++) {
        if (finder(x, y)) {
          const lx = x % 18 === x ? x : x - 18;
          const ly = y % 18 === y ? y : y - 18;
          const ring = lx === 0 || lx === 6 || ly === 0 || ly === 6;
          const core = lx >= 2 && lx <= 4 && ly >= 2 && ly <= 4;
          if (ring || core) out.push([x, y]);
        } else if (!(x < 8 && y < 8) && !(x > 16 && y < 8) && !(x < 8 && y > 16) && rnd() > 0.52) out.push([x, y]);
      }
    return out;
  })();
</script>

<Card title={t("tf.title")}>
  {#snippet actions()}
    {#if status === "on" && !loading && !failed}<Badge tone="good">{t("tf.onBadge")}</Badge>{/if}
  {/snippet}
  {#if failed}
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("tf.loadFailed")}>
      {#snippet actions()}<Button size="sm" onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
    </EmptyState>
  {:else if loading}
    <div class="sk" aria-busy="true"><Skeleton width="60%" height={14} /></div>
  {:else if status === "unavailable"}
    <p class="muted">{t("tf.unavailable")}</p>
  {:else if status === "off"}
    <div class="row">
      <p>{t("tf.off")}</p>
      <Button onclick={() => start("enable")}>{t("tf.enable")}</Button>
    </div>
  {:else}
    <div class="row">
      <div class="text">
        <p>{t("tf.on")}</p>
        <p class:warn={remaining <= 3} class="muted">
          {remaining <= 3 ? t("tf.remainingLow", { n: remaining }) : t("tf.remaining", { n: remaining })}
        </p>
      </div>
      <div class="acts">
        <Button size="sm" onclick={() => start("regenerate")}>{t("tf.regenerate")}</Button>
        <Button size="sm" variant="ghost" onclick={() => start("disable")}>{t("tf.disable")}</Button>
      </div>
    </div>
  {/if}
</Card>

<Dialog bind:open {title} requireConfirmation={busy || step === "codes"} size={mode === "enable" ? "md" : "sm"}>
  {#key step}
    <div class="step">
      {#if step === "password"}
        <form id="tf-form" class="form" onsubmit={next} novalidate>
          {#if mode === "disable"}<p>{t("tf.disableBody")}</p>{:else if mode === "regenerate"}<p>{t("tf.regenerateBody")}</p>{/if}
          <Field label={t("tf.password")} for="tf-pw" hint={mode === "enable" ? t("tf.passwordHint") : undefined} error={error || undefined}>
            <Input id="tf-pw" type="password" autocomplete="current-password" bind:value={password} invalid={!!error} />
          </Field>
        </form>
      {:else if step === "scan"}
        <form id="tf-form" class="form" onsubmit={next} novalidate>
          <p>{t("tf.scan")}</p>
          <div class="qr-row">
            <!-- 服务端生成的二维码（白底黑码，暗色主题下也能扫） -->
            <div class="qr">
              <svg viewBox="-2 -2 {N + 4} {N + 4}" role="img" aria-label="QR">
                <!-- design-lint-allow raw-color: 二维码必须白底黑码 -->
                <rect x="-2" y="-2" width={N + 4} height={N + 4} fill="#fff" />
                <!-- design-lint-allow raw-color: 二维码必须白底黑码 -->
                {#each cells as [x, y] (`${x},${y}`)}<rect {x} {y} width="1" height="1" fill="#000" />{/each}
              </svg>
            </div>
            <div class="manual">
              <CopyField label={t("tf.manual")} value={SECRET} />
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
            {#each CODES as c (c)}<li>{c}</li>{/each}
          </ol>
          <div class="code-acts">
            <Button size="sm" icon={Copy} aria-label={t("tf.copy")} onclick={copyCodes}>{t(copyStatus ?? "tf.copy")}</Button>
            <Button size="sm" icon={Download} onclick={downloadCodes}>{t("tf.download")}</Button>
            <span class="sr-only" role="status">{copyStatus ? t(copyStatus) : ""}</span>
          </div>
        </div>
      {/if}
    </div>
  {/key}
  {#snippet footer()}
    {#if step === "codes"}
      <Button variant="primary" onclick={finish}>{t("tf.saved")}</Button>
    {:else}
      <Button variant="ghost" disabled={busy} onclick={() => (open = false)}>{t("common.cancel")}</Button>
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
  /* 白底黑码：暗色主题下也要能扫 */
  .qr {
    flex: none;
    width: 168px;
    height: 168px;
    overflow: hidden;
    border-radius: var(--r-sm);
    /* design-lint-allow raw-color: 二维码必须白底黑码，深色主题下也要能扫 */
    background: #fff;
    box-shadow: 0 0 0 var(--hairline) var(--border);
  }
  .qr svg {
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
    flex-wrap: wrap;
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
