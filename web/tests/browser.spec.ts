import { test, expect, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const computerStatus = { browser: { available: true, running: false, isolation: 'private_profile_headless' }, desktop: { available: false, running: false, error: 'TEST FIXTURE: KWin dependency unavailable' }, policy: 'isolated_first', busy: false, privilege: { uid: 1000, mode: 'user', root_available: false }, host: { enabled: false, lease_remaining_secs: 0, physical_monitor: true, human_idle_ms: 2000 } };
// All data below is synthetic TEST FIXTURE data. Production code never seeds any demo content.
const network = { proxy_mode: 'environment', proxy_url: '', no_proxy: '', connect_timeout_secs: 10, headers_timeout_secs: 120, idle_timeout_secs: 300, request_timeout_secs: 1800, max_attempts: 3 };
const fixtureConfig = { revision: 3, sites: [], network };
const modelConfig = { revision: 3, network, sites: [{ id: 'site', name: 'Test site', base_url: 'https://example.test/v1', keys: [{ id: 'key', name: 'Test key', has_secret: true, models: [{ id: 'model', name: 'Test model', model: 'test-model', supports_images: true, max_output_tokens: 4096, context_window: 32000, reasoning_efforts: ['low', 'high'], reasoning_effort: 'low', strict_tools: true, reasoning_replay: false, send_store: true }] }] }] };
const session = { id: 'test-session', title: 'Browser test conversation', cwd: '/tmp/test', selection: { site_id: 'site', key_id: 'key', model_id: 'model' }, reasoning_effort: null, status: 'completed', created_at: '2026-11-13T12:00:00Z', updated_at: '2026-11-13T12:01:00Z', depth: 0, context_epoch: 0 };
const event = (seq: number, type: string, data: unknown) => ({ seq, session_id: session.id, type, data, created_at: '2026-11-13T12:00:00Z' });
const fixtureQuestion = { id: 'blocking-fixture', session_id: session.id, call_id: 'ask-fixture', question: '发布前需要选择哪种验证方式？', blocking: true, options: ['完整验证', '快速检查'], status: 'pending', answer: null, delivered: false, created_at: '2026-11-13T12:00:00Z', updated_at: '2026-11-13T12:00:00Z' };
const fixtureGoal = { id: 'goal-fixture', session_id: session.id, title: '完成工作台验收', objective: '验证桌面与手机布局，保留文件树、主题和所有会话设置。', status: 'pending', progress: '', rounds: 0, created_at: '2026-11-13T12:00:00Z', updated_at: '2026-11-13T12:00:00Z' };
async function autonomySession(page: Page, status = 'waiting_for_answer') {
  await controlledEvents(page); await bootstrap(page, true);
  await page.route('**/api/config', route => route.fulfill({ json: modelConfig }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [{ ...session, status }] }));
  await page.route('**/api/sessions/test-session', route => route.fulfill({ json: { session: { ...session, status }, events: [event(1, 'user_message', { text: 'TEST FIXTURE · 请完成工作台验收，不使用真实模型。' }), event(2, 'tool_start', { call_id: 'ask-fixture', name: 'ask_user', args: { action: 'ask', question: fixtureQuestion.question } }), event(3, 'tool_end', { call_id: 'ask-fixture', output: 'Question stored', is_error: false }), event(4, 'tool_start', { call_id: 'goal-fixture', name: 'goal', args: { action: 'create', title: fixtureGoal.title } }), event(5, 'tool_end', { call_id: 'goal-fixture', output: 'Goal stored', is_error: false })] } }));
  await page.route('**/api/sessions/test-session/files?*', route => route.fulfill({ json: { path: '', entries: [{ path: 'src', name: 'src', is_dir: true, size: 0 }, { path: 'README.md', name: 'README.md', is_dir: false, size: 43 }] } }));
  await page.goto('/'); await openSidebar(page); await page.getByRole('button', { name: /Browser test conversation/ }).click();
  await expect(page.getByLabel('消息内容')).toBeVisible();
}

async function serveBundle(page: Page) {
  await page.route('http://localhost:48991/**', async route => {
    const url = new URL(route.request().url());
    if (url.pathname.startsWith('/api/')) { await route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error: 'Unmocked test endpoint' }) }); return; }
    const path = url.pathname === '/' ? 'index.html' : url.pathname.slice(1);
    const file = resolve(process.cwd(), 'dist', path);
    if (!file.startsWith(resolve(process.cwd(), 'dist') + '/')) throw new Error('Invalid test asset path');
    const contentType = path.endsWith('.js') ? 'text/javascript' : path.endsWith('.css') ? 'text/css' : path.endsWith('.svg') ? 'image/svg+xml' : 'text/html';
    await route.fulfill({ contentType, body: await readFile(file) });
  });
}
async function bootstrap(page: Page, authenticated: boolean, initialized = true) {
  await page.route('**/api/bootstrap', route => route.fulfill({ json: { initialized, authenticated, allow_setup: true, version: 'test', workspace: '/tmp/test' } }));
}
async function openSidebar(page: Page) {
  const menu = page.getByRole('button', { name: '打开会话侧栏' });
  // Do not race hydration by sampling isVisible once: under parallel load the
  // mobile menu may not have mounted yet, leaving the session list inert.
  if ((page.viewportSize()?.width ?? 1600) <= 680) {
    await expect(menu).toBeVisible();
    await menu.click();
    await expect(page.locator('.sidebar')).not.toHaveAttribute('inert');
  }
}

// A controllable SSE fixture keeps every test offline and never calls a model.
async function controlledEvents(page: Page) {
  await page.addInitScript(() => {
    class FixtureEventSource extends EventTarget {
      onopen: ((event: Event) => void) | null = null;
      onerror: ((event: Event) => void) | null = null;
      constructor(_url: string) {
        super();
        const state = window as unknown as { testEventUrls?: string[] }; (state.testEventUrls ??= []).push(_url);
        Object.assign(window, { emitTestSessionEvent: (data: unknown) => this.dispatchEvent(new MessageEvent('update', { data: JSON.stringify(data) })) });
        queueMicrotask(() => this.onopen?.(new Event('open')));
      }
      close() { /* The fixture has no socket. */ }
    }
    Object.assign(window, { EventSource: FixtureEventSource });
  });
}
async function emitSessionEvent(page: Page, value: ReturnType<typeof event>) {
  await page.evaluate(value => (window as unknown as { emitTestSessionEvent: (value: unknown) => void }).emitTestSessionEvent(value), value);
}
async function openFiles(page: Page) {
  const toggle = page.getByRole('button', { name: '打开文件面板' });
  if ((page.viewportSize()?.width ?? 1600) <= 1180) {
    await expect(toggle).toBeVisible();
    await toggle.click();
  }
}

test.beforeEach(async ({ page }) => {
  await serveBundle(page);
  // Legacy fixtures have no autonomy records; new tests override these routes.
  await page.route('**/api/sessions/*/questions', route => route.fulfill({ json: [] }));
  await page.route('**/api/sessions/*/goals', route => route.fulfill({ json: [] }));
});

