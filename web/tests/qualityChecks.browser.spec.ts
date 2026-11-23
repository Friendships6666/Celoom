import { test, expect, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';

// Synthetic route fixtures only: this file starts no server and calls no model.
const selection = { site_id: 'quality-site', key_id: 'quality-key', model_id: 'quality-model' };
const otherSelection = { ...selection, model_id: 'other-model' };
const source = { site_name: 'Quality fixture site', model_name: 'Quality fixture model', model: 'requested-model', endpoint: 'https://fixture.example.test/v1/responses', api: 'responses', config_revision: 4 };
const network = { proxy_mode: 'direct', proxy_url: '', no_proxy: '', connect_timeout_secs: 10, headers_timeout_secs: 120, idle_timeout_secs: 300, request_timeout_secs: 1800, max_attempts: 3 };
const model = { id: selection.model_id, name: 'Quality fixture model', model: 'requested-model', supports_images: true, max_output_tokens: 4096, context_window: 32000, reasoning_efforts: ['low'], reasoning_effort: 'low', strict_tools: true, omit_tool_strict: true, cache_routing: true, reasoning_replay: false, send_store: true };
const config = { revision: 4, sites: [{ id: selection.site_id, name: source.site_name, base_url: 'https://fixture.example.test/v1', keys: [{ id: selection.key_id, name: 'Saved fixture key', has_secret: true, models: [model, { ...model, id: 'other-model', name: 'Other model' }] }] }], network };
function plan(input: any) {
  const count = (input.groups.includes('routing') ? 2 + input.instruction_probes.length * 2 : 0) + (input.groups.includes('cache') ? 4 + (input.extended_cache ? 8 : 0) : 0) + (input.groups.includes('protocol') ? 2 : 0);
  const networkVariants = input.groups.includes('network') ? ['connection', 'http_head', 'http_head', 'http_head', ...(input.network_trace ? ['route_trace'] : [])] : [];
  return { selection: input.selection, source: { ...source, model: input.selection.model_id === 'other-model' ? 'other-reported' : source.model }, request_count: count + networkVariants.length, provider_request_count: count, network_request_count: networkVariants.length, max_network_http_requests: networkVariants.length ? 4 : 0, network_trace: !!input.network_trace, max_output_tokens_per_request: count ? 256 : 0, max_output_tokens_total: 256 * count, estimated_input_tokens_upper: count ? input.context_size === 'large' ? 500000 : 20000 : 0, estimated_input_bytes_total: count ? input.context_size === 'large' ? 1000000 : 40000 : 0, context_size: input.context_size, extended_cache: input.extended_cache, probes: [...Array.from({ length: count }, (_, n) => ({ id: `fixture-${n}`, label: `合成探针 ${n + 1}`, group: 'routing', variant: n === 0 ? 'control' : 'child_role', repeat: n % 2 + 1, input_bytes: 300, max_output_tokens: 256 })), ...networkVariants.map((variant, n) => ({ id: `network-${n}`, label: `网络 ${variant}`, group: 'network', variant, repeat: variant === 'http_head' ? n : 1, input_bytes: 0, max_output_tokens: 0 }))], notes: ['Synthetic fixture plan; no actual provider requests.'] };
}
const defaultInput = { selection, groups: ['routing', 'cache', 'protocol'], instruction_probes: ['ignore_previous', 'child_role', 'independent_task'], context_size: 'small', extended_cache: false, network_trace: false, pricing: null };
function run(input = defaultInput, status = 'running') {
  return { id: 'quality-fixture-run', selection: input.selection, source, request: input, plan: plan(input), status, created_at: '2026-11-16T12:00:00Z', updated_at: '2026-11-16T12:00:00Z', completed_probes: 0, results: [], summary: {}, error: null };
}
async function settings(page: Page) {
  await page.goto('/');
  if ((page.viewportSize()?.width ?? 1600) <= 680) await page.getByRole('button', { name: '打开会话侧栏' }).click();
  await page.getByRole('button', { name: '工作台设置', exact: true }).click();
}
async function openChecks(page: Page) { await page.getByRole('button', { name: '模型质量检查', exact: true }).click(); await expect(page.getByRole('dialog', { name: '模型质量检查', exact: true })).toBeVisible(); }

test.beforeEach(async ({ page }) => {
  await page.route('**/*', route => new URL(route.request().url()).origin === 'http://localhost:48991' ? route.fallback() : route.abort('blockedbyclient'));
  await page.route('http://localhost:48991/**', async route => {
    const url = new URL(route.request().url());
    if (url.pathname.startsWith('/api/')) { await route.fulfill({ status: 404, json: { error: 'Unmocked quality fixture endpoint' } }); return; }
    const path = url.pathname === '/' ? 'index.html' : url.pathname.slice(1); const file = resolve(process.cwd(), 'dist', path);
    if (!file.startsWith(resolve(process.cwd(), 'dist') + '/')) throw new Error('Invalid fixture asset path');
    await route.fulfill({ body: await readFile(file), contentType: path.endsWith('.js') ? 'text/javascript' : path.endsWith('.css') ? 'text/css' : path.endsWith('.svg') ? 'image/svg+xml' : 'text/html' });
  });
  await page.route('**/api/bootstrap', route => route.fulfill({ json: { initialized: true, authenticated: true, allow_setup: false, version: 'fixture', workspace: '/tmp/fixture' } }));
  await page.route('**/api/config', route => route.fulfill({ json: config }));
  await page.route('**/api/sessions', route => route.fulfill({ json: [] }));
});

test('quality checks preview low-cost plan then start once, poll, cancel and reopen persisted record without rerun', async ({ page }, testInfo) => {
  let current: any = null; let posts = 0; let polls = 0; let cancels = 0; let exports = 0; const plans: any[] = []; const queries: string[] = [];
  await page.route('**/api/model-checks**', async route => {
    const url = new URL(route.request().url()); const path = url.pathname;
    if (path.endsWith('/plan')) { const input = route.request().postDataJSON(); plans.push(input); expect(route.request().headers()['x-celoom-request']).toBe('1'); return route.fulfill({ json: plan(input) }); }
    if (path.endsWith('/cancel')) { cancels++; current = { ...current, status: 'cancelled', updated_at: '2026-11-16T12:00:03Z' }; return route.fulfill({ json: current }); }
    if (path.endsWith('/export')) { exports++; return route.fulfill({ json: current }); }
    if (path === '/api/model-checks') {
      if (route.request().method() === 'POST') { posts++; const input = route.request().postDataJSON(); current = run(input); return route.fulfill({ status: 202, json: current }); }
      queries.push(url.search); return route.fulfill({ json: current ? [current, { ...current, id: 'foreign-run', selection: otherSelection }] : [] });
    }
    polls++; current = { ...current, updated_at: '2026-11-16T12:00:01Z', completed_probes: 1, results: [{ id: 'one', label: '子代理指令对照', group: 'routing', variant: 'child_role', repeat: 1, status: 'ok', observation: { http_status: 200, first_token_ms: 18, header_latency_ms: 8, total_ms: 62, protocol: 'responses_sse', terminal_status: 'completed', response_model: 'reported-other-model', reported_api: 'self-reported-api', reported_provider: 'fixture-provider', system_fingerprint: 'fixture-fingerprint', usage: { input_tokens: 100, output_tokens: 4 }, text_sample: '323', headers: { 'x-request-id': 'fixture-request' } }, analysis: { model_match: false, answer_correct: true, tool_valid: null, explicit_refusal: false }, cost: { currency: 'CNY', estimated_cost: null, complete: false, notes: [] } }], summary: { findings: [{ severity: 'warning', code: 'model_mismatch', message: '返回模型自报与请求不一致，不能证明底层权重。', probe_ids: ['one'] }], totals: { requests_completed: 1, input_tokens: 100, cached_tokens: null, output_tokens: 4, usage_complete: false, estimated_cost: null, currency: null } } };
    return route.fulfill({ json: current });
  });
  await settings(page); await openChecks(page);
  await expect(page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true })).toBeEnabled();
  expect(posts).toBe(0); expect(plans[0]).toEqual(defaultInput);
  expect(new URLSearchParams(queries[0]).get('site_id')).toBe(selection.site_id); expect(new URLSearchParams(queries[0]).get('key_id')).toBe(selection.key_id); expect(new URLSearchParams(queries[0]).get('model_id')).toBe(selection.model_id);
  await expect(page.getByLabel('大型上下文', { exact: false })).not.toBeChecked();
  await page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true }).click();
  await expect.poll(() => polls).toBeGreaterThan(0); await expect(page.getByText('reported-other-model', { exact: true })).toBeVisible();
  await expect(page.getByRole('table')).toContainText('18 ms'); await expect(page.getByRole('table')).toContainText('未配置费率'); await expect(page.getByRole('table')).toContainText('未返回');
  await page.getByRole('button', { name: '取消本次检查', exact: true }).click(); await expect.poll(() => cancels).toBe(1);
  await expect(page.getByText('已取消 · 1 / 14 次')).toBeVisible(); const settledPolls = polls;
  await expect(page.getByRole('table')).toContainText('self-reported-api');
  await expect(page.getByRole('table')).toContainText('fixture-provider');
  await page.getByRole('table').scrollIntoViewIfNeeded();
  await page.screenshot({ path: testInfo.outputPath('quality-check-table.png'), fullPage: true });
  const download = page.waitForEvent('download'); await page.getByRole('button', { name: '导出 JSON', exact: true }).click(); await download; expect(exports).toBe(1);
  await page.getByRole('button', { name: '返回模型设置', exact: true }).click(); await openChecks(page);
  await expect(page.getByText('已取消 · 1 / 14 次')).toBeVisible(); expect(posts).toBe(1); expect(polls).toBe(settledPolls);
  await expect(page.getByRole('option', { name: /foreign-run/ })).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('quality checks show all exact phrases, explicit larger plans and optional noninvented pricing', async ({ page }) => {
  const plans: any[] = []; let posts = 0;
  await page.route('**/api/model-checks**', route => {
    if (new URL(route.request().url()).pathname.endsWith('/plan')) { const input = route.request().postDataJSON(); plans.push(input); return route.fulfill({ json: plan(input) }); }
    if (route.request().method() === 'POST') { posts++; return route.fulfill({ status: 202, json: run(route.request().postDataJSON()) }); }
    return route.fulfill({ json: [] });
  });
  await settings(page); await openChecks(page); await expect(page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true })).toBeEnabled();
  await page.getByLabel('扩展缓存对照', { exact: false }).check(); await page.getByLabel('大型上下文', { exact: false }).check();
  await expect(page.getByRole('button', { name: '开始检查 · 22 次请求', exact: true })).toBeEnabled();
  await expect(page.getByText('大型上下文已启用，费用将显著提高。', { exact: false })).toBeVisible();
  await page.getByLabel('Do not spawn subagents.', { exact: false }).check();
  await expect(page.getByRole('button', { name: '开始检查 · 24 次请求', exact: true })).toBeEnabled();
  await expect(page.getByText('Your final answer must summarize the result, evidence, changed files and remaining limitations for your parent.', { exact: true })).toBeVisible();
  await page.getByText('可选费率 · 未配置费率', { exact: true }).click(); await page.getByLabel('未缓存输入 / 百万 token', { exact: true }).fill('0'); await page.getByLabel('输出 / 百万 token', { exact: true }).fill('3.5');
  await expect.poll(() => plans.at(-1)?.pricing).toEqual({ currency: 'CNY', input_per_million: 0, cached_input_per_million: null, output_per_million: 3.5 });
  expect(plans.at(-1).context_size).toBe('large'); expect(plans.at(-1).extended_cache).toBe(true); expect(posts).toBe(0);
});

