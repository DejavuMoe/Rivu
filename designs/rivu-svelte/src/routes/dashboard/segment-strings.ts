import { extend } from "../../lib/i18n";

// 分群（词表：segment → 分群 / Segment）
extend({
  "seg.term": ["分群", "Segment"],
  "seg.menu": ["分群", "Segments"],
  "seg.active": ["分群：{name}", "Segment: {name}"],
  "seg.clear": ["不使用分群", "Stop using segment"],
  "seg.conditions": ["{n} 个条件", "{n} conditions"],
  "seg.condition": ["1 个条件", "1 condition"],
  "seg.none": ["还没有分群", "No segments yet"],
  "seg.none.hint": ["添加筛选后，可以把它们保存为分群。", "Add filters, then save them as a segment."],
  "seg.saveCurrent": ["保存当前筛选为分群", "Save filters as segment"],
  "seg.manage": ["管理分群", "Manage segments"],
  "seg.missing": ["链接里的分群已被删除，下面是不带分群的数据。", "The segment in this link was deleted. Showing data without it."],

  "seg.save.title": ["保存为分群", "Save as segment"],
  "seg.save.conditions": ["条件", "Conditions"],
  "seg.save.submit": ["保存分群", "Save segment"],
  "seg.save.done": ["已保存分群「{name}」", "Saved segment “{name}”"],
  "seg.save.failed": ["保存失败，请稍后重试", "Couldn't save. Try again shortly"],

  "seg.name": ["名称", "Name"],
  "seg.name.required": ["请填写名称", "Enter a name"],
  "seg.name.taken": ["这个站点已经有同名的分群", "This site already has a segment with this name"],

  "seg.col.name": ["名称", "Name"],
  "seg.col.conditions": ["条件", "Conditions"],
  "seg.rename": ["改名", "Rename"],
  "seg.rename.title": ["给「{name}」改名", "Rename “{name}”"],
  "seg.renamed": ["已改名", "Renamed"],
  "seg.delete": ["删除分群", "Delete segment"],
  "seg.delete.title": ["删除「{name}」？", "Delete “{name}”?"],
  "seg.delete.body": ["站点里的所有人都将不能再使用这个分群，删除后不能恢复。", "It's removed for everyone on this site and can't be restored."],
  "seg.delete.done": ["已删除分群「{name}」", "Deleted segment “{name}”"],
  "seg.blocked.title": ["不能删除「{name}」", "Can't delete “{name}”"],
  "seg.blocked.body": [
    "共享链接{links}正在使用这个分群。先撤销这个链接，再删除分群。",
    "The shared link {links} uses this segment. Revoke it first, then delete the segment.",
  ],
  "seg.blocked.bodyMany": [
    "共享链接{links}正在使用这个分群。先撤销这些链接，再删除分群。",
    "The shared links {links} use this segment. Revoke them first, then delete the segment.",
  ],
  "seg.blocked.open": ["打开共享链接", "Open shared links"],
});