test('first setup validates confirmation and uses guarded same-origin mutation', async ({ page }) => {
  await bootstrap(page, false, false);
  let submitted = false;
  await page.route('**/api/setup', async route => { expect(route.request().headers()['x-justai-request']).toBe('1'); submitted = true; await bootstrap(page, true); await route.fulfill({ json: { ok: true } }); });
  await page.route('**/api/config', route => route.fulfill({ json: fixtureConfig }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
  await page.goto('/');
  await expect(page.getByRole('heading', { name: '建立你的专属空间' })).toBeVisible();
  await page.getByLabel('设置访问密码', { exact: true }).fill('test-fixture-password');
  await page.getByLabel('确认密码', { exact: true }).fill('mismatched-password');
  await page.getByRole('button', { name: '创建工作台' }).click();
  await expect(page.getByRole('alert')).toContainText('不一致');
  expect(submitted).toBe(false);
  await page.getByLabel('确认密码', { exact: true }).fill('test-fixture-password');
  await page.getByRole('button', { name: '创建工作台' }).click();
  await expect(page.getByRole('heading', { name: '你的工作台' })).toBeVisible();
  expect(submitted).toBe(true);
  expect(await page.evaluate(() => Object.keys(localStorage))).toEqual(['justai-theme']);
});

test('empty state, theme, layered settings and blank existing secret', async ({ page }, testInfo) => {
  await bootstrap(page, true);
  await page.route('**/api/config', route => route.fulfill({ json: modelConfig }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
  await page.goto('/');
  await expect(page.getByRole('heading', { name: '你的工作台' })).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('welcome-light.png'), fullPage: true });
  const themeButton = (testInfo.project.name === 'mobile' ? page.locator('.topbar') : page.locator('.sidebar')).getByRole('button', { name: /切换到.*主题/ });
  const theme = await page.locator('html').getAttribute('data-theme');
  await expect(page.locator('link[rel="icon"]')).toHaveAttribute('href', `/brand/justaiagent-${theme}.svg`);
  await themeButton.first().click();
  await expect(page.locator('link[rel="icon"]')).toHaveAttribute('href', `/brand/justaiagent-${theme === 'light' ? 'dark' : 'light'}.svg`);
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', theme!);
  await page.screenshot({ path: testInfo.outputPath('welcome-dark.png'), fullPage: true });
  await openSidebar(page);
  await page.getByRole('button', { name: '工作台设置', exact: true }).click();
  await expect(page.getByRole('dialog')).toBeVisible();
  await expect(page.getByLabel('替换密钥（已有密钥）')).toHaveValue('');
  await page.getByRole('button', { name: '网络与诊断' }).click();
  await expect(page.getByRole('radiogroup', { name: '代理模式' })).toBeVisible();
  await expect(page.getByLabel('搜索供应商')).toHaveValue('duckduckgo');
  await expect(page.getByLabel('搜索 API URL')).toHaveValue('');
  await expect(page.getByLabel('搜索 API Key')).toHaveCount(0);
  await expect(page.getByRole('button', { name: '测试连接' })).toBeEnabled();
  await page.getByText('手动代理', { exact: true }).click();
  await expect(page.getByLabel('代理 URL')).toBeVisible();
  await expect(page.getByRole('button', { name: '测试连接' })).toBeDisabled();
  await expect(page.getByRole('button', { name: '保存配置' })).toBeEnabled();
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflow).toBe(false);
  await page.screenshot({ path: testInfo.outputPath('network-settings.png'), fullPage: true });
});

test('per-model omit strict compatibility saves only the selected model and never changes strict_tools', async ({ page }) => {
  await bootstrap(page, true);
  const base = modelConfig.sites[0].keys[0].models[0];
  let configuration = { ...modelConfig, sites: [{ ...modelConfig.sites[0], keys: [{ ...modelConfig.sites[0].keys[0], models: [{ ...base, id: 'first-model', name: 'First model', omit_tool_strict: false }, { ...base, id: 'second-model', name: 'Second model', strict_tools: false, omit_tool_strict: false }, { ...base, id: 'third-model', name: 'Third model', omit_tool_strict: false }] }, { id: 'other-key', name: 'Other key', has_secret: true, models: [{ ...base, id: 'other-key-model', name: 'Other key model', omit_tool_strict: false }] }] }, { id: 'other-site', name: 'Other site', base_url: 'https://other.example.test/v1', keys: [{ id: 'remote-key', name: 'Remote key', has_secret: true, models: [{ ...base, id: 'remote-model', name: 'Remote model', omit_tool_strict: false }] }] }] };
  const originalSites = structuredClone(configuration.sites);
  expect(originalSites.flatMap(site => site.keys.flatMap(key => key.models)).every(model => model.omit_tool_strict === false)).toBe(true);
  const submissions: Record<string, any>[] = [];
  await page.route('**/api/config', async route => {
    if (route.request().method() === 'PUT') {
      const body = route.request().postDataJSON(); submissions.push(body);
      configuration = { ...body, revision: configuration.revision + 1 };
    }
    await route.fulfill({ json: configuration });
  });
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
  await page.goto('/'); await openSidebar(page);
  await page.getByRole('button', { name: '工作台设置', exact: true }).click();
  await page.locator('details.advanced > summary').click();
  const omit = page.getByRole('switch', { name: /^省略 strict 标记/ });
  const strict = page.getByRole('switch', { name: /^严格工具 Schema/ });
  await expect(omit).not.toBeChecked(); await expect(strict).toBeChecked();
  await expect(page.getByText('默认开启以改善兼容端点的缓存复用，仅省略 strict 字段，完整参数 Schema 与本地校验保留。端点需要 strict 标记时可关闭。', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Second model', exact: true }).click();
  await expect(omit).not.toBeChecked(); await expect(strict).not.toBeChecked();
  await omit.check(); await expect(strict).not.toBeChecked();
  await page.getByRole('button', { name: 'First model', exact: true }).click();
  await expect(omit).not.toBeChecked(); await expect(strict).toBeChecked();
  await page.getByRole('button', { name: 'Third model', exact: true }).click();
  await expect(omit).not.toBeChecked(); await expect(strict).toBeChecked();
  await page.getByRole('button', { name: 'Second model', exact: true }).click();
  await expect(omit).toBeChecked(); await expect(strict).not.toBeChecked();
  await page.getByRole('button', { name: '保存配置', exact: true }).click();
  await expect(page.getByRole('button', { name: '保存配置', exact: true })).toBeDisabled();
  expect(submissions).toHaveLength(1);
  const expectedSites = structuredClone(originalSites); expectedSites[0].keys[0].models[1].omit_tool_strict = true;
  expect(submissions[0].sites).toEqual(expectedSites);
  expect(submissions[0].sites.every((site: any) => site.keys.every((key: any) => key.secret === undefined))).toBe(true);
  await omit.uncheck(); await expect(strict).not.toBeChecked();
  await page.getByRole('button', { name: '保存配置', exact: true }).click();
  await expect(page.getByRole('button', { name: '保存配置', exact: true })).toBeDisabled();
  expect(submissions).toHaveLength(2); expect(submissions[1].sites).toEqual(originalSites);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('legacy and newly added models default to cache compatibility and allow strict marker opt-out', async ({ page }) => {
  await bootstrap(page, true);
  const submissions: Record<string, any>[] = [];
  await page.route('**/api/config', async route => {
    if (route.request().method() === 'PUT') {
      const body = route.request().postDataJSON(); submissions.push(body);
      return route.fulfill({ json: { ...body, revision: 4 } });
    }
    await route.fulfill({ json: modelConfig });
  });
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
  await page.goto('/'); await openSidebar(page);
  await page.getByRole('button', { name: '工作台设置', exact: true }).click();
  await page.locator('details.advanced > summary').click();
  await expect(page.getByRole('switch', { name: /^省略 strict 标记/ })).toBeChecked();
  await page.getByRole('button', { name: '添加模型', exact: true }).click();
  await expect(page.getByRole('switch', { name: /^省略 strict 标记/ })).toBeChecked();
  await expect(page.getByRole('switch', { name: /^严格工具 Schema/ })).toBeChecked();
  await page.getByLabel('显示名称', { exact: true }).fill('New compatible model');
  await page.getByLabel('API 模型标识', { exact: true }).fill('new-fixture-model');
  await page.getByRole('button', { name: '保存配置', exact: true }).click();
  await expect(page.getByRole('button', { name: '保存配置', exact: true })).toBeDisabled();
  expect(submissions).toHaveLength(1);
  expect(submissions[0].sites[0].keys[0].models.map((model: any) => model.omit_tool_strict)).toEqual([true, true]);
  expect(submissions[0].sites[0].keys[0].models.map((model: any) => model.strict_tools)).toEqual([true, true]);
  await page.getByRole('switch', { name: /^省略 strict 标记/ }).uncheck();
  await page.getByRole('button', { name: '保存配置', exact: true }).click();
  await expect(page.getByRole('button', { name: '保存配置', exact: true })).toBeDisabled();
  expect(submissions[1].sites[0].keys[0].models.map((model: any) => model.omit_tool_strict)).toEqual([true, false]);
});

test('search settings isolate credentials, preserve blank keys and clear changed origins', async ({ page }, testInfo) => {
  await bootstrap(page, true);
  const submissions: Record<string, any>[] = [];
  let configuration = { ...modelConfig, extra: { keep: true }, search: { provider: 'brave', base_url: '', has_api_key: true, api_key: 'returned-search-fixture-must-be-discarded' } };
  await page.route('**/api/config', async route => {
    if (route.request().method() === 'PUT') {
      const body = route.request().postDataJSON(); submissions.push(body);
      configuration = { ...body, revision: configuration.revision + 1, search: { ...body.search, has_api_key: true, api_key: 'returned-search-fixture-must-be-discarded' } };
    }
    await route.fulfill({ json: configuration });
  });
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
  await page.goto('/'); await openSidebar(page);
  await page.getByRole('button', { name: '工作台设置', exact: true }).click();
  await page.getByRole('button', { name: '网络与诊断' }).click();
  const key = page.getByLabel('搜索 API Key');
  await expect(key).toHaveValue(''); await expect(key).toHaveAttribute('placeholder', '留空保留已有搜索密钥');
  await expect(page.getByLabel('搜索供应商').locator('option')).toHaveCount(3);
  await key.fill('temporary-fixture'); await key.fill('');
  await page.getByRole('button', { name: '保存配置' }).click();
  await expect(page.getByRole('button', { name: '保存配置' })).toBeDisabled();
  expect(submissions[0].search).toMatchObject({ provider: 'brave', base_url: '', api_key: '', has_api_key: true });
  expect(submissions[0].extra).toEqual({ keep: true });
  expect(submissions[0].sites[0].keys[0].secret).toBeUndefined();
  await expect(key).toHaveValue('');
  await key.fill('temporary-fixture');
  await page.getByLabel('搜索 API URL').fill('https://search.example.test/api');
  await expect(key).toHaveValue(''); await expect(key).toHaveAttribute('placeholder', '输入搜索密钥');
  await key.fill('discard-on-provider-change');
  await page.getByLabel('搜索供应商').selectOption('searxng');
  await expect(key).toHaveValue(''); await expect(key).toHaveAttribute('placeholder', '输入搜索密钥');
  await page.getByLabel('搜索 API URL').fill('https://searx.example.test');
  await key.fill('dedicated-search-fixture');
  await page.getByRole('button', { name: '保存配置' }).click();
  await expect(page.getByRole('button', { name: '保存配置' })).toBeDisabled();
  expect(submissions[1].search).toEqual({ provider: 'searxng', base_url: 'https://searx.example.test', api_key: 'dedicated-search-fixture', has_api_key: false });
  await expect(key).toHaveValue('');
  expect(await page.evaluate(() => Object.keys(localStorage))).toEqual(['justai-theme']);
  expect(await page.evaluate(() => JSON.stringify(localStorage))).not.toContain('search-fixture');
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await page.locator('.search-settings').evaluate(element => element.scrollIntoView({ block: 'start' }));
  await page.screenshot({ path: testInfo.outputPath('search-settings.png'), fullPage: true });
});

test('subagent presets choose across sites and keys, reset effort, and round trip multiple aliases', async ({ page }, testInfo) => {
  await bootstrap(page, true);
  const base = modelConfig.sites[0].keys[0].models[0];
  const additionalSite = { id: 'pool-site', name: 'Pool site', base_url: 'https://pool.example.test/v1', keys: [{ id: 'pool-key-a', name: 'Research key', has_secret: true, models: [{ ...base, id: 'pool-model-a', name: 'Research model', reasoning_efforts: ['medium'], reasoning_effort: 'medium' }] }, { id: 'pool-key-b', name: 'Review key', has_secret: true, models: [{ ...base, id: 'pool-model-b', name: 'Review model', reasoning_efforts: ['low'], reasoning_effort: '' }] }] };
  let configuration: Record<string, any> = { ...modelConfig, extra: { keep: true }, sites: [...modelConfig.sites, additionalSite] };
  const submissions: Record<string, any>[] = [];
  await page.route('**/api/config', route => {
    if (route.request().method() === 'PUT') { const body = route.request().postDataJSON(); submissions.push(body); configuration = { ...body, revision: configuration.revision + 1 }; }
    return route.fulfill({ json: configuration });
  });
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
  await page.goto('/');
  if (testInfo.project.name === 'mobile') await page.locator('.topbar').getByRole('button', { name: /切换到.*主题/ }).click();
  await openSidebar(page); await page.getByRole('button', { name: '工作台设置', exact: true }).click();
  await page.getByRole('button', { name: '子代理', exact: true }).click();
  await expect(page.getByText('按任务搭配模型', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: '保存配置' })).toBeDisabled();
  await page.getByRole('button', { name: '添加预设' }).click();
  const first = page.getByRole('article', { name: '子代理预设 1', exact: true });
  await first.getByLabel('预设名称').fill('Follow model default');
  const picker = page.getByLabel('预设 1 模型连接', { exact: true });
  await expect(picker.locator('optgroup')).toHaveCount(2);
  await expect(picker.locator('option')).toHaveText(['Test model · Test key', 'Research model · Research key', 'Review model · Review key']);
  await page.getByLabel('预设 1 思考等级').selectOption('effort:high');
  await picker.selectOption(JSON.stringify(['pool-site', 'pool-key-a', 'pool-model-a']));
  await expect(page.getByLabel('预设 1 思考等级')).toHaveValue('default');
  await expect(page.getByLabel('预设 1 思考等级').locator('option')).toHaveText(['模型默认 · medium', '不发送思考等级', 'medium']);
  await page.getByLabel('预设 1 思考等级').selectOption('effort:medium');
  await picker.selectOption(JSON.stringify(['pool-site', 'pool-key-b', 'pool-model-b']));
  await expect(page.getByLabel('预设 1 思考等级')).toHaveValue('default');
  await expect(first).toContainText('Pool site · Review key');
  await picker.selectOption(JSON.stringify(['site', 'key', 'model']));
  await page.getByRole('button', { name: '添加预设' }).click();
  await page.getByRole('article', { name: '子代理预设 2', exact: true }).getByLabel('预设名称').fill('No explicit effort');
  await page.getByLabel('预设 2 思考等级').selectOption('omit');
  await page.getByRole('button', { name: '添加预设' }).click();
  await page.getByRole('article', { name: '子代理预设 3', exact: true }).getByLabel('预设名称').fill('Deep analysis');
  await page.getByLabel('预设 3 思考等级').selectOption('effort:high');
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await page.locator('.settings-scroll').evaluate(element => { element.scrollTop = 0; });
  await page.screenshot({ path: testInfo.outputPath('subagent-presets.png'), fullPage: true });
  await page.getByRole('button', { name: '保存配置' }).click(); await expect(page.getByRole('button', { name: '保存配置' })).toBeDisabled();
  expect(submissions[0].subagent_models.map((entry: any) => entry.reasoning_effort)).toEqual([null, '', 'high']);
  expect(submissions[0].subagent_models.map((entry: any) => entry.selection)).toEqual([session.selection, session.selection, session.selection]);
  expect(new Set(submissions[0].subagent_models.map((entry: any) => entry.id)).size).toBe(3);
  expect(submissions[0].extra).toEqual({ keep: true });
  for (const entry of submissions[0].subagent_models) expect(Object.keys(entry).sort()).toEqual(['id', 'name', 'reasoning_effort', 'selection']);
  await page.getByRole('button', { name: '删除预设 2' }).click();
  await page.getByRole('article', { name: '子代理预设 2', exact: true }).getByLabel('预设名称').fill('Edited deep analysis');
  await page.getByRole('button', { name: '保存配置' }).click(); await expect(page.getByRole('button', { name: '保存配置' })).toBeDisabled();
  expect(submissions[1].subagent_models.map((entry: any) => [entry.name, entry.reasoning_effort])).toEqual([['Follow model default', null], ['Edited deep analysis', 'high']]);
  expect(await page.evaluate(() => Object.keys(localStorage))).toEqual(['justai-theme']);
  expect(await page.evaluate(() => JSON.stringify(localStorage))).not.toContain('Deep analysis');
});

for (const target of ['站点', 'Key', '模型']) test(`deleting a referenced ${target} blocks save with a pool repair link and retains its preset`, async ({ page }) => {
  await bootstrap(page, true);
  const pool = [{ id: 'preset-fixture', name: 'Keep this preset', selection: session.selection, reasoning_effort: 'high' }];
  let configuration: Record<string, any> = { ...structuredClone(modelConfig), subagent_models: pool };
  const submissions: Record<string, any>[] = [];
  await page.route('**/api/config', route => {
    if (route.request().method() === 'PUT') { const body = route.request().postDataJSON(); submissions.push(body); configuration = { ...body, revision: configuration.revision + 1 }; }
    return route.fulfill({ json: configuration });
  });
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
  await page.goto('/'); await openSidebar(page); await page.getByRole('button', { name: '工作台设置', exact: true }).click();
  page.once('dialog', dialog => void dialog.accept());
  await page.getByRole('button', { name: target === '站点' ? '删除站点' : `删除此${target === 'Key' ? ' Key' : '模型'}`, exact: true }).click();
  await page.getByRole('button', { name: '保存配置' }).click();
  await expect(page.locator('.settings-scroll > .alert')).toContainText('子代理模型池「Keep this preset」引用的站点、Key 或模型已被删除');
  expect(submissions).toHaveLength(0);
  await page.getByRole('button', { name: '前往子代理' }).click();
  await expect(page.getByLabel('预设名称')).toHaveValue('Keep this preset');
  await expect(page.getByLabel('预设 1 模型连接')).toHaveValue(JSON.stringify(['site', 'key', 'model']));
  await expect(page.getByLabel('预设 1 思考等级')).toHaveValue('effort:high');
  await expect(page.getByText('连接已移除，请重新选择模型或删除此预设。')).toBeVisible();
  await page.getByRole('button', { name: '删除预设 1' }).click();
  await page.getByRole('button', { name: '保存配置' }).click(); await expect(page.getByRole('button', { name: '保存配置' })).toBeDisabled();
  expect(submissions).toHaveLength(1); expect(submissions[0].subagent_models).toEqual([]);
});

test('legacy configs save an empty pool and backend pool errors link to the tab', async ({ page }) => {
  await bootstrap(page, true);
  const submissions: Record<string, any>[] = []; let reject = true;
  await page.route('**/api/config', route => {
    if (route.request().method() === 'PUT') {
      const body = route.request().postDataJSON(); submissions.push(body);
      if (reject) { reject = false; return route.fulfill({ status: 422, json: { error: 'subagent_models: selected key has no secret' } }); }
      return route.fulfill({ json: { ...body, revision: 4 } });
    }
    return route.fulfill({ json: modelConfig });
  });
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
  await page.goto('/'); await openSidebar(page); await page.getByRole('button', { name: '工作台设置', exact: true }).click();
  await page.getByLabel('站点名称').fill('Edited legacy site');
  await page.getByRole('button', { name: '保存配置' }).click();
  await expect(page.getByRole('alert')).toContainText('subagent_models');
  await page.getByRole('button', { name: '前往子代理' }).click();
  await expect(page.getByText('按任务搭配模型', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: '保存配置' }).click(); await expect(page.getByRole('button', { name: '保存配置' })).toBeDisabled();
  expect(submissions.map(body => body.subagent_models)).toEqual([[], []]);
});

test('subagent cards display actual provenance from events and results without rewriting history', async ({ page }) => {
  await controlledEvents(page); await bootstrap(page, true);
  await page.route('**/api/config', route => route.fulfill({ json: modelConfig }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [session] }));
  await page.route('**/api/sessions/test-session', route => route.fulfill({ json: { session, events: [event(1, 'tool_start', { call_id: 'catalog', name: 'subagent', args: { action: 'models', model_id: null } }), event(2, 'tool_end', { call_id: 'catalog', output: '{"models":[]}', is_error: false }), event(3, 'subagent', { id: 'child-a', task: 'Inspect event route', status: 'running', site_name: 'Historical site', model_name: 'Historical model', model: 'wire-a', model_id: 'opaque-pool-id', reasoning_effort: 'high' }), event(4, 'tool_start', { call_id: 'spawn', name: 'subagent', args: { action: 'spawn', task: 'Inspect result route', model_id: 'opaque-pool-id' } }), event(5, 'tool_end', { call_id: 'spawn', output: JSON.stringify({ id: 'child-b', status: 'running', site_name: 'Result site', model_name: 'Result model', model_id: 'opaque-pool-id', reasoning_effort: '' }), is_error: false }), event(6, 'subagent', { id: 'legacy-child', task: 'Legacy child', status: 'completed' })] } }));
  await page.goto('/'); await openSidebar(page); await page.getByRole('button', { name: /Browser test conversation/ }).click();
  await expect(page.locator('.subagent-card')).toHaveCount(3);
  await expect(page.locator('.subagent-caption')).toHaveText(['Historical site - Historical model · high', 'Result site - Result model']);
  await expect(page.locator('.tool-operation')).toHaveText(['查看可用子代理模型', 'Inspect result route']);
  await expect(page.locator('.subagent-card')).not.toContainText(['opaque-pool-id', 'opaque-pool-id', 'opaque-pool-id']);
  await emitSessionEvent(page, event(7, 'subagent', { id: 'child-a', status: 'completed', site_name: 'Current site', model_name: 'Current model', reasoning_effort: 'low' }));
  await emitSessionEvent(page, event(8, 'subagent', { id: 'child-b', status: 'completed' }));
  await expect(page.locator('.subagent-caption')).toHaveText(['Historical site - Historical model · high', 'Result site - Result model']);
  await expect(page.locator('.subagent-card').first()).toContainText('已完成');
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('web and image tool cards show the query, URL and image path safely', async ({ page }) => {
  await controlledEvents(page); await bootstrap(page, true);
  const tools = [
    { name: 'web_search', args: { query: 'original logo design', max_results: 5 }, summary: 'original logo design' },
    { name: 'web_fetch', args: { url: 'https://example.test/docs', max_chars: 12000 }, summary: 'https://example.test/docs' },
    { name: 'read_image', args: { path: 'artifacts/logo.png' }, summary: 'artifacts/logo.png' },
  ];
  const events = tools.flatMap((tool, index) => [event(index * 2 + 1, 'tool_start', { call_id: tool.name, ...tool }), event(index * 2 + 2, 'tool_end', { call_id: tool.name, output: '<script>untrusted fixture</script>', is_error: false })]);
  await page.route('**/api/config', route => route.fulfill({ json: modelConfig }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [session] }));
  await page.route('**/api/sessions/test-session', route => route.fulfill({ json: { session, events } }));
  await page.goto('/'); await openSidebar(page); await page.getByRole('button', { name: /Browser test conversation/ }).click();
  await expect(page.locator('.tool-operation')).toHaveText(tools.map(tool => tool.summary));
  for (const tool of tools) {
    const card = page.locator('.tool-card').filter({ has: page.locator('strong', { hasText: tool.name }) });
    await card.locator('summary').click();
    await expect(card.locator('.tool-details')).toContainText('<script>untrusted fixture</script>');
    await expect(card.locator('script')).toHaveCount(0);
  }
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('session route controls PATCH during a run and SSE keeps source history intact', async ({ page }) => {
  await controlledEvents(page); await bootstrap(page, true);
  const baseModel = modelConfig.sites[0].keys[0].models[0];
  const alternateSite = { id: 'second-site', name: 'Second site', base_url: 'https://other.test/v1', keys: [{ id: 'second-key', name: 'Second key', has_secret: true, models: [{ ...baseModel, id: 'second-model', name: 'Second model', model: 'actual-second' }, { ...baseModel, id: 'alternate-model', name: 'Alternate model', model: 'actual-alternate' }] }, { id: 'third-key', name: 'Third key', has_secret: true, models: [{ ...baseModel, id: 'third-model', name: 'Third model', model: 'actual-third' }] }] };
  const configuration = { ...modelConfig, sites: [...modelConfig.sites, alternateSite] };
  let saved = { ...session, status: 'running' }; let rejectNext = false;
  const patches: { selection: typeof session.selection; reasoning_effort: string | null }[] = [];
  await page.route('**/api/config', route => route.fulfill({ json: configuration }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [saved] }));
  await page.route('**/api/sessions/test-session', route => route.fulfill({ json: { session: saved, events: [event(1, 'user_message', { text: 'route fixture' }), event(2, 'assistant_source', { attempt_id: 'original', site_name: 'Actual original site', model_name: 'Actual original model', model: 'original-wire-model', reasoning_effort: 'low' }), event(3, 'assistant_message', { attempt_id: 'original', text: 'Original answer' }), event(4, 'assistant_message', { attempt_id: 'legacy', text: 'Legacy answer' })] } }));
  await page.route('**/api/sessions/test-session/files?*', route => route.fulfill({ json: { path: '', entries: [] } }));
  await page.route('**/api/sessions/test-session/settings', route => {
    expect(route.request().method()).toBe('PATCH'); expect(route.request().headers()['x-justai-request']).toBe('1');
    if (rejectNext) { rejectNext = false; return route.fulfill({ status: 422, json: { error: 'Fixture route validation failed' } }); }
    const body = route.request().postDataJSON(); patches.push(body); saved = { ...saved, ...body };
    return route.fulfill({ json: saved });
  });
  await page.goto('/'); await openSidebar(page); await page.getByRole('button', { name: /Browser test conversation/ }).click();
  await expect(page.getByText('Actual original site - Actual original model', { exact: true })).toBeVisible();
  await expect(page.locator('.assistant-source').filter({ hasText: 'Test site - Test model' })).toHaveCount(1);
  await page.getByLabel('调整会话模型').click();
  await expect(page.getByLabel('会话站点', { exact: true })).toBeEnabled();
  await page.getByLabel('会话站点', { exact: true }).selectOption('second-site');
  await expect(page.getByLabel('会话密钥', { exact: true })).toHaveValue('second-key');
  await page.getByLabel('会话模型', { exact: true }).selectOption('alternate-model');
  await expect(page.getByLabel('会话模型', { exact: true })).toHaveValue('alternate-model');
  await page.getByLabel('会话密钥', { exact: true }).selectOption('third-key');
  await expect(page.getByLabel('会话模型', { exact: true })).toHaveValue('third-model');
  await page.getByLabel('会话思考等级').selectOption('effort:high');
  rejectNext = true; await page.getByRole('button', { name: '应用', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Fixture route validation failed');
  await expect(page.locator('.assistant-source').filter({ hasText: 'Test site - Test model' })).toHaveCount(1);
  await page.getByRole('button', { name: '应用', exact: true }).click();
  await expect(page.getByText('已保存，将在下次模型请求时生效。')).toBeVisible();
  expect(patches[0]).toEqual({ selection: { site_id: 'second-site', key_id: 'third-key', model_id: 'third-model' }, reasoning_effort: 'high' });
  await expect(page.locator('.assistant-source').filter({ hasText: 'Second site - Third model' })).toHaveCount(1);
  await expect(page.getByText('Actual original site - Actual original model', { exact: true })).toBeVisible();
  await page.getByLabel('会话思考等级').selectOption('omit'); await page.getByRole('button', { name: '应用', exact: true }).click();
  await expect(page.getByRole('button', { name: '应用', exact: true })).toBeDisabled(); expect(patches[1].reasoning_effort).toBe('');
  await page.getByLabel('会话思考等级').selectOption('inherit'); await page.getByRole('button', { name: '应用', exact: true }).click();
  await expect(page.getByRole('button', { name: '应用', exact: true })).toBeDisabled(); expect(patches[2].reasoning_effort).toBeNull();
  await emitSessionEvent(page, event(5, 'session_settings', { selection: session.selection, reasoning_effort: 'low', site_name: 'Test site', model_name: 'Test model', model: 'test-model' }));
  await expect(page.getByLabel('会话站点', { exact: true })).toHaveValue('site');
  await expect(page.getByLabel('会话模型', { exact: true })).toHaveValue('model');
  await expect(page.getByLabel('会话思考等级')).toHaveValue('effort:low');
  await emitSessionEvent(page, event(6, 'assistant_source', { attempt_id: 'next', site_name: 'Request site', model_name: 'Request model', model: 'request-wire-model', reasoning_effort: 'low' }));
  await emitSessionEvent(page, event(7, 'assistant_message', { attempt_id: 'next', text: 'Next answer' }));
  await expect(page.getByText('Request site - Request model', { exact: true })).toBeVisible();
  await expect(page.getByText('Actual original site - Actual original model', { exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('lazy file tree refreshes from real events and keeps artifacts at each round end', async ({ page }, testInfo) => {
  await controlledEvents(page); await bootstrap(page, true);
  let rootRequests = 0; let srcRequests = 0; let changed = false;
  const rootEntries = () => [{ path: 'src', name: 'src', is_dir: true, size: 0 }, { path: 'index.html', name: 'index.html', is_dir: false, size: 12 }, ...(changed ? [{ path: 'new.png', name: 'new.png', is_dir: false, size: 24 }] : [])];
  await page.route('**/api/config', route => route.fulfill({ json: modelConfig }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [session] }));
  await page.route('**/api/sessions/test-session', route => route.fulfill({ json: { session, events: [event(1, 'user_message', { text: 'Build a page' }), event(2, 'tool_start', { call_id: 'bash', name: 'bash', args: { command: '\n  printf "hello"\npwd' } }), event(3, 'tool_end', { call_id: 'bash', output: 'hello', is_error: false }), event(4, 'assistant_message', { attempt_id: 'first', text: 'Page ready' }), event(5, 'turn_files', { run_id: 'first', files: [{ path: 'index.html', name: 'index.html', kind: 'new' }, { path: 'removed.txt', name: 'removed.txt', kind: 'deleted' }] }), event(6, 'user_message', { text: 'Add a diagram' }), event(7, 'assistant_message', { attempt_id: 'second', text: 'Diagram ready' }), event(8, 'turn_files', { run_id: 'second', files: [{ path: 'src/chart.svg', name: 'chart.svg', kind: 'new' }] })] } }));
  await page.route('**/api/sessions/test-session/files?*', route => {
    const path = new URL(route.request().url()).searchParams.get('path');
    if (!path) { rootRequests++; return route.fulfill({ json: { path: '', entries: rootEntries() } }); }
    expect(path).toBe('src'); srcRequests++;
    return route.fulfill({ json: { path: 'src', entries: [{ path: 'src/chart.svg', name: 'chart.svg', is_dir: false, size: 12 }] } });
  });
  await page.goto('/'); await openSidebar(page); await page.getByRole('button', { name: /Browser test conversation/ }).click();
  await expect(page.locator('.tool-operation')).toHaveText('  printf "hello"');
  await page.locator('.tool-card > summary').click(); await expect(page.locator('.tool-details')).toContainText('pwd');
  const cards = page.getByRole('region', { name: '本轮文件产物' });
  await expect(cards).toHaveCount(2);
  await expect(cards.nth(0)).toContainText('index.html'); await expect(cards.nth(1)).toContainText('chart.svg');
  const preview = cards.nth(0).getByRole('link', { name: /index.html/ });
  await expect(preview).toHaveAttribute('href', '/api/sessions/test-session/file?path=index.html');
  await expect(preview).toHaveAttribute('target', '_blank'); await expect(preview).toHaveAttribute('rel', 'noopener noreferrer');
  await expect(cards.nth(1).getByRole('link')).toHaveAttribute('href', '/api/sessions/test-session/file?path=src%2Fchart.svg');
  await expect(cards.nth(0).getByRole('link', { name: /removed/ })).toHaveCount(0);
  expect(srcRequests).toBe(0);
  await openFiles(page); const tree = page.locator('#session-files');
  await expect(tree.getByRole('button', { name: '目录 src', exact: true })).toBeVisible(); expect(rootRequests).toBe(1); expect(srcRequests).toBe(0);
  await tree.getByRole('button', { name: '目录 src', exact: true }).click();
  await expect(tree.getByRole('link', { name: /chart.svg/ })).toBeVisible(); expect(srcRequests).toBe(1);
  await expect(tree.getByText('本轮新建')).toHaveCount(1);
  await emitSessionEvent(page, event(9, 'file_activity', { path: 'src/chart.svg', status: 'editing', is_new: false }));
  await expect(tree.getByRole('status')).toContainText('正在编辑');
  await expect(tree.getByRole('link', { name: /chart.svg/ })).toContainText('编辑中');
  changed = true;
  await emitSessionEvent(page, event(10, 'file_changed', { path: 'new.png', name: 'new.png', kind: 'new', is_dir: false }));
  await expect(tree.getByRole('link', { name: /new.png/ })).toBeVisible(); expect(rootRequests).toBeGreaterThan(1); expect(srcRequests).toBe(1);
  await emitSessionEvent(page, event(11, 'file_activity', { path: 'src/chart.svg', status: 'done', is_new: true }));
  await expect(tree.locator('.file-current-activity')).toHaveCount(0);
  await expect(tree.getByRole('link', { name: /chart.svg/ })).toContainText('本轮新建');
  if (testInfo.project.name === 'mobile') {
    await page.keyboard.press('Escape'); await expect(tree).not.toBeInViewport(); await expect(page.getByRole('button', { name: '打开文件面板' })).toBeFocused();
  }
  await emitSessionEvent(page, event(12, 'turn_files', { run_id: 'third', files: [] }));
  await expect(page.getByRole('region', { name: '本轮文件产物' })).toHaveCount(3);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('manual compression remains an optional icon action with a cost confirmation', async ({ page }) => {
  await controlledEvents(page); await bootstrap(page, true);
  let compactRequests = 0;
  await page.route('**/api/config', route => route.fulfill({ json: modelConfig }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [session] }));
  await page.route('**/api/sessions/test-session', route => route.fulfill({ json: { session, events: [event(1, 'user_message', { text: 'Existing conversation' })] } }));
  await page.route('**/api/sessions/test-session/files?*', route => route.fulfill({ json: { path: '', entries: [] } }));
  await page.route('**/api/sessions/test-session/compact', route => { compactRequests++; return route.fulfill({ json: { run_id: 'compact-fixture' } }); });
  await page.goto('/'); await openSidebar(page); await page.getByRole('button', { name: /Browser test conversation/ }).click();
  const compact = page.getByRole('button', { name: '手动压缩上下文', exact: true });
  await expect(compact).toBeEnabled(); await expect(compact).toHaveText('');
  page.once('dialog', dialog => { expect(dialog.message()).toContain('模型费用'); void dialog.dismiss(); });
  await compact.click(); expect(compactRequests).toBe(0);
  page.once('dialog', dialog => void dialog.accept()); await compact.click();
  await expect.poll(() => compactRequests).toBe(1);
});

test('detail once then SSE after sequence, authoritative replacement, duplicate suppression and UUID', async ({ page }) => {
  await bootstrap(page, true);
  await page.route('**/api/config', route => route.fulfill({ json: modelConfig }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [session] }));
  let details = 0; let sentId = '';
  await page.route('**/api/sessions/test-session', route => { details++; return route.fulfill({ json: { session, events: [event(4, 'user_message', { id: 'u', text: 'Test request' }), event(7, 'assistant_delta', { attempt_id: 'attempt', text: 'Provisional ' })] } }); });
  await page.route('**/api/sessions/test-session/events?after=*', route => {
    expect(new URL(route.request().url()).searchParams.get('after')).toBe('7');
    const complete = event(9, 'assistant_message', { attempt_id: 'attempt', id: 'answer', text: 'Authoritative test answer' });
    return route.fulfill({ contentType: 'text/event-stream', body: `id: 9\nevent: update\ndata: ${JSON.stringify(complete)}\n\nid: 9\nevent: update\ndata: ${JSON.stringify(complete)}\n\n` });
  });
  await page.route('**/api/sessions/test-session/messages', route => {
    const body = route.request().postDataJSON(); sentId = body.client_message_id;
    expect(route.request().headers()['x-justai-request']).toBe('1');
    return route.fulfill({ status: 202, json: { run_id: 'test-run' } });
  });
  await page.goto('/'); await openSidebar(page);
  await page.getByRole('button', { name: /Browser test conversation/ }).click();
  await expect(page.getByText('Authoritative test answer', { exact: true })).toHaveCount(1);
  await expect(page.getByText('Provisional ', { exact: true })).toHaveCount(0);
  expect(details).toBe(1);
  await page.getByRole('textbox', { name: '消息内容' }).fill('A test message');
  await page.getByRole('button', { name: '发送消息' }).click();
  await expect(page.getByRole('textbox', { name: '消息内容' })).toHaveValue('');
  expect(sentId).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
});

test('question stack answers during waiting or automatic compaction and retains AI history', async ({ page }, testInfo) => {
  const optional = { ...fixtureQuestion, id: 'optional-fixture', call_id: 'optional', question: '有偏好的报告格式吗？', blocking: false, options: [], created_at: '2026-11-13T12:01:00Z' };
  await page.route('**/api/sessions/test-session/questions', route => route.fulfill({ json: [optional, fixtureQuestion] }));
  await page.route('**/api/sessions/test-session/goals', route => route.fulfill({ json: [{ ...fixtureGoal, status: 'active', progress: '已完成初始界面检查，等待验证方案。', rounds: 7 }] }));
  let answerRequests = 0; let releaseAnswer!: () => void;
  const answerGate = new Promise<void>(resolve => { releaseAnswer = resolve; });
  await page.route('**/api/sessions/test-session/questions/blocking-fixture/answer', async route => {
    answerRequests++; expect(route.request().postDataJSON()).toEqual({ answer: '完整验证' });
    expect(route.request().headers()['x-justai-request']).toBe('1');
    await answerGate; await route.fulfill({ json: { ...fixtureQuestion, status: 'answered', answer: '完整验证', updated_at: '2026-11-13T12:02:00Z' } });
  });
  let optionalAttempts = 0;
  await page.route('**/api/sessions/test-session/questions/optional-fixture/answer', route => {
    optionalAttempts++; expect(route.request().postDataJSON()).toEqual({ answer: '简洁的 Markdown 报告' });
    return optionalAttempts === 1 ? route.fulfill({ status: 503, json: { error: 'Fixture temporary answer failure' } }) : route.fulfill({ json: { ...optional, status: 'answered', answer: '简洁的 Markdown 报告', updated_at: '2026-11-13T12:03:00Z' } });
  });
  let dialogs = 0; page.on('dialog', dialog => { dialogs++; void dialog.dismiss(); });
  await autonomySession(page);
  const stack = page.locator('.questions-panel');
  await expect(stack.locator('.question-text')).toHaveText([optional.question, fixtureQuestion.question]);
  await expect(page.getByRole('button', { name: '停止', exact: true })).toBeVisible();
  await expect(stack.getByRole('button', { name: /删除|撤回|忽略/ })).toHaveCount(0);
  await page.locator('.goals-panel > summary').click();
  await expect(page.locator('.goal-progress')).toContainText('等待验证方案');
  await expect(page.getByRole('button', { name: '停止', exact: true })).toBeInViewport();
  expect(await page.locator('.conversation-scroll').evaluate(element => element.clientHeight)).toBeGreaterThan(80);
  await page.evaluate(() => { const badge = document.createElement('div'); badge.textContent = 'TEST FIXTURES · 非生产数据'; badge.style.cssText = 'position:fixed;top:8px;left:50%;transform:translateX(-50%);z-index:1000;background:#88273f;color:white;padding:5px 10px;border-radius:6px;font:10px sans-serif;pointer-events:none'; document.body.append(badge); });
  await page.screenshot({ path: testInfo.outputPath('autonomy-fixtures.png'), fullPage: true });
  await page.locator('.questions-panel > summary').click();
  await expect(page.locator('.goals-panel')).not.toHaveAttribute('open');
  await page.screenshot({ path: testInfo.outputPath('autonomy-questions-fixtures.png'), fullPage: true });
  const themeControl = (testInfo.project.name === 'mobile' ? page.locator('.topbar') : page.locator('.sidebar')).getByRole('button', { name: /切换到.*主题/ });
  const originalTheme = await page.locator('html').getAttribute('data-theme'); await themeControl.click();
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', originalTheme!);
  await page.screenshot({ path: testInfo.outputPath('autonomy-alternate-theme-fixtures.png'), fullPage: true }); await themeControl.click();
  const blocking = page.getByRole('article', { name: fixtureQuestion.question });
  await blocking.getByRole('radio', { name: /完整验证/ }).check();
  await blocking.getByRole('button', { name: '提交回答' }).click();
  await expect(blocking.getByRole('button', { name: '提交回答' })).toBeDisabled();
  expect(answerRequests).toBe(1); releaseAnswer();
  await expect(stack.locator('.question-card')).toHaveCount(1);
  await emitSessionEvent(page, event(6, 'status', { status: 'auto_compacting' }));
  await expect(page.locator('.auto-compacting')).toContainText('无需确认');
  await expect(page.getByRole('button', { name: '手动压缩上下文' })).toBeDisabled();
  await stack.getByRole('textbox').fill('简洁的 Markdown 报告');
  await stack.getByRole('button', { name: '提交回答' }).click();
  await expect(stack.getByRole('alert')).toContainText('Fixture temporary answer failure');
  await expect(stack.getByRole('textbox')).toHaveValue('简洁的 Markdown 报告');
  await stack.getByRole('button', { name: '提交回答' }).click();
  await expect(page.locator('.questions-panel')).toHaveCount(0);
  const withdrawn = { ...optional, id: 'withdrawn-fixture', question: '不再需要的补充信息', status: 'withdrawn', updated_at: '2026-11-13T12:04:00Z' };
  await emitSessionEvent(page, event(7, 'question_withdrawn', { question: withdrawn }));
  await page.locator('.question-history > summary').click();
  await expect(page.locator('.question-history')).toContainText('AI 已撤回');
  await expect(page.locator('.question-history')).toContainText('简洁的 Markdown 报告');
  await expect(page.locator('.question-history')).toContainText('完整验证');
  expect(dialogs).toBe(0); expect(answerRequests).toBe(1); expect(optionalAttempts).toBe(2);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('goal list creates pending tasks and supports explicit lifecycle with agent-only completion', async ({ page }) => {
  let goal = { ...fixtureGoal }; const actions: string[] = []; let creates = 0;
  await page.route('**/api/sessions/test-session/goals', route => {
    if (route.request().method() === 'POST') { creates++; const body = route.request().postDataJSON(); expect(body).toEqual({ title: '完成工作台验收', objective: fixtureGoal.objective }); return route.fulfill({ json: goal }); }
    return route.fulfill({ json: [] });
  });
  await page.route('**/api/sessions/test-session/goals/goal-fixture', route => {
    const { action } = route.request().postDataJSON(); actions.push(action);
    expect(route.request().method()).toBe('PATCH');
    goal = { ...goal, status: action === 'pause' ? 'paused' : action === 'cancel' ? 'cancelled' : 'active', updated_at: `2026-11-13T12:0${actions.length}:00Z` };
    return route.fulfill({ json: goal });
  });
  await autonomySession(page, 'completed');
  await page.locator('.goals-panel > summary').click();
  await page.getByRole('button', { name: '添加目标', exact: true }).click();
  await page.getByLabel('目标标题').fill(fixtureGoal.title); await page.getByLabel('目标说明').fill(fixtureGoal.objective);
  await page.getByRole('button', { name: '添加到待办' }).click();
  const card = page.locator('.goal-card'); await expect(card).toContainText('待开始');
  expect(creates).toBe(1); expect(actions).toEqual([]);
  await expect(card.getByRole('checkbox')).toHaveCount(0);
  await card.getByRole('button', { name: '开始', exact: true }).click(); await expect(card).toContainText('进行中');
  await card.getByRole('button', { name: '暂停', exact: true }).click(); await expect(card).toContainText('已暂停');
  await card.getByRole('button', { name: '恢复', exact: true }).click(); await expect(card).toContainText('进行中');
  const progress = { ...goal, status: 'active', rounds: 12, progress: '桌面、手机和文件树验证通过。', updated_at: '2026-11-13T12:04:00Z' };
  await emitSessionEvent(page, event(6, 'goal', { goal: progress }));
  await expect(card).toContainText('已执行 12 轮'); await expect(card).toContainText(progress.progress);
  await expect(card.getByRole('button', { name: /完成/ })).toHaveCount(0);
  await emitSessionEvent(page, event(7, 'goal', { goal: { ...progress, status: 'completed', updated_at: '2026-11-13T12:05:00Z' } }));
  await expect(card).toContainText('已完成'); await expect(card.getByLabel('AI 已确认完成')).toBeVisible(); await expect(card.getByRole('button')).toHaveCount(0);
  await emitSessionEvent(page, event(8, 'goal', { goal: { ...fixtureGoal, id: 'other', title: '下一项待办' } }));
  const next = page.locator('.goal-card').filter({ hasText: '下一项待办' });
  await page.route('**/api/sessions/test-session/goals/other', route => { actions.push('cancel'); return route.fulfill({ json: { ...fixtureGoal, id: 'other', title: '下一项待办', status: 'cancelled' } }); });
  await next.getByRole('button', { name: '取消', exact: true }).click(); await expect(next).toContainText('已取消');
  expect(actions).toEqual(['start', 'pause', 'resume', 'cancel']);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('late initial GET cannot overwrite live SSE snapshots and failures remain retryable', async ({ page }) => {
  let release!: () => void; const gate = new Promise<void>(resolve => { release = resolve; }); let requested = false;
  await page.route('**/api/sessions/test-session/questions', async route => { requested = true; await gate; await route.fulfill({ json: [fixtureQuestion] }); });
  let goalLoads = 0;
  await page.route('**/api/sessions/test-session/goals', route => { goalLoads++; return goalLoads === 1 ? route.fulfill({ status: 503, json: { error: 'Fixture goal load failure' } }) : route.fulfill({ json: [{ ...fixtureGoal, status: 'pending' }] }); });
  await autonomySession(page);
  await expect.poll(() => requested).toBe(true);
  const answered = { ...fixtureQuestion, status: 'answered', answer: 'SSE 已提交的回答' };
  await emitSessionEvent(page, event(6, 'question_answer', { question: answered }));
  await emitSessionEvent(page, event(7, 'goal', { goal: { ...fixtureGoal, status: 'active', rounds: 8, progress: '来自最新 AI 事件' } }));
  release(); await page.locator('.goals-panel > summary').click();
  await expect(page.locator('.goals-panel')).toContainText('Fixture goal load failure');
  await expect(page.locator('.questions-panel')).toHaveCount(0);
  await page.locator('.question-history > summary').click(); await expect(page.locator('.question-history')).toContainText(answered.answer);
  await page.locator('.goals-panel').getByRole('button', { name: '重试', exact: true }).click();
  await expect.poll(() => goalLoads).toBe(2);
  await expect(page.locator('.question-history')).toContainText(answered.answer);
  // Retry GET is authoritative for mutable nonterminal goal status; old event
  // history is not replayed over it, and terminal answers never regress.
  await expect(page.locator('.goals-panel')).toContainText('待开始');
  await emitSessionEvent(page, event(8, 'status', { status: 'completed' }));
  await expect(page.locator('.topbar .session-status')).toContainText('等待回答');
});

test('SSE wins equal-timestamp initial GET and in-flight goal PATCH races', async ({ page }) => {
  let releaseGet!: () => void; const getGate = new Promise<void>(resolve => { releaseGet = resolve; }); let getRequested = false;
  await page.route('**/api/sessions/test-session/goals', async route => { getRequested = true; await getGate; await route.fulfill({ json: [fixtureGoal] }); });
  let releasePatch!: () => void; const patchGate = new Promise<void>(resolve => { releasePatch = resolve; }); let patchRequested = false;
  await page.route('**/api/sessions/test-session/goals/goal-fixture', async route => { patchRequested = true; expect(route.request().postDataJSON()).toEqual({ action: 'resume' }); await patchGate; await route.fulfill({ json: { ...fixtureGoal, status: 'active' } }); });
  await autonomySession(page, 'completed');
  await expect.poll(() => getRequested).toBe(true);
  await emitSessionEvent(page, event(6, 'goal', { goal: { ...fixtureGoal, status: 'paused', progress: '来自最新暂停事件', rounds: 3 } }));
  releaseGet(); await page.locator('.goals-panel > summary').click();
  const card = page.locator('.goal-card');
  await expect(card).toContainText('已暂停'); await expect(card).toContainText('来自最新暂停事件');
  await card.getByRole('button', { name: '恢复', exact: true }).click();
  await expect.poll(() => patchRequested).toBe(true);
  await emitSessionEvent(page, event(7, 'goal', { goal: { ...fixtureGoal, status: 'paused', progress: '恢复后被新的 AI 事件暂停', rounds: 4 } }));
  await expect(card).toContainText('恢复后被新的 AI 事件暂停');
  releasePatch();
  await expect(card.getByRole('button', { name: '恢复', exact: true })).toBeEnabled();
  await expect(card).toContainText('已暂停'); await expect(card).toContainText('已执行 4 轮');
});

test('computer status GET never launches and host requires one clear authorization', async ({ page }, testInfo) => {
  let state = structuredClone(computerStatus); const requests: string[] = []; const changes: boolean[] = [];
  await page.route('**/api/computer', route => { requests.push(route.request().method()); return route.fulfill({ json: state }); });
  await page.route('**/api/computer/host', route => {
    expect(route.request().method()).toBe('POST'); expect(route.request().headers()['x-justai-request']).toBe('1');
    const { enabled } = route.request().postDataJSON(); changes.push(enabled); state.host.enabled = enabled;
    return route.fulfill({ json: state });
  });
  let dialogs = 0; page.on('dialog', dialog => { dialogs++; void dialog.dismiss(); });
  await autonomySession(page, 'completed'); expect(requests).toEqual([]); expect(changes).toEqual([]);
  await page.getByRole('button', { name: '打开计算机面板' }).click(); const panel = page.getByRole('dialog', { name: '计算机', exact: true });
  await expect(panel).toContainText('可用 · 未启动'); await expect(panel).toContainText('TEST FIXTURE: KWin dependency unavailable');
  await expect(panel).toContainText('root 不可用'); await expect(panel).not.toContainText('最高 root 已授予');
  await expect(panel).toContainText('查看状态不会启动浏览器或桌面'); expect(requests.every(method => method === 'GET')).toBe(true);
  await panel.screenshot({ path: testInfo.outputPath('computer-panel-fixture.png') });
  const toggle = panel.getByRole('switch', { name: '允许宿主接管' }); await expect(toggle).not.toBeChecked(); await toggle.click();
  await expect(panel.locator('.host-authorization')).toContainText('主桌面'); await expect(panel.locator('.host-authorization')).toContainText('自动让行');
  expect(changes).toEqual([]); await expect(panel.getByRole('button', { name: '授权并开启' })).toBeDisabled();
  // Checking the explanation but cancelling is not an authorization to mutate.
  await panel.getByRole('checkbox', { name: '我已了解并允许宿主接管' }).check(); await panel.getByRole('button', { name: '暂不开启' }).click(); await toggle.click();
  expect(changes).toEqual([]); await expect(panel.locator('.host-authorization')).toBeVisible();
  await panel.getByRole('button', { name: '授权并开启' }).click();
  await expect(toggle).toBeChecked(); await expect(toggle).toBeEnabled(); await toggle.click(); await expect(toggle).not.toBeChecked(); await expect(toggle).toBeEnabled();
  await toggle.click(); await expect(toggle).toBeChecked(); expect(changes).toEqual([true, false, true]); expect(dialogs).toBe(0);
  expect(requests.every(method => method === 'GET')).toBe(true); expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await page.keyboard.press('Escape'); await expect(panel).toHaveCount(0); await expect(page.getByRole('button', { name: '打开计算机面板' })).toBeFocused();
  await page.getByRole('button', { name: '打开计算机面板' }).click(); await expect(toggle).toBeEnabled(); await toggle.click(); await expect(toggle).not.toBeChecked(); await expect(toggle).toBeEnabled();
  await toggle.click(); await expect(toggle).toBeChecked(); await expect(panel.locator('.host-authorization')).toHaveCount(0); expect(changes).toEqual([true, false, true, false, true]);
});

test('global emergency stop stays direct while busy and host toggle cannot mutate', async ({ page }) => {
  let busy = true; let stops = 0; let hosts = 0; let dialogs = 0;
  await page.route('**/api/computer', route => route.fulfill({ json: { ...computerStatus, busy, owner: 'other-session' } }));
  await page.route('**/api/computer/host', route => { hosts++; return route.fulfill({ json: computerStatus }); });
  await page.route('**/api/computer/stop', route => {
    stops++; expect(route.request().method()).toBe('POST'); expect(route.request().headers()['x-justai-request']).toBe('1'); busy = false;
    return route.fulfill({ json: { ok: true } });
  });
  page.on('dialog', dialog => { dialogs++; void dialog.dismiss(); });
  await autonomySession(page, 'completed'); await page.getByRole('button', { name: '打开计算机面板' }).click(); const panel = page.getByRole('dialog', { name: '计算机', exact: true });
  await expect(panel).toContainText('计算机忙碌'); await expect(panel).toContainText('其他会话持有');
  await expect(panel.getByRole('switch', { name: '允许宿主接管' })).toBeDisabled();
  await panel.getByRole('button', { name: /紧急停止所有计算机操作/ }).click(); await expect(panel).toContainText('已发送全局紧急停止');
  await expect(panel).toContainText('当前空闲'); expect(stops).toBe(1); expect(hosts).toBe(0); expect(dialogs).toBe(0);
});

test('computer status and mutation failures remain honest and retryable', async ({ page }) => {
  let stage = 0; let stops = 0;
  await page.route('**/api/computer', route => stage === 0 ? route.fulfill({ status: 503, json: { error: 'TEST FIXTURE status unavailable' } }) : stage === 1 ? route.fulfill({ json: { busy: false } }) : route.fulfill({ json: computerStatus }));
  await page.route('**/api/computer/stop', route => { stops++; return route.fulfill({ status: 503, json: { error: 'TEST FIXTURE stop failed' } }); });
  await page.route('**/api/computer/host', route => route.fulfill({ status: 409, json: { error: 'TEST FIXTURE host busy' } }));
  await autonomySession(page, 'completed'); await page.getByRole('button', { name: '打开计算机面板' }).click(); const panel = page.getByRole('dialog', { name: '计算机', exact: true });
  await expect(panel.getByRole('alert')).toContainText('TEST FIXTURE status unavailable'); await expect(panel).toContainText('实际权限：未确认');
  await expect(panel.getByRole('switch')).toBeDisabled(); await expect(panel.getByRole('button', { name: /紧急停止所有/ })).toBeEnabled();
  stage = 1; await panel.getByRole('button', { name: '重试状态' }).click(); await expect(panel.getByRole('alert')).toContainText('状态响应不完整');
  stage = 2; await panel.getByRole('button', { name: '重试状态' }).click(); await expect(panel).toContainText('root 不可用');
  await panel.getByRole('switch').click(); await panel.getByRole('checkbox').check(); await panel.getByRole('button', { name: '授权并开启' }).click();
  await expect(panel.getByRole('alert')).toContainText('TEST FIXTURE host busy'); await expect(panel.getByRole('switch')).not.toBeChecked();
  await panel.getByRole('button', { name: /紧急停止所有/ }).click(); await expect(panel.getByRole('alert')).toContainText('TEST FIXTURE stop failed');
  await expect(panel.getByRole('button', { name: /紧急停止所有/ })).toBeEnabled(); expect(stops).toBe(1);
});

test('computer screenshot fold appears at tool end using current-session authenticated PNG only', async ({ page }) => {
  await page.context().addCookies([{ name: 'justai-fixture-auth', value: 'offline-test', url: 'http://localhost:48991' }]);
  const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jf1cAAAAASUVORK5CYII=', 'base64');
  let images = 0;
  await page.route('**/api/sessions/test-session/computer/image/capture-fixture', route => {
    images++; expect(route.request().headers().cookie).toContain('justai-fixture-auth=offline-test');
    return route.fulfill({ contentType: 'image/png', body: png });
  });
  await page.route('**/api/computer', route => route.fulfill({ json: computerStatus }));
  await autonomySession(page, 'running');
  await emitSessionEvent(page, event(6, 'tool_start', { call_id: 'computer-shot', name: 'computer', args: { action: 'screenshot', target: 'browser', ref_id: 'ref-fixture' } }));
  await emitSessionEvent(page, event(7, 'computer', { action: 'screenshot', target: 'browser', summary: 'TEST FIXTURE current session screenshot', is_error: false, capture_id: 'capture-fixture', width: 1, height: 1 }));
  const tool = page.locator('.tool-card').filter({ hasText: 'screenshot · browser · ref-fixture' }); await tool.locator('summary').click();
  await expect(tool.locator('img')).toHaveCount(0);
  await emitSessionEvent(page, event(8, 'tool_end', { call_id: 'computer-shot', output: '{"text":"Captured","data":"base64-must-not-leak-fixture"}', is_error: false }));
  await expect(tool.locator('img')).toHaveAttribute('src', '/api/sessions/test-session/computer/image/capture-fixture');
  await expect(tool).not.toContainText('base64-must-not-leak-fixture');
  await emitSessionEvent(page, { ...event(9, 'computer', { action: 'screenshot', target: 'desktop', summary: 'OTHER SESSION MUST NOT LEAK', is_error: false, capture_id: 'other-capture' }), session_id: 'other-session' });
  await page.getByRole('button', { name: '打开计算机面板' }).click(); const panel = page.getByRole('dialog', { name: '计算机', exact: true });
  await expect(panel).toContainText('TEST FIXTURE current session screenshot'); await expect(panel).not.toContainText('OTHER SESSION MUST NOT LEAK');
  await expect(panel.locator('.computer-capture img')).toHaveAttribute('src', '/api/sessions/test-session/computer/image/capture-fixture');
  await expect.poll(() => images).toBeGreaterThan(0);
  await page.keyboard.press('Escape');
  await page.route('**/api/sessions', route => route.fulfill({ json: [session, { ...session, id: 'second-session', title: 'SECOND SESSION NO CAPTURE' }] }));
  await page.route('**/api/sessions/second-session', route => route.fulfill({ json: { session: { ...session, id: 'second-session', title: 'SECOND SESSION NO CAPTURE' }, events: [] } }));
  await openSidebar(page); await page.getByRole('button', { name: '刷新会话与配置' }).click(); await page.getByRole('button', { name: /SECOND SESSION NO CAPTURE/ }).click();
  await page.getByRole('button', { name: '打开计算机面板' }).click(); await expect(page.getByRole('dialog', { name: '计算机', exact: true })).toContainText('暂无本会话截图');
  await expect(page.getByRole('dialog').locator('.computer-capture')).toHaveCount(0);
});

// Paste fixtures use only synthetic paste-event data, never the system clipboard.
async function pasteFixture(page: Page, text = 'PASTE TEXT', count = 2, shift = false) {
  return page.getByLabel('消息内容').evaluate((element, options) => {
    const input = element as HTMLTextAreaElement;
    input.focus();
    if (options.shift) input.dispatchEvent(new KeyboardEvent('keydown', { key: 'V', ctrlKey: true, shiftKey: true, bubbles: true }));
    const data = new DataTransfer(); data.setData('text/plain', options.text);
    data.setData('text/html', '<img src=x onerror="window.pasteHtmlExecuted=true"><script>window.pasteHtmlExecuted=true</script>');
    // Distinct pixels, not merely distinct metadata: the new contract collapses
    // identical bytes within one paste even if the OS assigns different names.
    const canvas = document.createElement('canvas'); canvas.width = 2; canvas.height = 2;
    const context = canvas.getContext('2d')!;
    for (let n = 0; n < options.count; n++) {
      context.fillStyle = `rgb(${n * 20}, 80, 150)`; context.fillRect(0, 0, 2, 2);
      const png = Uint8Array.from(atob(canvas.toDataURL('image/png').split(',')[1]), char => char.charCodeAt(0));
      data.items.add(new File([png], `TEST-FIXTURE-${n}.png`, { type: 'image/png' }));
    }
    const event = new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }); input.dispatchEvent(event);
    return event.defaultPrevented;
  }, { text, count, shift });
}

type PasteSource = { pixel: number; name?: string; modified?: number };
async function pasteSources(page: Page, items: PasteSource[], files: PasteSource[], text = '') {
  return page.getByLabel('消息内容').evaluate((element, fixture) => {
    const input = element as HTMLTextAreaElement; input.focus();
    const make = (source: { pixel: number; name?: string; modified?: number }) => {
      const canvas = document.createElement('canvas'); canvas.width = 2; canvas.height = 2;
      const context = canvas.getContext('2d')!; context.fillStyle = `rgb(${source.pixel}, 80, 150)`; context.fillRect(0, 0, 2, 2);
      const png = Uint8Array.from(atob(canvas.toDataURL('image/png').split(',')[1]), char => char.charCodeAt(0));
      const bytes = new Uint8Array(1024); bytes.set(png); // Valid PNG with trailing padding: equal size, different actual pixels.
      return new File([bytes], source.name ?? 'same-metadata.png', { type: 'image/png', lastModified: source.modified ?? 1 });
    };
    const event = new ClipboardEvent('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(event, 'clipboardData', { value: { items: fixture.items.map(source => ({ kind: 'file', getAsFile: () => make(source) })), files: fixture.files.map(make), getData: (kind: string) => kind === 'text/plain' ? fixture.text : '' } });
    input.dispatchEvent(event); return event.defaultPrevented;
  }, { items, files, text });
}

test('paste two images plus text at cursor sends one guarded message without executing HTML', async ({ page }, testInfo) => {
  await autonomySession(page, 'completed');
  let body: any;
  await page.route('**/api/sessions/test-session/messages', async route => { body = route.request().postDataJSON(); expect(route.request().headers()['x-justai-request']).toBe('1'); await route.fulfill({ status: 202, json: { run_id: 'paste-run' } }); });
  const input = page.getByLabel('消息内容'); await input.fill('before AFTER');
  await input.evaluate(element => (element as HTMLTextAreaElement).setSelectionRange(7, 12));
  expect(await pasteFixture(page, '中间')).toBe(true);
  await expect(input).toHaveValue('before 中间'); await expect(page.locator('.attachment-preview')).toHaveCount(2);
  expect(await input.evaluate(element => (element as HTMLTextAreaElement).selectionStart)).toBe(9);
  expect(await page.evaluate(() => (window as any).pasteHtmlExecuted)).toBeUndefined();
  await page.screenshot({ path: testInfo.outputPath('paste-two-images.png'), fullPage: true });
  await page.getByRole('button', { name: '发送消息' }).click(); await expect.poll(() => body?.images?.length).toBe(2);
  expect(body.text).toBe('before 中间'); expect(body.delivery_mode).toBe('after_call'); expect(body.client_message_id).toMatch(/^[a-f0-9-]{36}$/);
  expect(await page.evaluate(() => Object.keys(localStorage))).toEqual(['justai-theme']);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('Shift paste consumes available image/text data, text-only paste stays native, no clipboard read permission', async ({ page }) => {
  await autonomySession(page, 'completed');
  await page.evaluate(() => {
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { read: () => { throw new Error('Clipboard read forbidden'); }, readText: () => { throw new Error('Clipboard read forbidden'); } } });
  });
  expect(await pasteFixture(page, 'SHIFT AVAILABLE', 2, true)).toBe(true);
  await expect(page.getByLabel('消息内容')).toHaveValue('SHIFT AVAILABLE'); await expect(page.locator('.attachment-preview')).toHaveCount(2);
  expect(await pasteFixture(page, 'OS STRIPPED IMAGES', 0, true)).toBe(false);
  await expect(page.locator('.attachment-preview')).toHaveCount(2);
  // A deliberate second paste is not deduplicated against the first.
  await pasteFixture(page, '', 2); await expect(page.locator('.attachment-preview')).toHaveCount(4);
});

test('readonly composer ignores paste, unsupported image model preserves available text honestly', async ({ page }) => {
  await autonomySession(page, 'completed');
  await page.getByLabel('消息内容').evaluate(element => { (element as HTMLTextAreaElement).readOnly = true; });
  expect(await pasteFixture(page, 'READONLY', 2)).toBe(false);
  await expect(page.getByLabel('消息内容')).toHaveValue(''); await expect(page.locator('.attachment-preview')).toHaveCount(0);
  const unsupported = structuredClone(modelConfig); unsupported.sites[0].keys[0].models[0].supports_images = false;
  await page.route('**/api/config', route => route.fulfill({ json: unsupported }));
  await page.reload(); await openSidebar(page); await page.getByRole('button', { name: /Browser test conversation/ }).click();
  await expect(page.getByLabel('消息内容')).toBeVisible(); await pasteFixture(page, 'TEXT STILL AVAILABLE', 2);
  await expect(page.getByLabel('消息内容')).toHaveValue('TEXT STILL AVAILABLE'); await expect(page.locator('.attachment-preview')).toHaveCount(0);
  await expect(page.getByRole('alert')).toContainText('未启用图片'); await expect(page.getByRole('button', { name: '添加图片' })).toBeDisabled();
});

test('offline IndexedDB outbox survives reload and online reconnect uses latest SSE cursor', async ({ page }) => {
  await autonomySession(page, 'completed');
  await emitSessionEvent(page, event(12, 'context_warning', { message: 'TEST FIXTURE latest cursor' }));
  await page.context().setOffline(true);
  await pasteFixture(page, 'OFFLINE DURABLE', 2); await expect(page.locator('.attachment-preview')).toHaveCount(2);
  await page.getByRole('button', { name: '发送消息' }).click(); await expect(page.getByLabel('消息内容')).toHaveValue('');
  await expect(page.getByRole('region', { name: '本地待发送消息' })).toContainText('OFFLINE DURABLE');
  const before = await page.evaluate(async () => new Promise<any[]>(resolve => { const request = indexedDB.open('justai-message-outbox'); request.onsuccess = () => { const db = request.result; const read = db.transaction('messages').objectStore('messages').getAll(); read.onsuccess = () => { db.close(); resolve(read.result); }; }; }));
  expect(before[0].images).toHaveLength(2); expect(before[0].client_message_id).toBeTruthy(); expect(before[0].delivery_mode).toBe('after_call');
  await page.reload(); await openSidebar(page); await page.getByRole('button', { name: /Browser test conversation/ }).click();
  await expect(page.getByRole('region', { name: '本地待发送消息' })).toContainText('OFFLINE DURABLE');
  let delivered: any;
  await page.route('**/api/sessions/test-session/messages', async route => { delivered = route.request().postDataJSON(); await route.fulfill({ status: 202, json: { run_id: 'offline-run' } }); });
  await emitSessionEvent(page, event(17, 'context_warning', { message: 'TEST FIXTURE newer cursor' }));
  await page.context().setOffline(false);
  await expect.poll(() => delivered?.client_message_id).toBe(before[0].client_message_id);
  expect(delivered.images).toHaveLength(2); await expect(page.getByRole('region', { name: '本地待发送消息' })).toHaveCount(0);
  await expect.poll(() => page.evaluate(() => (window as any).testEventUrls.at(-1))).toContain('after=17');
});

test('lost 202 keeps UUID for dedup, queued acceptance renders once and delivery clears badge', async ({ page }) => {
  await autonomySession(page, 'running');
  const attempts: any[] = []; const accepted = new Map<string, string>();
  await page.route('**/api/sessions/test-session/messages', async route => {
    const body = route.request().postDataJSON(); attempts.push(body); accepted.set(body.client_message_id, 'dedup-run');
    if (attempts.length === 1) { await route.abort('connectionreset'); return; }
    await route.fulfill({ status: 202, json: { run_id: accepted.get(body.client_message_id) } });
  });
  await page.getByLabel('消息内容').fill('LOST 202 FIXTURE'); await page.getByRole('button', { name: '发送消息' }).click();
  await expect.poll(() => attempts.length).toBe(2); expect(attempts[0]).toEqual(attempts[1]); expect(accepted.size).toBe(1);
  await emitSessionEvent(page, event(6, 'user_message', { ...attempts[0], run_id: 'dedup-run', queued: true }));
  await emitSessionEvent(page, event(7, 'user_message', { ...attempts[0], run_id: 'dedup-run', queued: true }));
  await expect(page.locator('.user-message').filter({ hasText: 'LOST 202 FIXTURE' })).toHaveCount(1); await expect(page.locator('.queued-badge')).toBeVisible();
  await emitSessionEvent(page, event(8, 'message_delivered', { client_message_id: attempts[0].client_message_id }));
  await expect(page.locator('.queued-badge')).toHaveCount(0);
});

test('SSE confirmation settles a lost response before retry and failed validation stays manually discardable', async ({ page }) => {
  await autonomySession(page, 'running'); let attempts = 0; let submitted: any;
  await page.route('**/api/sessions/test-session/messages', async route => {
    attempts++; submitted = route.request().postDataJSON();
    await emitSessionEvent(page, event(6, 'user_message', { ...submitted, queued: true, run_id: 'confirmed-run' }));
    await route.abort('connectionreset');
  });
  await page.getByLabel('消息内容').fill('CONFIRMED LOST RESPONSE'); await page.getByRole('button', { name: '发送消息' }).click();
  await expect(page.locator('.user-message').filter({ hasText: 'CONFIRMED LOST RESPONSE' })).toHaveCount(1);
  await expect(page.getByRole('region', { name: '本地待发送消息' })).toHaveCount(0);
  await page.evaluate(() => window.dispatchEvent(new Event('online')));
  await expect.poll(() => attempts).toBe(1);
  await page.route('**/api/sessions/test-session/messages', async route => { attempts++; await route.fulfill({ status: 400, json: { error: 'FIXTURE validation failed' } }); });
  await page.getByLabel('消息内容').fill('INVALID FIXTURE'); await page.getByRole('button', { name: '发送消息' }).click();
  const local = page.getByRole('region', { name: '本地待发送消息' }); await expect(local).toContainText('未通过验证');
  await page.evaluate(() => window.dispatchEvent(new Event('online'))); expect(attempts).toBe(2);
  await local.getByRole('button', { name: '丢弃' }).click(); await expect(local).toHaveCount(0);
});

test('IndexedDB save failure preserves the draft and attachments without any HTTP attempt', async ({ page }) => {
  await autonomySession(page, 'completed'); let requests = 0;
  await page.route('**/api/sessions/test-session/messages', async route => { requests++; await route.fulfill({ status: 202, json: { run_id: 'must-not-be-sent' } }); });
  await pasteFixture(page, 'PRESERVE UNSAVED DRAFT', 2); await expect(page.locator('.attachment-preview')).toHaveCount(2);
  await page.evaluate(() => { indexedDB.open = () => { throw new DOMException('TEST FIXTURE denied', 'SecurityError'); }; });
  await page.getByRole('button', { name: '发送消息' }).click();
  await expect(page.getByRole('alert').first()).toContainText('本地消息保存失败');
  await expect(page.getByLabel('消息内容')).toHaveValue('PRESERVE UNSAVED DRAFT'); await expect(page.locator('.attachment-preview')).toHaveCount(2); expect(requests).toBe(0);
});

test('sent queued Esc upgrades the same UUID without sending a draft; new sends stay after_call and Stop stays separate', async ({ page }, testInfo) => {
  await autonomySession(page, 'waiting_for_network'); const requests: any[] = []; let stops = 0;
  await page.route('**/api/sessions/test-session/messages', async route => { requests.push(route.request().postDataJSON()); await route.fulfill({ status: 202, json: { run_id: 'running-fixture' } }); });
  await page.route('**/api/sessions/test-session/cancel', async route => { stops++; await route.fulfill({ json: { ok: true } }); });
  await expect(page.getByRole('button', { name: '停止', exact: true })).toBeVisible(); await expect(page.locator('.working-state')).toContainText('等待网络恢复');
  const input = page.getByLabel('消息内容'); await input.fill('IME 中文输入');
  await input.evaluate(element => { element.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true })); element.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })); });
  expect(requests).toHaveLength(0); await expect(input).toHaveValue('IME 中文输入');
  await input.evaluate(element => element.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true })));
  await page.getByRole('button', { name: '发送消息' }).click(); await expect.poll(() => requests.length).toBe(1); expect(requests[0].delivery_mode).toBe('after_call');
  const interrupts: string[] = [];
  await page.route('**/api/sessions/test-session/messages/interrupt', async route => { const body = route.request().postDataJSON(); interrupts.push(body.client_message_id); expect(route.request().headers()['x-justai-request']).toBe('1'); await route.fulfill({ json: { client_message_id: body.client_message_id, promoted: true, delivered: false, pending_acceptance: false } }); });
  await emitSessionEvent(page, event(6, 'user_message', { ...requests[0], queued: true, run_id: 'queued-esc-fixture' }));
  await input.fill('UNSENT NEXT DRAFT');
  await input.evaluate(element => { element.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true })); element.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })); });
  expect(interrupts).toHaveLength(0);
  await input.evaluate(element => element.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true })));
  await input.press('Escape'); await expect.poll(() => interrupts.length).toBe(1); expect(interrupts[0]).toBe(requests[0].client_message_id);
  await expect(input).toHaveValue('UNSENT NEXT DRAFT'); expect(requests).toHaveLength(1); expect(stops).toBe(0);
  await emitSessionEvent(page, event(7, 'message_interrupted', { client_message_id: requests[0].client_message_id, delivery_mode: 'immediate' }));
  await expect(page.locator('.queued-badge')).toContainText('强制介入');
  await input.press('Escape'); expect(interrupts).toHaveLength(1);
  await emitSessionEvent(page, event(8, 'message_delivered', { client_message_id: requests[0].client_message_id }));
  await expect(page.locator('.queued-badge')).toHaveCount(0); await input.press('Escape'); expect(interrupts).toHaveLength(1);
  await page.screenshot({ path: testInfo.outputPath('running-composer-stop.png'), fullPage: true });
  const themeButton = (testInfo.project.name === 'mobile' ? page.locator('.topbar') : page.locator('.sidebar')).getByRole('button', { name: /切换到.*主题/ });
  await themeButton.first().click(); await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark'); await page.screenshot({ path: testInfo.outputPath('running-composer-stop-dark.png'), fullPage: true, animations: 'disabled' });
  await page.getByRole('button', { name: '发送消息' }).click(); await expect.poll(() => requests.length).toBe(2); expect(requests[1].delivery_mode).toBe('after_call');
  await page.getByRole('button', { name: '停止', exact: true }).click(); expect(stops).toBe(1);
  await emitSessionEvent(page, event(9, 'network_restored', {})); await emitSessionEvent(page, event(10, 'status', { status: 'running' }));
  await expect(page.locator('.working-state')).toContainText('Agent 正在工作'); await expect(page.locator('.timeline')).toContainText('网络已恢复');
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('one-paste bytes deduplicate items/files with different wrappers but preserve metadata-colliding pixels and deliberate repaste', async ({ page }) => {
  await autonomySession(page, 'completed');
  await pasteSources(page, [{ pixel: 20, name: 'item.png', modified: 10 }], [{ pixel: 20, name: 'file-wrapper.png', modified: 999 }]);
  await expect(page.locator('.attachment-preview')).toHaveCount(1);
  await pasteSources(page, [{ pixel: 20, name: 'again.png', modified: 30 }, { pixel: 20, name: 'again-wrapper.png', modified: 31 }], []);
  await expect(page.locator('.attachment-preview')).toHaveCount(2);
  await pasteSources(page, [{ pixel: 40 }], [{ pixel: 80 }]);
  await expect(page.locator('.attachment-preview')).toHaveCount(4);
  const sources = await page.locator('.attachment-preview img').evaluateAll(images => images.map(image => (image as HTMLImageElement).src));
  expect(sources[0]).toBe(sources[1]); expect(sources[2]).not.toBe(sources[3]);
});

