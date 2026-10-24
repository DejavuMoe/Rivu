<!-- 共享链接与公开看板的独立外壳：顶栏（标志、说明、主题切换）+ 内容，没有侧栏。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { Monitor, Moon, Sun } from "@lucide/svelte";
  import Logo from "../ui/Logo.svelte";
  import Segmented from "../ui/Segmented.svelte";
  import { t } from "../i18n";
  import { app, setTheme, type ThemePref } from "../state.svelte";
  import "./share-strings";

  interface Props {
    kind: "shared" | "public";
    children: Snippet;
  }
  let { kind, children }: Props = $props();
</script>

<div class="shell">
  <header class="bar">
    <div class="brand">
      <Logo size={20} />
      <span class="what">{kind === "public" ? t("share.brandPublic") : t("share.brand")}</span>
    </div>
    <Segmented
      size="sm"
      label={t("share.theme")}
      value={app.theme}
      onchange={(v: ThemePref) => setTheme(v)}
      options={[
        { value: "system", label: "", icon: Monitor, hint: t("theme.system") },
        { value: "light", label: "", icon: Sun, hint: t("theme.light") },
        { value: "dark", label: "", icon: Moon, hint: t("theme.dark") },
      ]}
    />
  </header>
  {@render children()}
</div>

<style>
  .shell {
    min-height: 100vh;
  }
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    height: 48px;
    padding: 0 var(--sp-8);
    border-bottom: var(--hairline) solid var(--border-subtle);
    background: var(--bg-app);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .what {
    font-weight: var(--fw-medium);
    white-space: nowrap;
  }
  @media (max-width: 720px) {
    .bar {
      padding: 0 var(--sp-4);
    }
  }
</style>
