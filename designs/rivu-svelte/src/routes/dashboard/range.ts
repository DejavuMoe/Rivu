// 时间范围控件（RangeControl）要的东西：按钮文字、上一周期 / 下一周期、自定义对话框的初始值。
// 站点看板与合并看板共用；区间换算在 period.ts。

import { NOW } from "../../lib/data/fixtures";
import { t } from "../../lib/i18n";
import type { Lang } from "../../lib/state.svelte";
import { canStep, periodText, resolve, step, ymd, type Period, type RangeParam } from "./period";

export interface RangeUi {
  rangeText: string;
  prev: RangeParam | null | undefined;
  next: RangeParam | null | undefined;
  custom: { from: string; to: string; today: string; timezone: string };
}

/** p：URL 里的范围；period：查询用到的区间（终点已封顶到今天）；first：最早有数据的本地日。 */
export function rangeUi(p: RangeParam, period: Period, first: Date, timezone: string, lang: Lang): RangeUi {
  const stepper = canStep(p.range);
  // 时刻区间（过去 24 小时平移出来的）、实时：对话框从它所在的日期开始
  const r = resolve(p, NOW, first);
  return {
    rangeText: p.range === "custom" ? periodText(period, lang, NOW) : t(`range.${p.range}`),
    prev: stepper ? step(p, -1, NOW, first) : undefined,
    next: stepper ? step(p, 1, NOW, first) : undefined,
    custom: { from: ymd(r.from), to: ymd(period.kind === "instant" ? r.to : period.to), today: ymd(NOW), timezone },
  };
}
