import { extend } from "#lib/i18n.ts";

extend({
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