test('paste counts eight unique images after cross-channel dedup and preserves text/draft when a later paste exceeds eight', async ({ page }) => {
  await autonomySession(page, 'completed');
  const seven = Array.from({ length: 7 }, (_, n) => ({ pixel: n * 20, name: `item-${n}.png`, modified: n }));
  await pasteSources(page, seven, seven.map(source => ({ ...source, name: `other-${source.name}`, modified: 999 })));
  await expect(page.locator('.attachment-preview')).toHaveCount(7);
  await pasteSources(page, [{ pixel: 200 }], [{ pixel: 200, name: 'eighth-other.png', modified: 888 }], 'EIGHT UNIQUE');
  await expect(page.locator('.attachment-preview')).toHaveCount(8);
  await expect(page.getByRole('alert')).toHaveCount(0);
  await pasteSources(page, [{ pixel: 200 }], [{ pixel: 200 }], ' TEXT PRESERVED');
  await expect(page.getByRole('alert')).toContainText('8');
  await expect(page.locator('.attachment-preview')).toHaveCount(8);
  await expect(page.getByLabel('消息内容')).toHaveValue('EIGHT UNIQUE TEXT PRESERVED');
});

test('uploading two intentionally identical images does not apply paste deduplication', async ({ page }) => {
  await autonomySession(page, 'completed');
  const buffer = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jf1cAAAAASUVORK5CYII=', 'base64');
  await page.locator('input[type="file"]').setInputFiles([{ name: 'intentional-one.png', mimeType: 'image/png', buffer }, { name: 'intentional-two.png', mimeType: 'image/png', buffer }]);
  await expect(page.locator('.attachment-preview')).toHaveCount(2);
});

