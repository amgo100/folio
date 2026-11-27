// Loads the preview viewer in an iframe on another origin, as VS Code's
// Simple Browser does, and checks that it connects and draws its pages.
// Run by `the_viewer_draws_pages_inside_an_iframe` in `preview.rs`:
//
//     node iframe.cjs <the preview's URL with its token>
//
// Exits with 0 when the viewer drew a page inside the frame, and otherwise
// prints why and exits with 1. Needs Node.js with Playwright and Chromium.
'use strict';

const http = require('http');
const { chromium } = require('playwright');

const TIMEOUT = 30000;

async function main() {
  const url = process.argv[2];
  if (!url) throw new Error('usage: node iframe.cjs <preview URL>');

  // The embedding page, on another port and so another origin, with the
  // sandbox VS Code's Simple Browser gives its frame.
  const embedder = http.createServer((_, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(
      '<!doctype html><title>Editor</title>' +
        '<iframe sandbox="allow-scripts allow-forms allow-same-origin allow-downloads"' +
        ` src="${url.replace(/&/g, '&amp;').replace(/"/g, '&quot;')}"` +
        ' style="width:800px;height:1000px"></iframe>',
    );
  });
  await new Promise((resolve) => embedder.listen(0, '127.0.0.1', resolve));
  const { port } = embedder.address();

  const browser = await chromium.launch();
  const problems = [];
  try {
    const page = await browser.newPage();
    // Errors of the viewer's frame, and the browser's refusal to frame it,
    // which it reports on the embedding page.
    const origin = new URL(url).origin;
    page.on('console', (m) => {
      if (m.type() !== 'error') return;
      const from = m.location().url || '';
      if (from.startsWith(origin) || /frame-ancestors|Refused to frame|X-Frame-Options/.test(m.text())) {
        problems.push(m.text());
      }
    });
    page.on('pageerror', (e) => problems.push(String(e)));
    await page.goto(`http://127.0.0.1:${port}/`);
    const frame = page.frameLocator('iframe');
    // The status names the revision once its update has arrived on the
    // websocket, and the first page is drawn on a canvas with a size.
    const status = frame.locator('#status');
    await status.filter({ hasText: /revision \d+/ }).waitFor({ timeout: TIMEOUT });
    const canvas = frame.locator('#pages .page canvas').first();
    await canvas.waitFor({ timeout: TIMEOUT });
    const drawn = await canvas.evaluate(async (c) => {
      // Waits for a frame in which something other than blank is painted.
      for (let i = 0; i < 300; i++) {
        if (c.width > 0 && c.height > 0) {
          const data = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
          for (let j = 0; j < data.length; j += 4) {
            if (data[j + 3] !== 0 && (data[j] < 250 || data[j + 1] < 250 || data[j + 2] < 250)) {
              return true;
            }
          }
        }
        await new Promise((r) => setTimeout(r, 100));
      }
      return false;
    });
    if (!drawn) throw new Error('the viewer drew nothing on its first page');
    const text = await status.textContent();
    if (!/revision \d+/.test(text)) throw new Error(`the viewer lost its connection: ${text}`);
    if (problems.length) throw new Error(problems.join('\n'));
  } finally {
    await browser.close();
    embedder.close();
  }
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(String(e && e.stack ? e.stack : e));
    process.exit(1);
  },
);
