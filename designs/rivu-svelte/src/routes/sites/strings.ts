import { extend } from "../../lib/i18n";

extend({
  // ---- 站点列表 ----
  "sl.search": ["搜索站点", "Search sites"],
  "sl.install": ["安装指引", "Install guide"],
  "sl.noMatch.title": ["没有匹配的站点", "No matching sites"],
  "sl.statsFailed": ["访客数据加载失败", "Couldn't load visitors"],
  "sl.openSite": ["打开 {name}", "Open {name}"],
  "sl.empty.viewer": ["站点由所有者或管理员添加。", "Sites are added by owners and admins."],
  "sl.deleted.title": ["已删除的站点", "Deleted sites"],
  "sl.deleted.name": ["名称", "Name"],
  "sl.deleted.purge": ["永久删除", "Purged"],
  "sl.deleted.days": ["{n} 天后永久删除", "Purged in {n} days"],
  "sl.deleted.today": ["今天永久删除", "Purged today"],
  "sl.deleted.restore": ["恢复", "Restore"],
  "sl.deleted.dlg.title": ["恢复 {name}？", "Restore {name}?"],
  "sl.deleted.dlg.body": ["站点恢复后重新开始接收事件。", "The site starts receiving events again."],
  "sl.deleted.restored": ["已恢复 {name}", "Restored {name}"],
  "sl.deleted.expired": ["恢复期已过，无法恢复", "The restore period has ended"],
  "sl.deleted.failed": ["恢复失败，请稍后重试", "Couldn't restore the site. Try again shortly."],

  // ---- 合并看板（词表：consolidated view → 合并看板 / Combined view） ----
  "sl.tab.list": ["列表", "List"],
  "cv.tab": ["合并看板", "Combined view"],
  "cv.visitorsSum": ["访客之和", "Visitors, summed"],
  "cv.visitors.def": [
    "把每个站点的访客数相加。同一个人访问了两个站点，会被算两次。",
    "Each site's visitors added up. Someone who visits two sites counts twice.",
  ],
  "cv.bySite": ["各站点", "By site"],
  "cv.col.site": ["站点", "Site"],
  "cv.col.share": ["访客占比", "Share of visitors"],

  // ---- 添加站点 ----
  "as.title": ["添加站点", "Add site"],
  "as.domain": ["域名", "Domain"],
  "as.domain.required": ["请填写域名。", "Enter a domain."],
  "as.domain.invalid": ["域名格式不正确，例如 example.com。", "Invalid domain, for example example.com."],
  "as.normalized": ["将统计 {host}", "Will track {host}"],
  "as.name": ["名称", "Name"],
  "as.timezone": ["时区", "Timezone"],
  "as.submit": ["创建站点", "Create site"],
  "as.created": ["已创建 {name}", "Created {name}"],
  "as.failed": ["创建失败，请稍后重试。", "Couldn't create the site. Try again shortly."],

  // ---- 安装 ----
  "in.title": ["安装 {name}", "Install {name}"],
  "in.copy": ["复制代码", "Copy the code"],
  "in.verify": ["验证安装", "Verify installation"],
  "in.method": ["安装方式", "Install method"],
  "in.method.site": ["站点脚本", "Site script"],
  "in.method.manual": ["手动 API", "Manual API"],
  "in.method.cdn": ["通用脚本 t.js", "Generic t.js"],
  "in.site.note": ["放进网站的 <head>。", "Add it to your site's <head>."],
  "in.manual.note": ["先放队列桩，再放站点脚本，之后在需要的地方调用 rivu()。", "Add the queue stub, then the site script, then call rivu() where needed."],
  "in.cdn.note": [
    "t.js 与站点无关，data-site 指定站点。放在自己的静态托管或 CDN 上时，用 data-api 指向 Rivu 的采集端点。",
    "t.js is site-agnostic; data-site selects the site. If you host it on your own static hosting or CDN, point data-api at Rivu's collection endpoint.",
  ],
  "in.code": ["安装代码", "Install code"],
  "in.exclude": [
    "排除自己的访问：在该浏览器的控制台运行 {cmd}。",
    "Exclude your own visits: run {cmd} in that browser's console.",
  ],
  "in.openDashboard": ["打开看板", "Open dashboard"],
  "in.verify.waiting": ["正在等待第一条事件。装好脚本后访问网站任意页面。", "Waiting for the first event. Visit any page once the script is installed."],
  "in.verify.ok": ["已收到事件，来自 {path}。", "Event received from {path}."],
});
