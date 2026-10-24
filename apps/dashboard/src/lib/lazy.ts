// 列表里按需加载：元素进入视口（含上下各一屏的余量）才开始，同时在途的任务有上限。
// 站点列表每张卡片要查两次，站点多时一次全发会把查询并发打满。

/** 同时最多运行 n 个任务的队列；signal 取消时，还没开始的任务直接丢弃。 */
export function limiter(n: number) {
  let running = 0;
  const waiting: (() => void)[] = [];
  const next = () => {
    if (running >= n) return;
    const job = waiting.shift();
    if (job) job();
  };
  return function run<T>(task: () => Promise<T>, signal: AbortSignal): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const start = () => {
        if (signal.aborted) {
          reject(new DOMException("aborted", "AbortError"));
          next();
          return;
        }
        running++;
        task()
          .then(resolve, reject)
          .finally(() => {
            running--;
            next();
          });
      };
      waiting.push(start);
      next();
    });
  };
}

/** Svelte action：元素第一次进入视口时调用 onvisible，之后不再观察。 */
export function inview(node: Element, onvisible: () => void) {
  const io = new IntersectionObserver(
    (entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        io.disconnect();
        onvisible();
      }
    },
    { rootMargin: "100% 0px" },
  );
  io.observe(node);
  return { destroy: () => io.disconnect() };
}
