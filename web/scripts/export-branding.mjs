import { chromium } from '@playwright/test';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const root = new URL('../../', import.meta.url);
const source = await readFile(new URL('docs/branding/relay-icon-master.png', root));
const icon = `data:image/png;base64,${source.toString('base64')}`;
const browser = await chromium.launch({ channel: process.env.CI ? undefined : 'chrome' });

try {
  const page = await browser.newPage({ deviceScaleFactor: 1 });
  await page.setContent(`<style>html,body{margin:0;background:transparent}img{display:block;width:100vw;height:100vh}</style><img src="${icon}" alt="">`);
  await page.locator('img').evaluate(image => image.decode());
  const images = new Map();
  for (const size of [16, 24, 32, 48, 64, 128, 180, 256, 512]) {
    await page.setViewportSize({ width: size, height: size });
    images.set(size, await page.screenshot({ omitBackground: true }));
  }

  for (const [path, size] of [
    ['web/public/relay-icon.png', 512],
    ['web/public/relay-icon-128.png', 128],
    ['web/public/apple-touch-icon.png', 180],
  ]) {
    await writeFile(new URL(path, root), images.get(size));
  }

  // ICO supports PNG frames; render every size directly from the original image.
  const sizes = [16, 24, 32, 48, 64, 128, 256];
  const header = Buffer.alloc(6 + sizes.length * 16);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(sizes.length, 4);
  let offset = header.length;
  sizes.forEach((size, index) => {
    const entry = 6 + index * 16;
    header[entry] = size === 256 ? 0 : size;
    header[entry + 1] = size === 256 ? 0 : size;
    header.writeUInt16LE(1, entry + 4);
    header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(images.get(size).length, entry + 8);
    header.writeUInt32LE(offset, entry + 12);
    offset += images.get(size).length;
  });
  await writeFile(new URL('web/public/favicon.ico', root), Buffer.concat([header, ...sizes.map(size => images.get(size))]));

  // Keep a review sheet outside the shipped assets, including actual sidebar size.
  await page.setViewportSize({ width: 880, height: 480 });
  await page.setContent(`<!doctype html><html lang="zh-CN"><meta charset="utf-8"><style>
    *{box-sizing:border-box}body{margin:0;font-family:Segoe UI,Microsoft YaHei,sans-serif;background:#eef2f5;color:#24313a}
    main{display:grid;grid-template-columns:1fr 1fr;min-height:480px}.panel{padding:36px}.dark{background:#1b242c;color:#e7eef3}
    .hero{display:block;width:160px;height:160px;margin:8px 0 30px}.brand{display:flex;align-items:center;gap:10px}
    .brand img{width:34px;height:34px}.brand strong{display:block;font-size:17px;font-weight:650}.brand small{display:block;font-size:11px;margin-top:2px}
    .sizes{display:flex;align-items:center;gap:24px;height:80px;margin-top:18px}.sizes figure{margin:0;text-align:center}
    figcaption{font-size:11px;opacity:.7;margin-top:8px}.sizes img{display:block}
    </style><main>${['panel', 'panel dark'].map(className => `<section class="${className}"><img class="hero" src="${icon}" alt="Codex Relay"><div class="brand"><img src="${icon}" alt=""><span><strong>Codex Relay</strong><small>模型接入管理台</small></span></div><div class="sizes">${[16, 24, 32, 48].map(size => `<figure><img src="${icon}" width="${size}" height="${size}" alt=""><figcaption>${size}px</figcaption></figure>`).join('')}</div></section>`).join('')}</main></html>`);
  const previewDirectory = new URL('.cache/branding/', root);
  await mkdir(previewDirectory, { recursive: true });
  await page.locator('img').evaluateAll(images => Promise.all(images.map(image => image.decode())));
  await page.screenshot({ path: fileURLToPath(new URL('relay-icon-preview.png', previewDirectory)) });
  console.log('Exported Codex Relay PNGs, multi-size ICO, and .cache/branding/relay-icon-preview.png');
} finally {
  await browser.close();
}