test('paste read failures report a clear error without deleting existing text or partial attachment state', async ({ page }) => {
  await autonomySession(page, 'completed'); await page.getByLabel('消息内容').fill('EXISTING ');
  await page.evaluate(() => { FileReader.prototype.readAsDataURL = function () { queueMicrotask(() => this.dispatchEvent(new ProgressEvent('error'))); }; });
  await pasteSources(page, [{ pixel: 20 }], [{ pixel: 20, name: 'other-wrapper.png' }], 'PASTED TEXT');
  await expect(page.getByRole('alert')).toContainText('图片读取失败');
  await expect(page.getByLabel('消息内容')).toHaveValue('EXISTING PASTED TEXT');
  await expect(page.locator('.attachment-preview')).toHaveCount(0);
});

test('draft lightbox is responsive, traps and returns focus, switches images and Esc never escalates a pending message', async ({ page }, testInfo) => {
  await autonomySession(page, 'running'); let interrupts = 0;
  await page.route('**/api/sessions/test-session/messages/interrupt', async route => { interrupts++; await route.fulfill({ json: { promoted: true, delivered: false, pending_acceptance: false } }); });
  await emitSessionEvent(page, event(6, 'user_message', { text: 'QUEUED LIGHTBOX FIXTURE', client_message_id: '11111111-1111-4111-8111-111111111111', queued: true, delivery_mode: 'after_call' }));
  await pasteFixture(page, 'DRAFT LIGHTBOX', 2); await expect(page.locator('.attachment-preview')).toHaveCount(2);
  const opener = page.getByRole('button', { name: '预览待发送图片 1', exact: true }); await opener.click();
  const dialog = page.getByRole('dialog', { name: '图片预览', exact: true }); await expect(dialog).toBeVisible();
  const close = dialog.getByRole('button', { name: '关闭弹窗' }); await expect(close).toBeFocused();
  await page.keyboard.press('Shift+Tab'); await expect(dialog.getByRole('button', { name: '下一张图片' })).toBeFocused();
  await page.keyboard.press('Tab'); await expect(close).toBeFocused();
  await expect(dialog.getByRole('status')).toHaveText('1 / 2');
  const first = await dialog.locator('img').getAttribute('src');
  await dialog.getByRole('button', { name: '下一张图片' }).click(); await expect(dialog.getByRole('status')).toHaveText('2 / 2');
  expect(await dialog.locator('img').getAttribute('src')).not.toBe(first);
  await page.keyboard.press('ArrowLeft'); await expect(dialog.getByRole('status')).toHaveText('1 / 2');
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  expect(await dialog.evaluate(el => Array.from(el.querySelectorAll('[aria-label],[aria-description]')).some(node => /base64|iVBOR/.test(`${node.getAttribute('aria-label')}${node.getAttribute('aria-description')}`)))).toBe(false);
  await page.screenshot({ path: testInfo.outputPath('draft-image-lightbox.png'), fullPage: true });
  await page.keyboard.press('Escape'); await expect(dialog).toHaveCount(0); await expect(opener).toBeFocused();
  await expect(page.getByLabel('消息内容')).toHaveValue('DRAFT LIGHTBOX'); expect(interrupts).toBe(0);
});