test('quality checks require saved config and retry only failed local planning and progress reads', async ({ page }) => {
  let planning = 0; let starting = 0; let saveCount = 0; let listCount = 0; const startIds: string[] = [];
  await page.route('**/api/config', route => { if (route.request().method() === 'PUT') saveCount++; return route.fulfill({ json: config }); });
  await page.route('**/api/model-checks**', route => {
    const url = new URL(route.request().url());
    if (url.pathname.endsWith('/plan')) { planning++; if (planning === 1) return route.fulfill({ status: 503, json: { error: 'Plan temporarily unavailable' } }); return route.fulfill({ json: plan(route.request().postDataJSON()) }); }
    if (route.request().method() === 'POST') { starting++; startIds.push(route.request().postDataJSON().client_request_id); return route.fulfill({ status: 503, json: { error: 'Run not accepted' } }); }
    listCount++; return route.fulfill({ json: [] });
  });
  await settings(page); await page.getByLabel('显示名称', { exact: true }).fill('Unsaved model name'); await page.getByRole('button', { name: '模型质量检查', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('请先保存模型配置'); expect(planning).toBe(0); expect(saveCount).toBe(0); expect(starting).toBe(0);
  await page.getByLabel('显示名称', { exact: true }).fill(model.name); await openChecks(page);
  await expect(page.getByRole('alert')).toContainText('Plan temporarily unavailable'); await page.getByRole('button', { name: '重试计划', exact: true }).click();
  await expect(page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Run not accepted'); expect(starting).toBe(1); await expect.poll(() => listCount).toBeGreaterThan(1); expect(starting).toBe(1);
  await expect(page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true }).click(); await expect.poll(() => startIds.length).toBe(2);
  expect(startIds[0]).toMatch(/^[0-9a-f-]{36}$/i); expect(startIds[1]).toBe(startIds[0]);
});

test('quality checks exact selection isolation and unmount clean polling without restarting paid runs', async ({ page }) => {
  const queries: any[] = []; let posts = 0; let reads = 0;
  await page.route('**/api/model-checks**', route => {
    const url = new URL(route.request().url());
    if (url.pathname.endsWith('/plan')) return route.fulfill({ json: plan(route.request().postDataJSON()) });
    if (route.request().method() === 'POST') { posts++; return route.fulfill({ status: 202, json: run() }); }
    if (url.pathname === '/api/model-checks') { const selected = Object.fromEntries(url.searchParams); queries.push(selected); return route.fulfill({ json: [{ ...run(), selection: otherSelection, id: 'other-record', status: 'completed' }] }); }
    reads++; if (!failedPoll) { failedPoll = true; return route.fulfill({ status: 503, json: { error: 'Progress temporarily unavailable' } }); } return route.fulfill({ json: run() });
  });
  let failedPoll = false;
  await settings(page); await page.getByRole('button', { name: 'Other model', exact: true }).click(); await openChecks(page);
  await expect.poll(() => queries.length).toBe(1); expect(queries[0]).toEqual(otherSelection); expect(posts).toBe(0); expect(reads).toBe(0);
  await page.getByRole('button', { name: '返回模型设置', exact: true }).click(); await page.getByRole('button', { name: model.name, exact: true }).click(); await openChecks(page);
  await expect(page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true })).toBeEnabled(); await expect(page.getByLabel('查看检查')).toHaveCount(0);
  await page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true }).click(); await expect.poll(() => reads).toBeGreaterThan(0);
  await expect(page.getByRole('alert')).toContainText('Progress temporarily unavailable'); await page.getByRole('button', { name: '重试进度读取', exact: true }).click(); await expect.poll(() => reads).toBeGreaterThan(1); expect(posts).toBe(1);
  await page.getByRole('button', { name: '返回模型设置', exact: true }).click(); const before = reads;
  await expect(page.getByRole('dialog', { name: '模型质量检查', exact: true })).toHaveCount(0); await page.getByLabel('显示名称', { exact: true }).fill('Just a local draft');
  expect(posts).toBe(1); expect(reads).toBe(before);
});


