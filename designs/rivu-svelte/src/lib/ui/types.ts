import type { Component } from "svelte";

/** lucide 图标组件（或任何接受 size / strokeWidth 的图标组件）。 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type IconType = Component<any>;

export type Tone = "neutral" | "accent" | "good" | "warn" | "bad" | "info";

export interface Column {
  key: string;
  label: string;
  align?: "left" | "right" | "center";
  width?: string;
  /** 窄屏时隐藏 */
  optional?: boolean;
}

export interface Option<V extends string = string> {
  value: V;
  label: string;
  icon?: IconType;
  hint?: string;
  disabled?: boolean;
}
