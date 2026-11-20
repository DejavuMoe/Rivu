export interface Series {
  key: string;
  label: string;
  values: (number | null)[];
  /** CSS 颜色，默认按顺序取 --series-N；对比期序列忽略这个字段 */
  color?: string;
  compare?: boolean;
  /** 提示里这个序列对应的时间（对比期的日期与主序列不同） */
  labels?: string[];
}

/** 趋势图上的一个标记（如图表批注）：落在第 index 个点上。 */
export interface ChartMark {
  index: number;
  /** 这个点上有几条（大于 1 时标记上显示数字） */
  count: number;
  /** 读屏读到的名字 */
  label: string;
  /** 悬停或键盘移到这个点时，提示框里列出的文字 */
  lines: string[];
}

export interface BarRow {
  key: string;
  label: string;
  sub?: string;
  value: number;
  /** 对比期的值（显示变化） */
  prev?: number;
  /** 行首的小图标、国旗等 */
  code?: string;
  /** 主数值右侧的附加列（已格式化），与 BarList 的 extras 表头对应 */
  extra?: string[];
}