const networkInput = { ...defaultInput, groups: ['network'], network_trace: false };
async function networkOnly(page: Page) {
  await page.getByLabel('网络连通与延迟', { exact: false }).check();
  for (const label of ['路由与指令对照', '缓存复用', 'Responses 协议与工具参数']) await page.getByLabel(label, { exact: false }).uncheck();
  await expect(page.getByRole('button', { name: '开始检查 · 4 次请求', exact: true })).toBeEnabled();
}
function networkResult(kind = 'http_head', extra: any = {}) {
  return { id: `network-${kind}`, label: `网络 ${kind}`, group: 'network', variant: kind, repeat: 1, status: 'ok', observation: { protocol: 'network', http_status: kind === 'route_trace' ? null : 405, header_latency_ms: kind === 'route_trace' ? null : 12, first_token_ms: null, total_ms: 16, usage: null, response_model: null, network: { kind, target: { host: 'fixture.example.test', port: 443, scheme: 'https' }, path: 'direct', policy: 'fixture direct policy', http: { method: 'HEAD', status: 405, version: 'HTTP/2', headers: { server: 'cloudflare', 'cf-ray': 'fixture-ray', location: 'https://fixture-user:fixture-password@redirect.example.test/v1?secret=fixture-query#fixture-fragment', 'set-cookie': 'fixture-private-cookie' }, redirect_location: 'https://fixture-user:fixture-password@redirect.example.test/v1?secret=fixture-query#fixture-fragment', redirect_followed: false }, cdn: { candidates: [{ name: 'Cloudflare', evidence: ['server: cloudflare', 'cf-ray: fixture-ray'] }], evidence_only: true, notes: ['Fixture header brand is only a clue.'] }, ...extra } }, analysis: { model_match: false, answer_correct: false }, cost: { estimated_cost: 0, complete: true } };
}
function networkRun(input: any = networkInput, status = 'completed', results: any[] = [networkResult()]) {
  return { ...run(input, status), completed_probes: results.length, results, summary: { totals: { requests_completed: results.length, provider_requests_completed: 0, network_tasks_completed: results.length, input_tokens: 0, cached_tokens: 0, output_tokens: 0, usage_complete: true, estimated_cost: 0 }, network: { samples: results.length, successful_http_samples: 3, head_ms: { min: 10, max: 30, median: 20, mean: 20, stddev: 8.2 }, cdn_candidates: [{ name: 'Cloudflare', evidence: ['server: cloudflare'] }], route_status: null } } };
}

