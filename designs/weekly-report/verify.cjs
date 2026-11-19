const assert = require('node:assert/strict');
const { readFileSync, writeFileSync } = require('node:fs');
const { createServer } = require('node:http');
const { join } = require('node:path');
const { pathToFileURL } = require('node:url');
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE ?? 'playwright');

const files = { '/': 'index.html', '/index.html': 'index.html', '/email.html': 'email.html' };
const pages = Object.fromEntries(Object.values(files).map((name) => [name, readFileSync(join(__dirname, name))]));
const output = process.env.REVIEW_OUTPUT;
const collector = process.env.COLLECTOR_PATH;
const CODE = /^[A-Za-z0-9_-]{43}$/;
const redactPatterns = ['[A-Za-z0-9_-]{43}'];
const waitFor = (page, id, text) => page.locator(`#${id}`).getByText(text, { exact: true }).waitFor();

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

async function settings(page, mobile, address) {
  const errors = watch(page, address);
  if (collector) await page.addInitScript({ path: collector });
  const control = (id) => page.locator(`#${id}`);
  const captures = [];
  const capture = collect(page, captures);
  const en = mobile;
  const names = en ? ['Not subscribed', 'Confirmation pending', 'Subscribed'] : ['未订阅', '待确认', '已订阅'];
  const state = (index) => waitFor(page, 'report-card', names[index]);
  const skipWait = async () => { if (await control('skip-wait').isVisible()) await control('skip-wait').click(); };

  await page.goto(address);
  const preview = control('preview').locator('summary');
  await preview.click();
  if (en) await control('language').selectOption('en');
  await state(0);
  assert.match(await control('schedule').innerText(), /周一|Monday/);
  assert.match(await control('example').innerText(), /16.+22/);
  assert.equal(await control('email').innerText(), 'viewer@example.com');
  assert.equal(await control('email').locator('input').count(), 0);
  assert.equal(await control('sections').locator(':scope > :visible').count(), 1);
  assert.equal(await control('send').evaluate((element) => getComputedStyle(element).backgroundColor), 'rgb(0, 144, 255)');
  await capture();
  if (output && !mobile) {
    await preview.click();
    await page.screenshot({ path: join(output, 'settings-desktop.png') });
    await preview.click();
  }

  await control('sections').locator('a').focus();
  await page.keyboard.press('Tab');
  assert.equal(await control('send').evaluate((element) => element === document.activeElement), true);
  await page.keyboard.press('Enter');
  await capture();
  await state(1);
  assert.equal(await control('code').evaluate((element) => element === document.activeElement), true);
  assert.equal(await control('code').getAttribute('maxlength'), null);
  assert.equal(await control('code').getAttribute('pattern'), null);
  const oldToken = await control('preview-code').innerText();
  assert.match(oldToken, CODE);
  assert(await control('resend').isDisabled());
  assert.match(await control('cooldown').innerText(), /秒后可再次发送|Request another code in/);
  await capture();
  if (output && mobile) {
    await preview.click();
    await page.screenshot({ path: join(output, 'settings-mobile.png'), fullPage: true });
    await preview.click();
  }

  for (const [value, message] of [['', /请输入|Enter a/], ['wrong-code', /无效或已过期|invalid or expired/]]) {
    await control('code').fill(value);
    await page.keyboard.press('Enter');
    assert.equal(await control('code').getAttribute('aria-invalid'), 'true');
    assert.match(await control('code-error').innerText(), message);
    await capture();
  }
  await control('scenario').selectOption('unavailable');
  assert(await control('resend').isDisabled());
  assert(await control('activate').isDisabled());
  assert(await control('cancel').isEnabled());
  await capture();
  await control('scenario').selectOption('normal');
  await skipWait();
  assert(await control('resend').isEnabled());
  await control('resend').click();
  await page.waitForFunction((old) => document.querySelector('#preview-code').textContent !== old, oldToken);
  await capture();
  await control('code').fill(oldToken);
  await page.keyboard.press('Enter');
  assert.match(await control('code-error').innerText(), /无效或已过期|invalid or expired/);
  await control('copy-code').click();
  await page.keyboard.press('Enter');
  await capture();
  await state(2);
  assert.equal(await control('cancel').evaluate((element) => element === document.activeElement), true);
  assert(await control('preview-code').isHidden());
  assert(await control('cooldown').isHidden());
  await capture();

  await control('site').selectOption('docs');
  await state(0);
  assert.match(await control('schedule').innerText(), /周日|Sunday/);
  assert.match(await control('example').innerText(), /15.+21/);
  await capture();
  await control('site').selectOption('studio');
  await state(2);
  await control('access').selectOption('member');
  await state(0);
  assert.equal(await control('email').innerText(), 'member@example.com');
  assert.equal(await control('sections').locator(':scope > :visible').count(), 4);
  await capture();
  await control('access').selectOption('denied');
  assert(await control('email').isHidden());
  assert(await control('sections').isHidden());
  await capture();
  await control('access').selectOption('viewer');
  await control('scenario').selectOption('load-error');
  assert(await control('send').isHidden());
  await capture();
  await control('retry').click();
  assert(await control('loading').isVisible());
  await capture();
  await state(2);
  assert.equal(await control('weekly-report').evaluate((element) => element === document.activeElement), true);

  await control('scenario').selectOption('unavailable');
  assert(await control('email').isVisible());
  assert(await control('cancel').isEnabled());
  await capture();
  await control('cancel').click();
  await capture();
  await state(0);
  assert(await control('send').isDisabled());
  await capture();
  await control('scenario').selectOption('normal');
  await skipWait();
  await control('send').click();
  await state(1);
  await control('scenario').selectOption('unavailable');
  await control('cancel').click();
  await state(0);
  await capture();

  // Five wrong codes void the pending confirmation.
  await control('scenario').selectOption('normal');
  await skipWait();
  await control('send').click();
  await state(1);
  for (let attempt = 1; attempt <= 5; attempt += 1) {
    await control('code').fill(`wrong-${attempt}`);
    await page.keyboard.press('Enter');
  }
  await state(0);
  assert.match(await control('feedback').innerText(), /已失效|no longer valid/);
  assert.equal(await control('feedback').getAttribute('role'), 'alert');
  await capture();

  // An expired code is reported the same way as a used-up one.
  await skipWait();
  await control('send').click();
  await state(1);
  await control('expire').click();
  await control('copy-code').click();
  await page.keyboard.press('Enter');
  await state(0);
  assert.match(await control('feedback').innerText(), /已失效|no longer valid/);
  await capture();

  // SMTP refuses the confirmation email: pending without a delivered code.
  await control('scenario').selectOption('send-fails');
  await skipWait();
  await control('send').click();
  await state(1);
  assert.match(await control('feedback').innerText(), /没有发出|couldn’t be sent/);
  assert(await control('preview-inbox').isHidden());
  assert(await control('resend').isDisabled());
  await capture();
  await control('scenario').selectOption('normal');
  await skipWait();
  await control('resend').click();
  await page.waitForFunction(() => /^[A-Za-z0-9_-]{43}$/.test(document.querySelector('#preview-code').textContent));
  await control('copy-code').click();
  await page.keyboard.press('Enter');
  await state(2);
  await capture();

  assert.deepEqual(errors, []);
  save(`settings-${mobile ? 'mobile' : 'desktop'}`, captures);
}