test('historical user image buttons preview inline raster only with multi-image backdrop/close/Esc focus return and no remote fetch', async ({ page }) => {
  await autonomySession(page, 'running'); let interrupts = 0; let remoteImages = 0;
  await page.route('**/api/sessions/test-session/messages/interrupt', async route => { interrupts++; await route.fulfill({ json: { promoted: true, delivered: false, pending_acceptance: false } }); });
  await page.route('https://remote-image.example.test/**', async route => { remoteImages++; await route.abort(); });
  await pasteFixture(page, '', 2); await expect(page.locator('.attachment-preview')).toHaveCount(2);
  const images = await page.locator('.attachment-preview img').evaluateAll(nodes => nodes.map(node => ({ mime: 'image/png', data: (node as HTMLImageElement).src.split(',')[1] })));
  await emitSessionEvent(page, event(6, 'user_message', { text: 'HISTORY PREVIEW FIXTURE', client_message_id: '22222222-2222-4222-8222-222222222222', queued: true, images: [...images, { mime: 'image/svg+xml', data: 'PHN2Zy8+' }] }));
  await emitSessionEvent(page, event(7, 'assistant_delta', { text: '![remote](https://remote-image.example.test/leak.png)' }));
  await expect(page.locator('.message-images button')).toHaveCount(2); await expect(page.locator('.message-images a')).toHaveCount(0);
  const opener = page.getByRole('button', { name: '查看附件 2', exact: true }); await opener.click();
  const dialog = page.getByRole('dialog', { name: '图片预览', exact: true }); await expect(dialog.getByRole('status')).toHaveText('2 / 2');
  await page.keyboard.press('ArrowRight'); await expect(dialog.getByRole('status')).toHaveText('1 / 2');
  await page.locator('.modal-backdrop').click({ position: { x: 2, y: 2 } }); await expect(dialog).toHaveCount(0); await expect(opener).toBeFocused();
  await opener.click(); await dialog.getByRole('button', { name: '关闭弹窗' }).click(); await expect(opener).toBeFocused();
  await opener.click(); await page.keyboard.press('Escape'); await expect(dialog).toHaveCount(0); await expect(opener).toBeFocused();
  expect(interrupts).toBe(0); expect(remoteImages).toBe(0); expect(page.context().pages()).toHaveLength(1);
});

