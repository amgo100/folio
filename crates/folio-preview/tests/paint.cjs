// Watches what the preview viewer paints in Chromium, for a test or a
// benchmark that edits the project and waits for the pages:
//
//     node paint.cjs <the preview's URL with its token> <page>
//
// It loads the viewer, scrolls page <page> (from 0) into view, and prints
// one line for each thing it sees, on standard output:
//
// - `ready <revision>` once every visible page shows its current list,
//   without holes;
// - `painted <revision>` each time a later revision reaches that state;
// - `exact <revision>` each time a later revision has every visible page
//   drawn exactly, after glyphs drawn from another quarter-pixel offset
//   were replaced (often with `painted`, when none was needed);
// - `holes <revision>` each time, after `ready`, a visible page shows a
//   frame with glyphs or images missing before the viewer's limit on how
//   long it keeps the old frame ran out;
// - `error <message>` when the viewer throws, at once.
//
// The viewer reports what it shows through `folioPainted`, which this
// script defines. It stops when its standard input closes, and exits with
// 1 and a message on standard error when the viewer fails. Needs Node.js
// with Playwright and Chromium.
'use strict';

const { chromium } = require('playwright');

const TIMEOUT = 30000;

async function main() {
  const [url, which] = process.argv.slice(2);
  if (!url || which === undefined) throw new Error('usage: node paint.cjs <preview URL> <page>');
  const target = Number(which);

  const browser = await chromium.launch();
  try {
    const page = await browser.newPage({ viewport: { width: 1000, height: 1300 } });
    const problems = [];
    page.on('pageerror', (e) => {
      problems.push(String(e));
      console.log(`error ${String(e).replace(/\s+/g, ' ')}`);
    });
    let ready = false;
    // Set once the page is scrolled into view, which may draw pages the
    // first time.
    let scrolled = false;
    let last = 0;
    let lastExact = 0;
    let readyAt = null;
    const readied = new Promise((resolve) => (readyAt = resolve));
    await page.exposeFunction('folioPainted', (state) => {
      if (!scrolled || !state.visible.includes(target)) return;
      const settled = state.current && !state.holes && state.revision > 0;
      if (!ready) {
        if (settled) {
          ready = true;
          last = state.revision;
          lastExact = state.revision;
          console.log(`ready ${state.revision}`);
          readyAt();
        }
        return;
      }
      if (state.early) console.log(`holes ${state.revision}`);
      if (settled && state.revision > last) {
        last = state.revision;
        console.log(`painted ${state.revision}`);
      }
      if (settled && state.exact && state.revision > lastExact) {
        lastExact = state.revision;
        console.log(`exact ${state.revision}`);
      }
    });
    await page.goto(url);
    await page.locator('#status').filter({ hasText: /revision \d+/ }).waitFor({ timeout: TIMEOUT });
    const el = page.locator(`#pages .page[data-index="${target}"]`);
    await el.waitFor({ timeout: TIMEOUT });
    await el.scrollIntoViewIfNeeded();
    await page.waitForTimeout(500);
    scrolled = true;
    // A resize makes the viewer draw what it lacks and report.
    await page.evaluate(() => window.dispatchEvent(new Event('resize')));
    let timer;
    await Promise.race([
      readied,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('the page was never painted')), TIMEOUT);
      }),
    ]);
    clearTimeout(timer);
    // Until standard input closes.
    await new Promise((resolve) => {
      process.stdin.on('end', resolve);
      process.stdin.on('close', resolve);
      process.stdin.resume();
    });
    if (problems.length) throw new Error(problems.join('\n'));
  } finally {
    await browser.close();
  }
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(e && e.stack ? e.stack : String(e));
    process.exit(1);
  },
);
