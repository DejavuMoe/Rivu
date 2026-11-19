<!--
  评审面板（只在原型里）：切换角色、数据状态、安装检测结果等，让评审者看到各种状态。
  右下角浮动按钮打开；URL 带 ?capture=1 时隐藏（截图用）。
-->
<script lang="ts">
  import { SlidersHorizontal, X } from "@lucide/svelte";
  import Segmented from "../ui/Segmented.svelte";
  import Button from "../ui/Button.svelte";
  import { app, setLang, setReview, setTheme, type DataState, type Role } from "../state.svelte";
  import { navigate } from "../router.svelte";
  import { weekly, type WeeklyEmail } from "../../routes/account/weekly.svelte";

  let open = $state(false);

  const pages: [string, string][] = [
    ["/login", "登录"],
    ["/forgot-password", "找回密码"],
    ["/reset-password/t0k3n", "重设密码"],
    ["/reset-password/expired", "重设链接失效"],
    ["/setup", "初始化"],
    ["/invite/t0k3n", "接受邀请"],
    ["/sites", "站点"],
    ["/sites/new", "添加站点"],
    ["/s/4/install", "安装"],
    ["/s/1", "看板"],
    ["/s/1/diagnostics", "诊断"],
    ["/s/1/settings/general", "设置"],
    ["/s/10", "访客站点"],
    ["/org/members", "成员"],
    ["/org/api-keys", "API 密钥"],
    ["/account", "账号"],
    ["/weekly-report/confirm/t0k3n", "周报确认链接"],
    ["/weekly-report/confirm/expired", "周报链接失效"],
    ["/share/9f3k2m7q8w1x4z6c5v0b2n8m", "共享链接"],
    ["/share/locked-7h2x9d4k", "共享链接（要密码）"],
    ["/share/missing", "链接无效"],
    ["/public/d4pl6nq2wzce", "公开看板"],
  ];
</script>