test('network is default off; network-only preview costs no model request and trace requires explicit selection', async ({ page }) => {
  const plans: any[] = []; let posts = 0;
  await page.route('**/api/model-checks**', route => {
    if (new URL(route.request().url()).pathname.endsWith('/plan')) { const input = route.request().postDataJSON(); plans.push(input); return route.fulfill({ json: plan(input) }); }
    if (route.request().method() === 'POST') posts++;
    return route.fulfill({ json: [] });
  });
  await settings(page); await openChecks(page);
  await expect(page.getByRole('button', { name: '开始检查 · 14 次请求', exact: true })).toBeEnabled();
  await expect(page.getByLabel('网络连通与延迟', { exact: false })).not.toBeChecked();
  const trace = page.getByLabel('路由跳数（直接路径', { exact: false });
  await expect(trace).not.toBeChecked(); await expect(trace).toBeDisabled();
  expect(plans[0].network_trace).toBe(false); expect(plans[0].groups).toEqual(defaultInput.groups);
  await networkOnly(page);
  await expect(page.getByLabel('检查计划')).toContainText('模型请求 0 次 · 网络任务 4 项 · 网络 HTTP 最多 4 次');
  await expect(page.getByText('本计划 0 次模型请求、0 模型费用；点击开始才执行网络探测。', { exact: true })).toBeVisible();
  expect(plans.at(-1).groups).toEqual(['network']); expect(plans.at(-1).network_trace).toBe(false);
  await trace.check(); await expect(page.getByRole('button', { name: '开始检查 · 5 次请求', exact: true })).toBeEnabled();
  expect(plans.at(-1).network_trace).toBe(true);
  await expect(page.getByText('增加 1 项路由跳数探测', { exact: false })).toBeVisible();
  await page.getByLabel('网络连通与延迟', { exact: false }).uncheck();
  await expect(trace).not.toBeChecked(); await expect(trace).toBeDisabled();
  await page.getByLabel('网络连通与延迟', { exact: false }).check();
  await expect(page.getByRole('button', { name: '开始检查 · 4 次请求', exact: true })).toBeEnabled();
  expect(plans.at(-1).network_trace).toBe(false); expect(posts).toBe(0);
});

