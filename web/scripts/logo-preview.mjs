import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { chromium } from 'playwright';

// Static, self-contained review sheet. No server or external resources are used.
const light = await readFile(new URL('../public/brand/justaiagent-light.svg', import.meta.url), 'utf8');
const dark = await readFile(new URL('../public/brand/justaiagent-dark.svg', import.meta.url), 'utf8');
const previous = '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg"><rect width="64" height="64" rx="18" fill="currentColor"/><path d="M34 12 18 35h13l-3 17 19-26H33z" fill="var(--bg)"/></svg>';
const samples = svg => [16, 24, 32, 48, 80].map(size => `<div class="sample"><span style="width:${size}px;height:${size}px">${svg}</span><small>${size}px</small></div>`).join('');
const panel = (theme, svg) => `<section class="${theme}"><header><span>${theme === 'light' ? 'LIGHT · 酒红' : 'DARK · 电光青'}</span><small>JustAIAgent / mark study</small></header><div class="wordmark"><div>${svg}</div><b>JustAI<span>Agent</span></b></div><div class="row"><label>ORIGINAL</label><div class="samples">${samples(svg)}</div></div><div class="row previous"><label>PREVIOUS</label><div class="samples">${samples(previous)}</div></div><footer>J monogram / open orbit / rising path</footer></section>`;
const html = `<!doctype html><html lang="zh-CN"><meta charset="utf-8"><title>JustAIAgent Logo Review</title><style>
*{box-sizing:border-box}body{margin:0;background:#ddd8d1;font-family:Inter,-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif}main{display:grid;grid-template-columns:1fr 1fr;min-height:620px}section{--bg:#f8f5f0;--muted:#8d7e80;--line:#e8dfd7;background:var(--bg);color:#88273f;padding:44px 44px 32px}.dark{--bg:#0b1820;--muted:#819ba5;--line:#203540;color:#56dfdf}header{display:flex;justify-content:space-between;gap:12px;font-size:11px;letter-spacing:2px}header small{font-size:9px;color:var(--muted);letter-spacing:.5px}.wordmark{display:flex;align-items:center;gap:18px;margin:70px 0 60px}.wordmark>div{width:66px;height:66px}.wordmark b{font-size:36px;letter-spacing:-1.5px;color:#35272c}.dark .wordmark b{color:#deecee}.wordmark b span{font-weight:400}svg{display:block;width:100%;height:100%}.row{padding:24px 0;border-top:1px solid var(--line)}label{display:block;letter-spacing:1.6px;font-size:9px;color:var(--muted);margin-bottom:28px}.samples{display:flex;align-items:center;justify-content:space-between;gap:18px}.sample{height:110px;display:flex;align-items:center;justify-content:center;flex-direction:column;gap:18px}.sample>span{display:block}.sample small{font-size:10px;color:var(--muted);letter-spacing:.5px}.previous .sample{height:80px}.previous .sample>span{transform:scale(.8)}footer{font-size:10px;letter-spacing:1px;color:var(--muted);padding-top:22px;border-top:1px solid var(--line)}
</style><main>${panel('light', light)}${panel('dark', dark)}</main></html>`;
const output = new URL('../artifacts/', import.meta.url);
await mkdir(output, { recursive: true });
await writeFile(new URL('logo-comparison.html', output), html);
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 700 }, deviceScaleFactor: 2 });
  await page.setContent(html);
  await page.screenshot({ path: new URL('logo-comparison.png', output).pathname, fullPage: true });
} finally { await browser.close(); }
