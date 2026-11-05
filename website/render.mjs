// Render the images the site serves from its SVG and HTML sources, with the
// headless Chrome that mermaid-cli's Puppeteer uses:
//
// - og.png: the committed 1200x630 link-preview image;
// - favicon-32.png, apple-touch-icon.png (180), icon-192.png and
//   icon-512.png: the logo's icon (docs/assets/logo/udpstp-icon.svg).
//
// usage: node website/render.mjs OUT_DIR [--check-social|--update-social]
// Normal builds copy the social image. Check or update it after editing og.html.
//
// Needs the site tools: `npm ci --ignore-scripts` in website/ and
// `npx puppeteer browsers install chrome`.
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import puppeteer from 'puppeteer';

const out = resolve(process.argv[2] ?? 'target/site/rendered');
const root = resolve(import.meta.dirname, '..');
const socialMode = process.argv[3] ?? '';
if (!['', '--check-social', '--update-social'].includes(socialMode)) {
  throw new Error('Expected --check-social or --update-social');
}
mkdirSync(out, { recursive: true });

const browser = await puppeteer.launch({
  headless: true,
  args: ['--no-sandbox', '--disable-gpu'],
  // Apply the same Linux font rendering rules on desktops and build runners.
  env: { ...process.env, FONTCONFIG_FILE: join(root, 'website', 'fonts', 'fontconfig.conf') },
});
try {
  const page = await browser.newPage();

  const socialImage = join(root, 'website', 'og.png');
  if (socialMode) {
    await page.setViewport({ width: 1200, height: 630, deviceScaleFactor: 1 });
    await page.goto(pathToFileURL(join(root, 'website', 'og.html')).href);
    await page.evaluate(() => document.fonts.ready);
    const failedFonts = await page.evaluate(() =>
      [...document.fonts].filter(font => font.status === 'error').map(font => font.family),
    );
    if (failedFonts.length) throw new Error(`Social image fonts failed to load: ${failedFonts.join(', ')}`);
    const rendered = await page.screenshot();
    if (socialMode === '--update-social') writeFileSync(socialImage, rendered);
    else if (!readFileSync(socialImage).equals(rendered)) {
      throw new Error('Social image is stale: run with --update-social');
    }
  }
  copyFileSync(socialImage, join(out, 'og.png'));

  const icon = readFileSync(join(root, 'docs', 'assets', 'logo', 'udpstp-icon.svg'), 'utf8');
  // The apple-touch-icon fills its square: iOS rounds the corners itself.
  for (const [name, size, background] of [
    ['favicon-32.png', 32, null],
    ['apple-touch-icon.png', 180, '#121214'],
    ['icon-192.png', 192, null],
    ['icon-512.png', 512, null],
  ]) {
    await page.setViewport({ width: size, height: size, deviceScaleFactor: 1 });
    const svg = icon.replace(/width="32" height="32"/, `width="${size}" height="${size}"`);
    await page.setContent(
      `<html><body style="margin:0;background:${background ?? 'transparent'}">${svg}</body></html>`,
    );
    await page.screenshot({ path: join(out, name), omitBackground: background === null });
  }
  console.log(`rendered og.png and the PNG icons to ${out}`);
} finally {
  await browser.close();
}