test('network-only explicit start shows actual HEAD summary, CDN evidence, safe redirect and n/a model data', async ({ page }, testInfo) => {
  let posts = 0; let posted: any; const outbound: string[] = [];
  page.on('request', request => { if (new URL(request.url()).origin !== 'http://localhost:48991') outbound.push(request.url()); });
  const result = networkResult('connection', { dns: { status: 'ok', elapsed_ms: 1, addresses: ['192.0.2.8', '2001:db8::8'], notes: ['Local fixture resolver.'] }, connection: { status: 'ok', dns_ms: 1, tcp_ms: 3, tls_ms: 5, http_header_ms: 12, remote_ip: '192.0.2.8', http_version: 'HTTP/2', tls: { verified: true, subject: 'fixture.example.test', issuer: 'Fixture CA', not_after: '2027-01-01' }, notes: [] } });
  // Even malformed model-only fields in a network observation must not produce model warnings or TTFT.
  result.observation.first_token_ms = 9876 as any;
  await page.route('**/api/model-checks**', route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith('/plan')) return route.fulfill({ json: plan(route.request().postDataJSON()) });
    if (route.request().method() === 'POST') { posts++; posted = route.request().postDataJSON(); return route.fulfill({ status: 202, json: networkRun(posted, 'completed', [result]) }); }
    return route.fulfill({ json: [] });
  });
  await settings(page); await openChecks(page); await networkOnly(page); expect(posts).toBe(0);
  await page.getByRole('button', { name: '开始检查 · 4 次请求', exact: true }).click();
  await expect(page.getByLabel('网络观测汇总')).toContainText('HEAD 中位数 20 ms');
  await expect(page.getByLabel('网络观测汇总')).toContainText('HEAD 标准差 8.2 ms');
  await expect(page.getByLabel('网络观测汇总')).toContainText('HTTP 响应样本 3');
  const row = page.locator('.quality-network-row');
  await expect(row).toContainText('网络探测'); await expect(row).toContainText('0 费用');
  await expect(row).toContainText('HTTP 405 已响应');
  await expect(row).not.toContainText('自报不一致'); await expect(row).not.toContainText('9876'); await expect(row).not.toContainText('未返回 usage');
  await row.getByText('网络 / CDN / 路由详情', { exact: true }).click();
  await expect(row).toContainText('192.0.2.8'); await expect(row).toContainText('2001:db8::8');
  await expect(row).toContainText('Cloudflare'); await expect(row).toContainText('server: cloudflare');
  await expect(row).toContainText('基于 HTTP 响应头识别'); await expect(row).toContainText('https://redirect.example.test/v1');
  await expect(row).not.toContainText('fixture-password'); await expect(row).not.toContainText('fixture-query'); await expect(row).not.toContainText('fixture-fragment');
  await row.getByText('HTTP 响应头', { exact: true }).click();
  await expect(row).not.toContainText('fixture-private-cookie'); await expect(row).not.toContainText('fixture-password'); await expect(row).not.toContainText('fixture-query');
  await expect(row.getByRole('link')).toHaveCount(0);
  await row.getByText('TLS / 证书实际观测', { exact: true }).click(); await expect(row).toContainText('Fixture CA');
  expect(posts).toBe(1); expect(posted.groups).toEqual(['network']); expect(posted.network_trace).toBe(false);
  await row.scrollIntoViewIfNeeded();
  await page.screenshot({ path: testInfo.outputPath('network-diagnostics-table.png'), fullPage: true });
  expect(outbound).toEqual([]); expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
});

