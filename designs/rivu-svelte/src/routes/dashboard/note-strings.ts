import { extend } from "../../lib/i18n";

// 图表批注（词表：annotation → 批注 / Note）
extend({
  "note.add": ["添加批注", "Add note"],
  "note.mark": ["{date}：{n} 条批注", "{date}: {n} notes"],
  "note.markOne": ["{date}：1 条批注", "{date}: 1 note"],
  "note.more": ["还有 {n} 条", "{n} more"],
  "note.col": ["批注", "Notes"],
  "note.date": ["日期", "Date"],
  "note.text": ["内容", "Note"],
  "note.text.required": ["请填写内容", "Enter a note"],
  "note.date.required": ["请选择日期", "Choose a date"],
  "note.save": ["保存批注", "Save note"],
  "note.added": ["已添加批注", "Note added"],
  "note.updated": ["已保存", "Saved"],
  "note.deleted": ["已删除批注", "Note deleted"],
  "note.failed": ["保存失败，请稍后重试", "Couldn't save. Try again shortly"],
  "note.edit": ["修改批注", "Edit note"],
  "note.delete": ["删除批注", "Delete note"],
  "note.deleteAsk": ["删除这条批注？", "Delete this note?"],
  "note.full": [
    "这个站点的批注已达到 500 条上限。删除一些旧批注后再添加。",
    "This site has reached the limit of 500 notes. Delete some older notes to add more.",
  ],
});
