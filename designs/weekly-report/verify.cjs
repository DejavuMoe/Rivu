const assert = require('node:assert/strict');
const { readFileSync, writeFileSync } = require('node:fs');
const { createServer } = require('node:http');
const { join } = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE ?? 'playwright');

const files = { '/email.html': 'email.html' };
const pages = Object.fromEntries(Object.values(files).map((name) => [name, readFileSync(join(__dirname, name))]));
const output = process.env.REVIEW_OUTPUT;
const collector = process.env.COLLECTOR_PATH;
const TOKEN = /^[A-Za-z0-9_-]{43}$/;
const CONFIRM = /^https:\/\/rivu\.example\.com\/weekly-report\/confirm\/[A-Za-z0-9_-]{43}$/;
const UNSUBSCRIBE = /^https:\/\/rivu\.example\.com\/api\/weekly-report\/unsubscribe\/[A-Za-z0-9_-]{43}$/;
const UNSUBSCRIBE_LABEL = { zh: '退订周报', en: 'Unsubscribe' };
const redactPatterns = ['[A-Za-z0-9_-]{43}'];

function watch(page, address) {
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  page.on('console', (message) => { if (message.type() === 'error') errors.push(message.text()); });
  page.on('request', (request) => {
    const url = request.url();
    if (!url.startsWith(address) && !url.startsWith('about:') && !url.startsWith('data:')) errors.push(url);
  });
  return errors;
}

