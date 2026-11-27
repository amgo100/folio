// Opens the preview viewer and checks that it draws every image of every
// page: that the area of each image item of a page's display list is not
// blank once the page is drawn. Run by `the_viewer_draws_images` in
// `preview.rs`:
//
//     node images.cjs <the preview's URL with its token>
//
// Exits with 0 when every image was drawn, and otherwise prints why and
// exits with 1. Needs Node.js with Playwright and Chromium.
'use strict';

const { chromium } = require('playwright');

const TIMEOUT = 30000;

async function main() {
  const url = process.argv[2];
  if (!url) throw new Error('usage: node images.cjs <preview URL>');
  const browser = await chromium.launch();
  const problems = [];
  try {
    const page = await browser.newPage({ viewport: { width: 900, height: 1200 } });
    page.on('pageerror', (e) => problems.push(String(e)));
    page.on('console', (m) => {
      if (m.type() === 'error') problems.push(m.text());
    });
    await page.goto(url);
    await page.locator('#status').filter({ hasText: /revision \d+/ }).waitFor({ timeout: TIMEOUT });
    // The viewer's pages (`pages`, a binding of its script) with images.
    const withImages = await page.evaluate(() =>
      pages.flatMap((p, i) => (p && p.list && p.list.items.some((it) => it.kind === 'image') ? [i] : [])),
    );
    if (withImages.length === 0) throw new Error('no page has an image');
    let images = 0;
    for (const i of withImages) {
      await page.locator(`.page[data-index="${i}"]`).scrollIntoViewIfNeeded();
      // Drawn completely: every glyph and image was there.
      await page.waitForFunction((i) => pages[i].drawn > 0, i, { timeout: TIMEOUT });
      const blank = await page.evaluate((i) => {
        const { list, canvas } = pages[i];
        const scale = canvas.width / list.width;
        const ctx = canvas.getContext('2d');
        const out = [];
        for (const item of list.items) {
          if (item.kind !== 'image') continue;
          const x = Math.floor(item.rect.x * scale), y = Math.floor(item.rect.y * scale);
          const w = Math.max(1, Math.floor(item.rect.width * scale));
          const h = Math.max(1, Math.floor(item.rect.height * scale));
          const data = ctx.getImageData(x, y, w, h).data;
          let ink = 0;
          for (let p = 0; p < data.length; p += 4) {
            if (data[p] < 240 || data[p + 1] < 240 || data[p + 2] < 240) ink++;
          }
          out.push({ format: item.format, ink });
        }
        return out;
      }, i);
      for (const b of blank) {
        images++;
        if (b.ink === 0) problems.push(`page ${i + 1}: an image of format ${b.format} is blank`);
      }
    }
    // A JPEG turned by its Exif orientation is drawn as stored, as in the
    // PDF: the viewer's `withoutExif` drops the segment.
    const exif = await page.evaluate(async () => {
      const jpeg = pages
        .flatMap((p) => (p && p.list ? p.list.items : []))
        .find((it) => it.kind === 'image' && it.format === 0);
      if (!jpeg) return null;
      // APP1: "Exif\0\0", a little-endian TIFF header and one IFD entry,
      // the orientation (0x0112), a SHORT of 6: turned a quarter.
      const app1 = Uint8Array.from([
        0xff, 0xe1, 0x00, 0x22, 0x45, 0x78, 0x69, 0x66, 0x00, 0x00,
        0x49, 0x49, 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00,
        0x01, 0x00, 0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
      ]);
      const turned = new Uint8Array(jpeg.bytes.length + app1.length);
      turned.set(jpeg.bytes.subarray(0, 2));
      turned.set(app1, 2);
      turned.set(jpeg.bytes.subarray(2), 2 + app1.length);
      const size = async (bytes) => {
        const b = await createImageBitmap(new Blob([bytes], { type: 'image/jpeg' }));
        return [b.width, b.height];
      };
      return { stored: await size(jpeg.bytes), turned: await size(turned), stripped: await size(withoutExif(turned)) };
    });
    if (exif) {
      const [w, h] = exif.stored;
      if (w !== h && exif.turned[0] !== h) problems.push(`the Exif test image was not turned: ${JSON.stringify(exif)}`);
      if (exif.stripped[0] !== w || exif.stripped[1] !== h) {
        problems.push(`a JPEG is drawn turned by its Exif orientation: ${JSON.stringify(exif)}`);
      }
    }
    if (problems.length) throw new Error(problems.join('\n'));
    console.log(`${images} images drawn on ${withImages.length} pages`);
  } finally {
    await browser.close();
  }
}

main().then(
  () => process.exit(0),
  (e) => {
    console.error(String(e && e.stack ? e.stack : e));
    process.exit(1);
  },
);
