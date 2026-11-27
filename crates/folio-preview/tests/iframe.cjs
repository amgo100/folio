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
    page.on('console', (m) => {
      if (m.type() === 'error') problems.push(m.text());
    });
    page.on('pageerror', (e) => problems.push(String(e)));
    await page.goto(`http://127.0.0.1:${port}/`);
    const frame = page.frameLocator('iframe');
    // The viewer's pages, drawn on canvases from the display lists that
    // arrive on its websocket.
    await frame.locator('#pages .page canvas').first().waitFor({ timeout: TIMEOUT });
    const status = await frame.locator('#status').textContent({ timeout: TIMEOUT });
    if (/Connecting/.test(status)) throw new Error(`the viewer did not connect: ${status}`);
    const blocked = problems.filter((p) => /frame-ancestors|Refused to frame|X-Frame-Options/.test(p));
    if (blocked.length) throw new Error(blocked.join('\n'));
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