test('network trace partial hops remain unknown total; stars and missing stages never become fabricated data', async ({ page }) => {
  const input = { ...networkInput, network_trace: true };
  const result = networkResult('route_trace', { http: null, cdn: null, route: { status: 'partial', tool: 'traceroute', reached_target: false, hop_count: 12, max_hops: 12, observed_hops: 2, hops: [{ ttl: 1, ip: '192.0.2.1', rtt_ms: 2, status: 'ok' }, { ttl: 2, ip: null, rtt_ms: null, status: '*' }], notes: ['ICMP fixture limited; target was not reached.'] } });
  await page.route('**/api/model-checks**', route => new URL(route.request().url()).pathname.endsWith('/plan') ? route.fulfill({ json: plan(route.request().postDataJSON()) }) : route.fulfill({ json: [networkRun(input, 'completed', [result])] }));
  await settings(page); await openChecks(page);
  const row = page.locator('.quality-network-row'); await expect(row).toContainText('总跳数未知');
  await row.getByText('网络 / CDN / 路由详情', { exact: true }).click();
  await expect(row).toContainText('总跳数 未知'); await expect(row).toContainText('部分可见'); await expect(row).toContainText('ICMP fixture limited');
  const hops = page.getByRole('table', { name: /实际观察到的跳点/ });
  await expect(hops).toContainText('192.0.2.1'); await expect(hops).toContainText('2 ms'); await expect(hops).toContainText('*'); await expect(hops).toContainText('未响应');
  await expect(hops.locator('tbody tr').last().locator('td').nth(2)).toHaveText('*');
  await expect(hops.locator('tbody tr').first().locator('td').nth(2)).toHaveText('2 ms');
});