test('Esc on an unsent draft does nothing, dialogs and file drawer do not steal it, session changes reset the hint and default', async ({ page }) => {
  await autonomySession(page, 'running'); const submitted: any[] = []; let interrupts = 0;
  await page.route('**/api/sessions/test-session/messages', async route => { submitted.push(route.request().postDataJSON()); await route.fulfill({ status: 202, json: { run_id: 'default-fixture' } }); });
  await page.route('**/api/sessions/test-session/messages/interrupt', async route => { interrupts++; await route.fulfill({ json: { promoted: true, delivered: false, pending_acceptance: false } }); });
  const input = page.getByLabel('消息内容'); await input.fill('UNSENT ESC'); await input.press('Escape');
  expect(submitted).toHaveLength(0); expect(interrupts).toBe(0); await expect(input).toHaveValue('UNSENT ESC');
  await page.getByRole('button', { name: '发送消息' }).click(); await expect.poll(() => submitted.length).toBe(1); expect(submitted[0].delivery_mode).toBe('after_call');
  await emitSessionEvent(page, event(6, 'user_message', { ...submitted[0], queued: true }));
  if ((page.viewportSize()?.width ?? 1600) <= 1180) {
    await page.getByRole('button', { name: '打开文件面板' }).click(); await page.keyboard.press('Escape'); expect(interrupts).toBe(0);
  }
  await page.route('**/api/sessions/test-session/messages/interrupt', async route => { interrupts++; await route.fulfill({ status: 403, json: { error: 'TEST FIXTURE interrupt denied' } }); });
  await input.fill('KEEP DRAFT ON ERROR'); await input.press('Escape'); await expect.poll(() => interrupts).toBe(1);
  await expect(page.getByRole('alert').first()).toContainText('TEST FIXTURE interrupt denied'); await expect(input).toHaveValue('KEEP DRAFT ON ERROR');
  await page.route('**/api/sessions', route => route.fulfill({ json: [session, { ...session, id: 'second-session', title: 'ESC SECOND SESSION' }] }));
  await page.route('**/api/sessions/second-session', route => route.fulfill({ json: { session: { ...session, id: 'second-session', title: 'ESC SECOND SESSION' }, events: [] } }));
  await page.route('**/api/sessions/second-session/messages', async route => { submitted.push(route.request().postDataJSON()); await route.fulfill({ status: 202, json: { run_id: 'second-default' } }); });
  await openSidebar(page); await page.getByRole('button', { name: '刷新会话与配置' }).click(); await page.getByRole('button', { name: /ESC SECOND SESSION/ }).click();
  await expect(input).toHaveValue(''); await input.fill('SECOND DEFAULT'); await input.press('Escape');
  expect(interrupts).toBe(1); await page.getByRole('button', { name: '发送消息' }).click(); await expect.poll(() => submitted.length).toBe(2); expect(submitted[1].delivery_mode).toBe('after_call');
});

