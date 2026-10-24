<!--
  可搜索的时区选择：Popover + 搜索框 + 选项列表（listbox）。不搜索时只列常用时区，
  搜索时在浏览器支持的全部 IANA 时区里找。浏览器时区不在常用列表里时放在最前面。UTC 偏移由 Intl 计算。
-->
<script lang="ts">
  import { ChevronDown, Search } from "@lucide/svelte";
  import Popover from "../ui/Popover.svelte";
  import MenuItem from "../ui/MenuItem.svelte";
  import Input from "../ui/Input.svelte";
  import { t } from "../i18n";
  import "./shell-strings";

  interface Props {
    value: string;
    id?: string;
    onchange?: (v: string) => void;
  }
  let { value = $bindable(), id, onchange }: Props = $props();

  const COMMON = [
    "UTC",
    "Asia/Shanghai",
    "Asia/Hong_Kong",
    "Asia/Taipei",
    "Asia/Tokyo",
    "Asia/Seoul",
    "Asia/Singapore",
    "Asia/Kolkata",
    "Europe/London",
    "Europe/Berlin",
    "Europe/Paris",
    "America/New_York",
    "America/Chicago",
    "America/Los_Angeles",
    "Australia/Sydney",
  ];

  function browserZone() {
    try {
      return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    } catch {
      return "UTC";
    }
  }

  function allZones(): string[] {
    try {
      const all = (Intl as unknown as { supportedValuesOf(k: string): string[] }).supportedValuesOf("timeZone");
      return all.includes("UTC") ? all : ["UTC", ...all];
    } catch {
      return COMMON;
    }
  }

  function offset(tz: string) {
    try {
      const p = new Intl.DateTimeFormat("en-US", { timeZone: tz, timeZoneName: "shortOffset" }).formatToParts(new Date());
      const v = p.find((x) => x.type === "timeZoneName")?.value ?? "";
      return v.replace("GMT", "UTC") || "UTC";
    } catch {
      return "";
    }
  }

  const browser = browserZone();
  const common = COMMON.includes(browser) ? COMMON : [browser, ...COMMON];
  const everything = allZones();

  let q = $state("");
  let open = $state(false);
  // 当前值不在列表里（旧数据或别名）时也要能显示
  const base = $derived(common.includes(value) ? common : [value, ...common]);
  const list = $derived.by(() => {
    const k = q.trim().toLowerCase().replace(/\s+/g, "_");
    const zones = k ? everything.filter((z) => z.toLowerCase().includes(k)).slice(0, 50) : base;
    return zones.map((z) => ({ z, off: offset(z), browser: z === browser }));
  });

  $effect(() => {
    if (!open) q = "";
  });
</script>

<div class="tzwrap">
  <Popover bind:open width="trigger" role="listbox" label={t("tz.label")}>
    {#snippet trigger({ toggle, props })}
      <button type="button" class="tz" {id} onclick={toggle} {...props}>
        <span class="val">{value}</span>
        <span class="off">{offset(value)}</span>
        <ChevronDown size={14} strokeWidth={1.75} aria-hidden="true" />
      </button>
    {/snippet}
    {#snippet children({ close })}
      <div class="find">
        <Input icon={Search} size="sm" data-autofocus placeholder={t("tz.search")} aria-label={t("tz.search")} bind:value={q} />
      </div>
      {#each list as x (x.z)}
        <MenuItem
          role="option"
          checked={x.z === value}
          hint={x.browser ? t("tz.browser") : undefined}
          onclick={() => {
            value = x.z;
            onchange?.(x.z);
            close();
          }}
        >
          {x.z}
          <span class="opt-off">{x.off}</span>
        </MenuItem>
      {:else}
        <p class="none">{t("tz.none")}</p>
      {/each}
    {/snippet}
  </Popover>
</div>

<style>
  .tzwrap :global(.popover) {
    display: flex;
    width: 100%;
  }
  .tz {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    height: var(--control-md);
    padding: 0 10px;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-sm);
    background: var(--bg-app);
    text-align: left;
  }
  .tz:hover {
    border-color: var(--border-hover);
  }
  .tz[aria-expanded="true"] {
    border-color: var(--accent-focus);
  }
  .val {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .off {
    color: var(--text-muted);
    font-size: var(--fs-md);
    font-variant-numeric: tabular-nums;
  }
  .tz :global(svg) {
    color: var(--text-muted);
    flex: none;
  }
  .find {
    padding: 2px 2px 6px;
  }
  .opt-off {
    margin-left: 8px;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .none {
    padding: 12px 8px;
    color: var(--text-muted);
    font-size: var(--fs-md);
    text-align: center;
  }
</style>
