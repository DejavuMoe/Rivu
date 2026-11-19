<!--
  可搜索的时区选择：Popover + 搜索框 + 选项列表（listbox）。只列 15 个常见 IANA 时区，
  浏览器时区不在列表里时放在最前面。UTC 偏移由 Intl 计算。
-->
<script lang="ts">
  import { ChevronDown, Search } from "@lucide/svelte";
  import Popover from "../../lib/ui/Popover.svelte";
  import MenuItem from "../../lib/ui/MenuItem.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import { t } from "../../lib/i18n";
  import "./strings";

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

  function offset(tz: string) {
    try {
      const p = new Intl.DateTimeFormat("en-US", { timeZone: tz, timeZoneName: "shortOffset" }).formatToParts(new Date(2026, 9, 24));
      const v = p.find((x) => x.type === "timeZoneName")?.value ?? "";
      return v.replace("GMT", "UTC") || "UTC";
    } catch {
      return "";
    }
  }

  const zones = $derived.by(() => {
    const b = browserZone();
    return (COMMON.includes(b) ? COMMON : [b, ...COMMON]).map((z) => ({ z, off: offset(z), browser: z === b }));
  });

  let q = $state("");
  let open = $state(false);
  const list = $derived.by(() => {
    const k = q.trim().toLowerCase().replace(/\s+/g, "_");
    return k ? zones.filter((x) => x.z.toLowerCase().includes(k) || x.off.toLowerCase().includes(k)) : zones;
  });
  const current = $derived(zones.find((x) => x.z === value));

  $effect(() => {
    if (!open) q = "";
  });
</script>

<div class="tzwrap">
<Popover bind:open width="trigger" role="listbox" label={t("as.timezone")}>
  {#snippet trigger({ toggle, props })}
    <button type="button" class="tz" {id} onclick={toggle} {...props}>
      <span class="val">{value}</span>
      <span class="off">{current?.off ?? offset(value)}</span>
      <ChevronDown size={14} strokeWidth={1.75} aria-hidden="true" />
    </button>
  {/snippet}
  {#snippet children({ close })}
    <div class="find">
      <Input icon={Search} size="sm" data-autofocus placeholder={t("as.timezone.search")} aria-label={t("as.timezone.search")} bind:value={q} />
    </div>
    {#each list as x (x.z)}
      <MenuItem
        role="option"
        checked={x.z === value}
        hint={x.browser ? t("as.timezone.browser") : undefined}
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
      <p class="none">{t("as.timezone.none")}</p>
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