test('offline sent-message Esc persists the same UUID and immediate mode across reload without changing the next draft', async ({ page }) => {
  await autonomySession(page, 'running'); await page.context().setOffline(true);
  const input = page.getByLabel('消息内容'); await input.fill('OFFLINE ESC SENT');
  await page.getByRole('button', { name: '发送消息' }).click(); await expect(input).toHaveValue('');
  const saved = async () => page.evaluate(async () => new Promise<any[]>(resolve => { const request = indexedDB.open('justai-message-outbox'); request.onsuccess = () => { const db = request.result; const read = db.transaction('messages').objectStore('messages').getAll(); read.onsuccess = () => { db.close(); resolve(read.result); }; }; }));
  const before = await saved(); expect(before).toHaveLength(1); expect(before[0].delivery_mode).toBe('after_call');
  await input.fill('DO NOT SEND THIS DRAFT'); await input.press('Escape');
  await expect.poll(async () => (await saved())[0]?.delivery_mode).toBe('immediate');
  await expect(input).toHaveValue('DO NOT SEND THIS DRAFT'); await expect(page.locator('.delivery-explanation')).toContainText('已保存强制介入请求');
  const escalated = await saved(); expect(escalated[0].client_message_id).toBe(before[0].client_message_id); expect(escalated[0].escalation_pending).toBe(true);
  await page.reload(); await openSidebar(page); await page.getByRole('button', { name: /Browser test conversation/ }).click();
  await expect(page.getByRole('region', { name: '本地待发送消息' })).toContainText('尽快介入');
  const requests: any[] = []; const interrupts: string[] = [];
  await page.route('**/api/sessions/test-session/messages', async route => { requests.push(route.request().postDataJSON()); await route.fulfill({ status: 202, json: { run_id: 'offline-escalated' } }); });
  await page.route('**/api/sessions/test-session/messages/interrupt', async route => { const body = route.request().postDataJSON(); interrupts.push(body.client_message_id); await route.fulfill({ json: { client_message_id: body.client_message_id, promoted: requests.length > 0, delivered: false, pending_acceptance: requests.length === 0 } }); });
  await page.context().setOffline(false);
  await expect.poll(() => requests.length).toBe(1); expect(requests[0].delivery_mode).toBe('immediate'); expect(requests[0].client_message_id).toBe(before[0].client_message_id);
  await expect(page.getByRole('region', { name: '本地待发送消息' })).toHaveCount(0);
  expect(interrupts.every(id => id === before[0].client_message_id)).toBe(true);
});