test('network proxy policy skips and unavailable tools are readable without model failure claims', async ({ page }) => {
  const proxy = networkResult('connection', { path: 'configured_proxy', dns: { status: 'skipped', addresses: [], notes: ['Proxy policy: direct origin DNS skipped.'] }, connection: { status: 'skipped', dns_ms: null, tcp_ms: null, tls_ms: null, notes: ['Stage measurements unavailable inside proxy tunnel.'] }, route: { status: 'skipped', reached_target: false, hop_count: null, hops: [], notes: ['Route skipped; never bypass proxy.'] } });
  const unavailable = { ...networkResult('route_trace', { http: null, cdn: null, route: { status: 'unavailable', reached_target: false, hop_count: null, hops: [], notes: ['traceroute fixture binary unavailable'] } }), status: 'error' };
  const missing = { ...networkResult('missing'), observation: { protocol: 'network' } };
  await page.route('**/api/model-checks**', route => new URL(route.request().url()).pathname.endsWith('/plan') ? route.fulfill({ json: plan(route.request().postDataJSON()) }) : route.fulfill({ json: [networkRun(networkInput, 'completed', [proxy, unavailable, missing])] }));
  await settings(page); await openChecks(page);
  const rows = page.locator('.quality-network-row'); await expect(rows).toHaveCount(3);
  await rows.nth(0).getByText('网络 / CDN / 路由详情', { exact: true }).click();
  await expect(rows.nth(0)).toContainText('经由已配置的代理路径'); await expect(rows.nth(0)).toContainText('direct origin DNS skipped');
  await expect(rows.nth(0)).toContainText('Stage measurements unavailable'); await expect(rows.nth(0)).toContainText('DNS 未返回');
  await expect(rows.nth(1)).toContainText('当前环境无法检查');
  await rows.nth(1).getByText('网络 / CDN / 路由详情', { exact: true }).click(); await expect(rows.nth(1)).toContainText('traceroute fixture binary unavailable');
  await rows.nth(2).getByText('网络 / CDN / 路由详情', { exact: true }).click(); await expect(rows.nth(2)).toContainText('未返回网络观测。');
  await expect(page.locator('.quality-table')).not.toContainText('自报不一致'); await expect(page.locator('.quality-table')).not.toContainText('答案未匹配');
});