function collect(page, captures) {
  return async () => {
    assert(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
    if (collector) captures.push(await page.evaluate((patterns) => window.__prototypeFirstUICollectDOMContent({ redactPatterns: patterns }), redactPatterns));
  };
}

function save(name, captures) {
  if (!captures.length || !output) return;
  const items = new Map(captures.flatMap(({ items }) => items.map((item) => [JSON.stringify(item), item])));
  writeFileSync(join(output, `${name}.json`), JSON.stringify({ ...captures[0], items: [...items.values()] }));
}

function expectedCurrent(kind, f, lang, confirm, unsubscribe) {
  const link = `https://rivu.example.com/s/${f.id}`;
  if (kind === 'confirmation') {
    return lang === 'zh'
      ? ['确认接收 Rivu 周报', `点下面的链接，确认用这个邮箱接收 Rivu 周报。链接 1 小时内有效，只能用一次。\n\n${confirm}\n\n如果不是你本人操作，忽略这封邮件即可。\n`]
      : ['Confirm your email for Rivu weekly reports', `Open this link to confirm that this address should receive Rivu weekly reports. The link works once and expires in 1 hour.\n\n${confirm}\n\nIf you didn't ask for this, you can ignore this email.\n`];
  }
  return lang === 'zh'
    ? ['Rivu 周报', `${f.site}\n${f.from} 至 ${f.to}（${f.tz}）\n\n访客：${f.visitors}\n访问：${f.visits}\n浏览量：${f.pageviews}\n\n查看仪表盘：${link}\n退订周报：${unsubscribe}\n`]
    : ['Rivu weekly report', `${f.site}\n${f.from} to ${f.to} (${f.tz})\n\nVisitors: ${f.visitors}\nVisits: ${f.visits}\nPageviews: ${f.pageviews}\n\nView dashboard: ${link}\nUnsubscribe: ${unsubscribe}\n`];
}

async function emails(page, address, mobile) {
  const errors = watch(page, address);
  if (collector) await page.addInitScript({ path: collector });
  const control = (id) => page.locator(`#${id}`);
  const captures = [];
  const capture = collect(page, captures);
  await page.goto(`${address}email.html`);
  await control('preview').locator('summary').click();
  const frame = page.frameLocator('#message');
  const api = await page.evaluateHandle(() => window.__weeklyReportEmail);
  const fixtures = await api.evaluate((x) => x.fixtures);
  const token = await api.evaluate((x) => x.sampleToken);
  assert.match(token, TOKEN);
  const confirm = await api.evaluate((x) => x.confirmUrl);
  assert.match(confirm, CONFIRM);
  assert(confirm.endsWith(`/${token}`));
  const unsubscribe = await api.evaluate((x) => x.unsubscribeUrl);
  assert.match(unsubscribe, UNSUBSCRIBE);
  // The report carries the List-Unsubscribe header in every version; the confirmation email has none.
  const header = async (kind) => {
    assert.equal(await control('list-unsubscribe-row').isHidden(), kind === 'confirmation');
    if (kind === 'report') assert.equal(await control('list-unsubscribe').innerText(), `<${unsubscribe}>`);
  };

  const expected = {
    zh: {
      studio: ['studio.example.com 周报：11 月 16 日至 22 日', '2026 年 11 月 16 日至 22 日（Asia/Taipei）', ['1,284', '1,702', '4,315']],
      quiet: ['docs.example.com 周报：11 月 15 日至 21 日', '2026 年 11 月 15 日至 21 日（Asia/Taipei）', ['0', '0', '0']],
      large: ['shop.example.com 周报：2026 年 12 月 28 日至 2027 年 1 月 3 日', '2026 年 12 月 28 日至 2027 年 1 月 3 日（America/New_York）', ['12,840,310', '18,755,902', '96,302,417']],
    },
    en: {
      studio: ['studio.example.com weekly report: Nov 16 – 22', 'Nov 16 – 22, 2026 (Asia/Taipei)', ['1,284', '1,702', '4,315']],
      quiet: ['docs.example.com weekly report: Nov 15 – 21', 'Nov 15 – 21, 2026 (Asia/Taipei)', ['0', '0', '0']],
      large: ['shop.example.com weekly report: Dec 28, 2026 – Jan 3, 2027', 'Dec 28, 2026 – Jan 3, 2027 (America/New_York)', ['12,840,310', '18,755,902', '96,302,417']],
    },
  };
  const normalize = (value) => value.replace(/[   ]/g, ' ');

  for (const lang of ['zh', 'en']) {
    await control('language').selectOption(lang === 'zh' ? 'zh-CN' : 'en');
    for (const kind of ['report', 'confirmation']) {
      await control('mail').selectOption(kind);
      for (const fixture of kind === 'report' ? ['studio', 'quiet', 'large'] : ['studio']) {
        if (kind === 'report') await control('fixture').selectOption(fixture);
        const f = fixtures[fixture];
        for (const width of ['600', '360']) {
          for (const scheme of ['light', 'dark']) {
            await control('width').selectOption(width);
            await control('scheme').selectOption(scheme);
            await control('version').selectOption('html');
            const html = await api.evaluate((x, [k, f, l]) => (k === 'report' ? x.reportHtml(f, l) : x.confirmHtml(l)), [kind, f, lang]);
            assert(Buffer.byteLength(html) < 100_000);
            assert(!/<script|<link|@import|url\(/i.test(html));
            for (const [, href] of html.matchAll(/href="([^"]+)"/g)) assert.match(href, /^https:\/\//);
            const marker = kind === 'report' ? f.site : (lang === 'zh' ? '确认接收周报' : 'Confirm your email');
            await page.waitForFunction(([m, l]) => {
              const doc = document.querySelector('#message').contentDocument;
              return doc?.documentElement.lang === l && doc.body?.innerText.includes(m);
            }, [marker, lang === 'zh' ? 'zh-CN' : 'en']);
            const body = frame.locator('body');
            const text = normalize(await body.innerText());
            if (kind === 'report') {
              const [subject, range, values] = expected[lang][fixture];
              assert.equal(normalize(await control('subject').innerText()), subject);
              assert(text.includes(range), `${range} in ${text}`);
              for (const value of values) assert(text.includes(value), `${value} in ${text}`);
              assert.equal(text.includes(lang === 'zh' ? '这一周没有记录到访问。' : 'No visits were recorded this week.'), fixture === 'quiet');
              assert.equal(await frame.locator(`a[href="https://rivu.example.com/account"]`).count(), 1);
              assert.equal(await frame.locator(`a[href="${unsubscribe}"]`).count(), 1);
              assert.equal(await frame.locator(`a[href="${unsubscribe}"]`).innerText(), UNSUBSCRIBE_LABEL[lang]);
              assert.equal(await frame.locator('a').count(), 3);
            } else {
              assert.equal(await control('subject').innerText(), lang === 'zh' ? '确认接收 Rivu 周报' : 'Confirm your email for Rivu weekly reports');
              assert(text.includes(lang === 'zh' ? '链接 1 小时内有效，只能用一次。' : 'The link works once and expires in 1 hour.'));
              assert(text.includes(lang === 'zh' ? '如果不是你本人操作，忽略这封邮件即可。' : 'ignore this email'));
              assert(!/unsubscribe|退订|代码|确认码|code/i.test(text));
              assert(!/unsubscribe|退订/i.test(html));
              // One button, and its target is the confirmation link; opening the email changes nothing.
              assert.equal(await frame.locator('a').count(), 1);
              assert.equal(await frame.locator('a').getAttribute('href'), confirm);
              assert.equal(await frame.locator('a').innerText(), lang === 'zh' ? '确认邮箱' : 'Confirm email');
            }
            await header(kind);
            const fits = await frame.locator('html').evaluate((element) => element.scrollWidth <= element.clientWidth + 1);
            assert(fits, `${kind} ${lang} ${fixture} ${width} overflows`);
            const background = await frame.locator('.card').first().evaluate((element) => getComputedStyle(element).backgroundColor);
            assert.equal(background, scheme === 'dark' ? 'rgb(25, 25, 25)' : 'rgb(252, 252, 252)');
            if (width === '600' && scheme === 'light') await capture();
            if (output && fixture === 'studio' && lang === 'zh' && scheme === 'light' && width === (mobile ? '360' : '600')) {
              await page.screenshot({ path: join(output, `email-${kind}-${width}.png`), fullPage: true });
            }
            if (output && mobile && fixture === 'large' && lang === 'en' && width === '360' && scheme === 'dark') {
              await page.screenshot({ path: join(output, 'email-report-large-360-dark.png'), fullPage: true });
            }
          }
        }
        await control('width').selectOption('600');
        await control('scheme').selectOption('light');
        await control('version').selectOption('text');
        const plain = normalize(await control('plain').innerText());
        if (kind === 'report') {
          const [, range, values] = expected[lang][fixture];
          assert(plain.includes(range));
          for (const value of values) assert(plain.includes(value));
          assert(plain.includes(`${lang === 'zh' ? '管理周报：' : 'Manage weekly report: '}https://rivu.example.com/account`));
          assert(!plain.includes('/settings/'));
          assert(plain.includes(`${UNSUBSCRIBE_LABEL[lang]}${lang === 'zh' ? '：' : ': '}${unsubscribe}`));
        } else {
          assert(plain.includes(`${lang === 'zh' ? '确认邮箱：' : 'Confirm email: '}${confirm}`));
          assert(!/unsubscribe|退订/i.test(plain));
        }
        assert(await control('preheader-row').isHidden());
        await header(kind);
        await capture();
        await control('version').selectOption('current');
        const [subject, body] = expectedCurrent(kind, f, lang, confirm, unsubscribe);
        assert.equal(await control('subject').innerText(), subject);
        assert.equal(await control('plain').evaluate((element) => element.textContent), body);
        await header(kind);
        await capture();
        await control('version').selectOption('html');
      }
    }
  }

  // The current unsubscribe pages are unstyled, bilingual and fixed: no style, script, image or link, and no form action.
  await control('language').selectOption('zh-CN');
  await control('mail').selectOption('unsubscribe');
  assert(await control('envelope').isHidden());
  assert(await control('version-field').isHidden());
  assert.equal(await control('unsubscribe-frame').getAttribute('sandbox'), 'allow-same-origin');
  const notice = page.frameLocator('#unsubscribe-frame');
  const pages = {
    confirm: ['Unsubscribe · Rivu', ['Unsubscribe from Rivu weekly reports', '退订 Rivu 周报', 'Unsubscribe · 退订'], 2],
    done: ['Rivu weekly reports', ['You have unsubscribed from Rivu weekly reports for this site.', '你已退订此站点的 Rivu 周报。'], 1],
    expired: ['Rivu weekly reports', ['This link has expired or the subscription is already off.', '退订链接已过期，或订阅已关闭。'], 1],
  };
  for (const [name, [title, visible, zh]] of Object.entries(pages)) {
    await control('unsubscribe-page').selectOption(name);
    await page.waitForFunction((value) => document.querySelector('#unsubscribe-frame').contentDocument?.title === value, title);
    assert.equal(await control('unsubscribe-frame').getAttribute('title'), title);
    assert(!/<style|<script|<img|<link|<iframe|style=|src=|href=|@import|url\(/i.test(await control('unsubscribe-frame').getAttribute('srcdoc')));
    assert.equal(await notice.locator('html').getAttribute('lang'), 'en');
    assert.deepEqual((await notice.locator('h1, p, button').allInnerTexts()).map(normalize), visible);
    assert.equal(await notice.locator('[lang="zh"]').count(), zh);
    assert.equal(await notice.locator('form[method="post"]').count(), name === 'confirm' ? 1 : 0);
    assert.equal(await notice.locator('form[action], a').count(), 0);
    assert(await notice.locator('html').evaluate((element) => element.scrollWidth <= element.clientWidth + 1), `${name} overflows`);
    await capture();
    if (output && !mobile) await page.screenshot({ path: join(output, `email-unsubscribe-${name}.png`), fullPage: true });
  }
  assert.deepEqual(errors, []);
  save(`email-${mobile ? 'mobile' : 'desktop'}`, captures);
}

async function main() {
  const server = createServer((request, response) => {
    const name = files[new URL(request.url, 'http://x').pathname];
    if (!name) {
      response.statusCode = 404;
      response.end();
      return;
    }
    response.setHeader('Content-Type', 'text/html; charset=utf-8');
    response.end(pages[name]);
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = `http://127.0.0.1:${server.address().port}/`;
  try {
    const browser = await chromium.launch();
    try {
      await emails(await browser.newPage({ viewport: { width: 1440, height: 900 } }), address, false);
      await emails(await browser.newPage({ viewport: { width: 360, height: 800 }, isMobile: true, hasTouch: true, colorScheme: 'dark' }), address, true);
      const auto = await browser.newPage({ colorScheme: 'dark' });
      await auto.goto(`${address}email.html`);
      const html = await auto.evaluate(() => window.__weeklyReportEmail.reportHtml(window.__weeklyReportEmail.fixtures.studio, 'zh'));
      assert.match(html, /@media \(prefers-color-scheme: dark\)/);
      await auto.setContent(html);
      assert.equal(await auto.locator('.card').evaluate((element) => getComputedStyle(element).backgroundColor), 'rgb(25, 25, 25)');
      await auto.emulateMedia({ colorScheme: 'light' });
      assert.equal(await auto.locator('.card').evaluate((element) => getComputedStyle(element).backgroundColor), 'rgb(252, 252, 252)');
      await auto.close();
      const offline = await browser.newPage({ offline: true, viewport: { width: 320, height: 800 } });
      await offline.goto(pathToFileURL(join(__dirname, 'email.html')).href);
      await offline.frameLocator('#message').locator('h1').waitFor();
      assert(await offline.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
      console.log('Passed: emails (2 languages x 4 mails x 2 widths x 2 schemes, text and current versions), 3 current unsubscribe pages, 320px offline file.');
    } finally {
      await browser.close();
    }
  } finally {
    server.close();
  }
}

main().catch((error) => { console.error(error); process.exitCode = 1; });