test('Esc while an old after_call HTTP request is in flight escalates without a duplicate message or UUID change', async ({ page }) => {
  await autonomySession(page, 'running'); let release!: () => void;
  const ack = new Promise<void>(resolve => { release = resolve; }); const sent: any[] = []; const interrupts: string[] = [];
  await page.route('**/api/sessions/test-session/messages', async route => { sent.push(route.request().postDataJSON()); await ack; await route.fulfill({ status: 202, json: { run_id: 'inflight-escalated' } }); });
  await page.route('**/api/sessions/test-session/messages/interrupt', async route => { const body = route.request().postDataJSON(); interrupts.push(body.client_message_id); await route.fulfill({ json: { client_message_id: body.client_message_id, promoted: true, delivered: false, pending_acceptance: false } }); });
  const input = page.getByLabel('消息内容'); await input.fill('INFLIGHT ESC SENT'); await page.getByRole('button', { name: '发送消息' }).click();
  await expect.poll(() => sent.length).toBe(1); await expect(input).toHaveValue(''); expect(sent[0].delivery_mode).toBe('after_call');
  await input.fill('NEXT UNSENT DRAFT'); await input.press('Escape');
  await expect.poll(() => interrupts.length).toBeGreaterThan(0); expect(interrupts[0]).toBe(sent[0].client_message_id);
  release(); await expect(page.getByRole('region', { name: '本地待发送消息' })).toHaveCount(0);
  expect(sent).toHaveLength(1); expect(interrupts.every(id => id === sent[0].client_message_id)).toBe(true); await expect(input).toHaveValue('NEXT UNSENT DRAFT');
});