<div class="review">
  {#if open}
    <div class="panel" role="dialog" aria-label="评审">
      <div class="head">
        <strong>原型评审</strong>
        <button type="button" class="x" aria-label="关闭" onclick={() => (open = false)}><X size={15} /></button>
      </div>
      <div class="row">
        <span>角色</span>
        <Segmented
          size="sm"
          label="角色"
          value={app.review.role}
          onchange={(v: Role) => setReview({ role: v })}
          options={[
            { value: "owner", label: "所有者" },
            { value: "admin", label: "管理员" },
            { value: "editor", label: "编辑者" },
            { value: "viewer", label: "查看者" },
          ]}
        />
      </div>
      <div class="row">
        <span>数据状态</span>
        <Segmented
          size="sm"
          label="数据状态"
          value={app.review.dataState}
          onchange={(v: DataState) => setReview({ dataState: v })}
          options={[
            { value: "ready", label: "正常" },
            { value: "loading", label: "加载" },
            { value: "empty", label: "无数据" },
            { value: "error", label: "出错" },
            { value: "busy", label: "排队" },
            { value: "timeout", label: "超时" },
          ]}
        />
      </div>
      <div class="row">
        <span>应用加载</span>
        <Segmented
          size="sm"
          label="应用加载"
          value={app.review.shell}
          onchange={(v) => setReview({ shell: v })}
          options={[
            { value: "ready", label: "正常" },
            { value: "failed", label: "失败" },
          ]}
        />
      </div>
      <div class="row">
        <span>安装检测</span>
        <Segmented
          size="sm"
          label="安装检测"
          value={app.review.install}
          onchange={(v) => setReview({ install: v })}
          options={[
            { value: "waiting", label: "等待" },
            { value: "received", label: "已收到" },
          ]}
        />
      </div>
      <div class="row">
        <span>登录结果</span>
        <Segmented
          size="sm"
          label="登录结果"
          value={app.review.login}
          onchange={(v) => setReview({ login: v })}
          options={[
            { value: "ok", label: "成功" },
            { value: "invalid", label: "密码错误" },
            { value: "rateLimited", label: "尝试过多" },
            { value: "totp", label: "要验证码" },
            { value: "network", label: "断网" },
            { value: "failed", label: "失败" },
          ]}
        />
      </div>
      <div class="row">
        <span>验证码步骤</span>
        <Segmented
          size="sm"
          label="验证码步骤"
          value={app.review.loginCode}
          onchange={(v) => setReview({ loginCode: v })}
          options={[
            { value: "ok", label: "正常" },
            { value: "expired", label: "已超时" },
            { value: "unavailable", label: "不可用" },
            { value: "rateLimited", label: "尝试过多" },
            { value: "failed", label: "失败" },
          ]}
        />
      </div>
      <div class="row">
        <span>邮件</span>
        <Segmented
          size="sm"
          label="邮件"
          value={app.review.mail ? "on" : "off"}
          onchange={(v) => setReview({ mail: v === "on" })}
          options={[
            { value: "on", label: "已配置" },
            { value: "off", label: "未配置" },
          ]}
        />
      </div>
      <div class="row">
        <span>两步验证</span>
        <Segmented
          size="sm"
          label="两步验证"
          value={app.review.twofa}
          onchange={(v) => setReview({ twofa: v })}
          options={[
            { value: "off", label: "未开启" },
            { value: "on", label: "已开启" },
            { value: "unavailable", label: "不可用" },
          ]}
        />
      </div>
      <div class="row">
        <span>初始化</span>
        <Segmented
          size="sm"
          label="初始化"
          value={app.review.setup}
          onchange={(v) => setReview({ setup: v })}
          options={[
            { value: "token", label: "有令牌" },
            { value: "open", label: "无令牌" },
            { value: "done", label: "已初始化" },
          ]}
        />
      </div>
      <div class="row">
        <span>邀请页</span>
        <Segmented
          size="sm"
          label="邀请页"
          value={app.review.invite}
          onchange={(v) => setReview({ invite: v })}
          options={[
            { value: "loading", label: "加载" },
            { value: "invalid", label: "无效" },
            { value: "failed", label: "出错" },
            { value: "new", label: "新账号" },
            { value: "existing", label: "已登录" },
            { value: "other", label: "他人" },
            { value: "signedOut", label: "未登录" },
          ]}
        />
      </div>
      <div class="row">
        <span>邀请范围</span>
        <Segmented
          size="sm"
          label="邀请范围"
          value={app.review.inviteScope}
          onchange={(v) => setReview({ inviteScope: v })}
          options={[
            { value: "org", label: "组织" },
            { value: "site", label: "单站点" },
          ]}
        />
      </div>
      <div class="row">
        <span>已删除站点</span>
        <Segmented
          size="sm"
          label="已删除站点"
          value={app.review.deleted}
          onchange={(v) => setReview({ deleted: v })}
          options={[
            { value: "some", label: "有" },
            { value: "none", label: "无" },
          ]}
        />
      </div>
      <div class="row">
        <span>周报邮箱</span>
        <Segmented
          size="sm"
          label="周报邮箱"
          value={weekly.email}
          onchange={(v: WeeklyEmail) => (weekly.email = v)}
          options={[
            { value: "unconfirmed", label: "未确认" },
            { value: "pending", label: "待确认" },
            { value: "confirmed", label: "已确认" },
          ]}
        />
      </div>
      <div class="row">
        <span>周报确认邮件</span>
        <Segmented
          size="sm"
          label="周报确认邮件"
          value={app.review.weeklyMail}
          onchange={(v) => setReview({ weeklyMail: v })}
          options={[
            { value: "ok", label: "发出" },
            { value: "fails", label: "发送失败" },
          ]}
        />
      </div>
      <div class="row">
        <span>语言与主题</span>
        <div class="pair">
          <Segmented size="sm" label="语言" value={app.lang} onchange={setLang} options={[{ value: "zh", label: "中" }, { value: "en", label: "EN" }]} />
          <Segmented
            size="sm"
            label="主题"
            value={app.theme}
            onchange={setTheme}
            options={[
              { value: "system", label: "系统" },
              { value: "light", label: "浅" },
              { value: "dark", label: "深" },
            ]}
          />
        </div>
      </div>
      <div class="pages">
        {#each pages as [p, l] (p)}
          <Button size="sm" variant="ghost" onclick={() => navigate(p)}>{l}</Button>
        {/each}
      </div>
    </div>
  {/if}
  <Button icon={SlidersHorizontal} active={open} onclick={() => (open = !open)}>评审</Button>
</div>

<style>
  .review {
    position: fixed;
    right: 16px;
    bottom: 16px;
    z-index: 90;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 8px;
  }
  .panel {
    width: min(460px, calc(100vw - 32px));
    max-height: calc(100vh - 88px);
    overflow-y: auto;
    padding: 12px 14px;
    border-radius: var(--r-lg);
    background: var(--bg-app);
    box-shadow: var(--shadow-pop);
    font-size: var(--fs-md);
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 8px;
  }
  .x {
    border: 0;
    background: none;
    color: var(--text-muted);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 6px 0;
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .row > span {
    color: var(--text-muted);
    white-space: nowrap;
  }
  .pair {
    display: flex;
    gap: 6px;
  }
  .pages {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    margin-top: 6px;
    padding-top: 8px;
    border-top: var(--hairline) solid var(--border-subtle);
  }
</style>