function expectedCurrent(kind, f, lang, code) {
  const link = `https://rivu.example.com/s/${f.id}`;
  if (kind === 'code') {
    return lang === 'zh'
      ? ['Rivu 周报验证码', `请将下面的验证码粘贴到 Rivu。验证码在 15 分钟内有效。\n\n${code}\n`]
      : ['Rivu weekly report confirmation code', `Paste this code into Rivu within 15 minutes.\n\n${code}\n`];
  }
  return lang === 'zh'
    ? ['Rivu 周报', `${f.site}\n${f.from} 至 ${f.to}（${f.tz}）\n\n访客：${f.visitors}\n访问：${f.visits}\n浏览量：${f.pageviews}\n\n查看仪表盘：${link}\n`]
    : ['Rivu weekly report', `${f.site}\n${f.from} to ${f.to} (${f.tz})\n\nVisitors: ${f.visitors}\nVisits: ${f.visits}\nPageviews: ${f.pageviews}\n\nView dashboard: ${link}\n`];
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
  const code = await api.evaluate((x) => x.sampleCode);
  assert.match(code, CODE);

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
    for (const kind of ['report', 'code']) {
      await control('mail').selectOption(kind);
      for (const fixture of kind === 'report' ? ['studio', 'quiet', 'large'] : ['studio']) {
        if (kind === 'report') await control('fixture').selectOption(fixture);
        const f = fixtures[fixture];
        for (const width of ['600', '360']) {
          for (const scheme of ['light', 'dark']) {
            await control('width').selectOption(width);
            await control('scheme').selectOption(scheme);
            await control('version').selectOption('html');
            const html = await api.evaluate((x, [k, f, l]) => (k === 'report' ? x.reportHtml : x.codeHtml)(f, l), [kind, f, lang]);
            assert(Buffer.byteLength(html) < 100_000);
            assert(!/<script|<link|@import|url\(/i.test(html));
            for (const [, href] of html.matchAll(/href="([^"]+)"/g)) assert.match(href, /^https:\/\//);
            const marker = kind === 'report' ? f.site : code;
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
              assert.equal(await frame.locator(`a[href="https://rivu.example.com/s/${f.id}/settings/weekly-report"]`).count(), 1);
            } else {
              assert.equal(await control('subject').innerText(), lang === 'zh' ? 'Rivu 周报确认码' : 'Rivu weekly report confirmation code');
              assert(text.includes(code));
              assert(text.includes(lang === 'zh' ? '15 分钟' : '15 minutes'));
            }
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
          assert(plain.includes(`https://rivu.example.com/s/${f.id}/settings/weekly-report`));
        } else {
          assert(plain.includes(code));
        }
        assert(await control('preheader-row').isHidden());
        await capture();
        await control('version').selectOption('current');
        const [subject, body] = expectedCurrent(kind, f, lang, code);
        assert.equal(await control('subject').innerText(), subject);
        assert.equal(await control('plain').evaluate((element) => element.textContent), body);
        await capture();
        await control('version').selectOption('html');
      }
    }
  }
  assert.deepEqual(errors, []);
  save('email', captures);
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
      await settings(await browser.newPage({ viewport: { width: 1440, height: 900 } }), false, address);
      await settings(await browser.newPage({ viewport: { width: 360, height: 800 }, isMobile: true, hasTouch: true, colorScheme: 'dark' }), true, address);
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
      await offline.goto(pathToFileURL(join(__dirname, 'index.html')).href);
      await offline.locator('#send').click();
      await waitFor(offline, 'report-card', '待确认');
      await offline.locator('#preview summary').click();
      await offline.locator('#copy-code').click();
      await offline.locator('#activate').click();
      await waitFor(offline, 'report-card', '已订阅');
      assert(await offline.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
      await offline.goto(pathToFileURL(join(__dirname, 'email.html')).href);
      await offline.frameLocator('#message').locator('h1').waitFor();
      assert(await offline.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth));
      console.log('Passed: settings (1440px zh light, 360px en dark), emails (2 languages x 4 mails x 2 widths x 2 schemes, text and current versions), 320px offline files.');
    } finally {
      await browser.close();
    }
  } finally {
    server.close();
  }
}

main().catch((error) => { console.error(error); process.exitCode = 1; });