test('network-only cancel persists partial data and export without rerunning probes', async ({ page }) => {
  let current: any = null; let posts = 0; let cancels = 0; let exports = 0;
  await page.route('**/api/model-checks**', route => {
    const path = new URL(route.request().url()).pathname;
    if (path.endsWith('/plan')) return route.fulfill({ json: plan(route.request().postDataJSON()) });
    if (path.endsWith('/cancel')) { cancels++; current = { ...current, status: 'cancelled', updated_at: '2026-11-16T12:00:03Z' }; return route.fulfill({ json: current }); }
    if (path.endsWith('/export')) { exports++; return route.fulfill({ json: current }); }
    if (path === '/api/model-checks' && route.request().method() === 'POST') { posts++; current = networkRun(route.request().postDataJSON(), 'running', [networkResult()]); return route.fulfill({ status: 202, json: current }); }
    return route.fulfill({ json: path === '/api/model-checks' ? current ? [current] : [] : current });
  });
  await settings(page); await openChecks(page); await networkOnly(page);
  await page.getByRole('button', { name: '开始检查 · 4 次请求', exact: true }).click();
  await page.getByRole('button', { name: '取消本次检查', exact: true }).click();
  await expect(page.getByText('已取消 · 1 / 4 次', { exact: true })).toBeVisible();
  const pendingDownload = page.waitForEvent('download'); await page.getByRole('button', { name: '导出 JSON', exact: true }).click();
  const download = await pendingDownload; const exported = JSON.parse(await readFile((await download.path())!, 'utf8'));
  expect(exported.status).toBe('cancelled'); expect(exported.results[0].observation.network.kind).toBe('http_head'); expect(exported.results[0].observation.usage).toBeNull();
  await page.getByRole('button', { name: '返回模型设置', exact: true }).click(); await openChecks(page);
  await expect(page.getByText('已取消 · 1 / 4 次', { exact: true })).toBeVisible(); expect(posts).toBe(1); expect(cancels).toBe(1); expect(exports).toBe(1);
});

test('quality plan enforces forty tasks and preserves old maximum plus five network tasks', async ({ page }) => {
  let excessive = false; let posts = 0;
  await page.route('**/api/model-checks**', route => {
    if (new URL(route.request().url()).pathname.endsWith('/plan')) { const next = plan(route.request().postDataJSON()); return route.fulfill({ json: excessive ? { ...next, request_count: 41 } : next }); }
    if (route.request().method() === 'POST') posts++;
    return route.fulfill({ json: [] });
  });
  await settings(page); await openChecks(page);
  for (const checkbox of await page.locator('.quality-phrase input').all()) await checkbox.check();
  await page.getByLabel('扩展缓存对照', { exact: false }).check();
  await page.getByLabel('网络连通与延迟', { exact: false }).check(); await page.getByLabel('路由跳数（直接路径', { exact: false }).check();
  await expect(page.getByRole('button', { name: '开始检查 · 39 次请求', exact: true })).toBeEnabled();
  await expect(page.getByLabel('检查计划')).toContainText('模型请求 34 次 · 网络任务 5 项');
  excessive = true; await page.getByLabel('路由跳数（直接路径', { exact: false }).uncheck();
  await expect(page.getByRole('alert')).toContainText('超过 40 项任务上限');
  await expect(page.getByRole('button', { name: '开始检查 · 41 次请求', exact: true })).toBeDisabled(); expect(posts).toBe(0);
});
