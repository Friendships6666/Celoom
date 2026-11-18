import { afterEach, describe, expect, it, vi } from 'vitest';
import { api, ApiError, computerImageUrl, filePreviewUrl, request, sanitizeConfig } from '../src/api';
import type { AppConfig } from '../src/types';

afterEach(() => vi.unstubAllGlobals());
describe('same-origin API contract', () => {
  it('sends cookies and mutation protection, including authentication', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ ok: true }), { status: 200 }));
    vi.stubGlobal('fetch', fetch);
    await api.auth('login', 'test-fixture-not-a-real-password');
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe('/api/login');
    expect(init.credentials).toBe('same-origin');
    expect(init.headers['X-JustAI-Request']).toBe('1');
    expect(init.method).toBe('POST');
  });
  it('preserves the client_message_id and strips no image data prefixes implicitly', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify({ run_id: 'run' })));
    vi.stubGlobal('fetch', fetch);
    const payload = { text: 'hello', images: [{ mime: 'image/png', data: 'ZmFrZQ==' }], client_message_id: 'request-id' };
    await api.message('session/id', payload);
    expect(fetch.mock.calls[0][0]).toBe('/api/sessions/session%2Fid/messages');
    expect(JSON.parse(fetch.mock.calls[0][1].body)).toEqual(payload);
  });
  it('keeps revision conflicts typed and never renders raw non-JSON errors', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce(new Response(JSON.stringify({ error: 'revision conflict', code: 'revision_conflict' }), { status: 409 })).mockResolvedValueOnce(new Response('<html>untrusted</html>', { status: 502 })));
    await expect(request('/config', 'PUT', {})).rejects.toMatchObject({ status: 409, code: 'revision_conflict' });
    await expect(request('/config')).rejects.toEqual(new ApiError('请求失败（HTTP 502）', 502));
  });
  it('patches session routing and distinguishes default, omitted and explicit effort', async () => {
    const fetch = vi.fn().mockImplementation(() => Promise.resolve(new Response(JSON.stringify({ id: 's' }))));
    vi.stubGlobal('fetch', fetch);
    const selection = { site_id: 'site', key_id: 'key', model_id: 'model' };
    for (const reasoning_effort of [null, '', 'high']) await api.sessionSettings('session/id', { selection, reasoning_effort });
    expect(fetch.mock.calls.map(call => JSON.parse(call[1].body).reasoning_effort)).toEqual([null, '', 'high']);
    expect(fetch.mock.calls[0][0]).toBe('/api/sessions/session%2Fid/settings');
    expect(fetch.mock.calls[0][1]).toMatchObject({ method: 'PATCH', credentials: 'same-origin', headers: { 'X-JustAI-Request': '1' } });
  });
  it('encodes authenticated root-relative file listing and preview URLs', async () => {
    const fetch = vi.fn().mockImplementation(() => Promise.resolve(new Response(JSON.stringify({ path: '', entries: [] }))));
    vi.stubGlobal('fetch', fetch);
    await api.files('s/id'); await api.files('s/id', 'src/a & b');
    expect(fetch.mock.calls[0][0]).toBe('/api/sessions/s%2Fid/files?path=');
    expect(fetch.mock.calls[1][0]).toBe('/api/sessions/s%2Fid/files?path=src%2Fa%20%26%20b');
    expect(fetch.mock.calls[1][1].credentials).toBe('same-origin');
    expect(filePreviewUrl('s/id', 'a/preview #1.svg')).toBe('/api/sessions/s%2Fid/file?path=a%2Fpreview%20%231.svg');
  });
  it('reads computer status without launching and guards global stop/host mutations', async () => {
    const fetch = vi.fn().mockImplementation(() => Promise.resolve(new Response(JSON.stringify({ ok: true }))));
    vi.stubGlobal('fetch', fetch);
    await api.computer(); await api.stopComputer(); await api.computerHost(true); await api.computerHost(false);
    expect(fetch.mock.calls.map(call => call[0])).toEqual(['/api/computer', '/api/computer/stop', '/api/computer/host', '/api/computer/host']);
    expect(fetch.mock.calls[0][1]).toMatchObject({ method: 'GET', credentials: 'same-origin', body: undefined });
    for (const call of fetch.mock.calls.slice(1)) expect(call[1]).toMatchObject({ method: 'POST', credentials: 'same-origin', headers: { 'X-JustAI-Request': '1' } });
    expect(fetch.mock.calls.slice(2).map(call => JSON.parse(call[1].body))).toEqual([{ enabled: true }, { enabled: false }]);
    expect(computerImageUrl('s/id', 'capture #1')).toBe('/api/sessions/s%2Fid/computer/image/capture%20%231');
  });
  it('defaults missing search config and strips search credentials without losing other config', () => {
    const config = { revision: 9, network: {}, sites: [], extra: { keep: true } } as unknown as AppConfig;
    expect(sanitizeConfig(config)).toMatchObject({ revision: 9, search: { provider: 'duckduckgo', base_url: '' }, extra: { keep: true } });
    expect(config.search).toBeUndefined();
  });
  it('never repopulates a search key returned by an older backend', () => {
    const config = { revision: 9, network: {}, sites: [], search: { provider: 'brave', base_url: 'https://search.example.test', api_key: 'search-key-fixture', has_api_key: true } } as AppConfig;
    expect(sanitizeConfig(config).search).toEqual({ provider: 'brave', base_url: 'https://search.example.test', has_api_key: true });
    expect(config.search?.api_key).toBe('search-key-fixture');
  });
  it('loads autonomy and submits exact answer/create/action contracts with encoded ids', async () => {
    const fetch = vi.fn().mockImplementation(() => Promise.resolve(new Response(JSON.stringify({ id: 'record' }))));
    vi.stubGlobal('fetch', fetch);
    await api.questions('s/id'); await api.goals('s/id');
    await api.answerQuestion('s/id', 'q/id', 'Selected answer');
    await api.createGoal('s/id', { title: 'Ship', objective: 'Verify output' });
    for (const action of ['start', 'pause', 'resume', 'cancel'] as const) await api.changeGoal('s/id', 'g/id', action);
    expect(fetch.mock.calls.slice(0, 2).map(call => call[0])).toEqual(['/api/sessions/s%2Fid/questions', '/api/sessions/s%2Fid/goals']);
    expect(fetch.mock.calls[2][0]).toBe('/api/sessions/s%2Fid/questions/q%2Fid/answer');
    expect(JSON.parse(fetch.mock.calls[2][1].body)).toEqual({ answer: 'Selected answer' });
    expect(JSON.parse(fetch.mock.calls[3][1].body)).toEqual({ title: 'Ship', objective: 'Verify output' });
    for (const call of fetch.mock.calls.slice(4)) {
      expect(call[0]).toBe('/api/sessions/s%2Fid/goals/g%2Fid');
      expect(call[1]).toMatchObject({ method: 'PATCH', headers: { 'X-JustAI-Request': '1' } });
    }
    expect(fetch.mock.calls.slice(4).map(call => JSON.parse(call[1].body))).toEqual([{ action: 'start' }, { action: 'pause' }, { action: 'resume' }, { action: 'cancel' }]);
  });
  it('defaults missing per-model omit_tool_strict to true without mutating legacy config', () => {
    const config = { revision: 8, network: {}, sites: [{ id: 'site', name: 'Site', base_url: 'https://example.test/v1', keys: [{ id: 'key', name: 'Key', has_secret: true, models: [{ id: 'legacy', name: 'Legacy', model: 'fixture-model', strict_tools: true }] }] }] } as unknown as AppConfig;
    const result = sanitizeConfig(config);
    expect(result.sites[0].keys[0].models[0]).toMatchObject({ omit_tool_strict: true, strict_tools: true });
    expect(config.sites[0].keys[0].models[0].omit_tool_strict).toBeUndefined();
  });
  it.each([true, false])('round trips explicit omit_tool_strict=%s independently of strict_tools and strips returned credentials', async omit_tool_strict => {
    const config = { revision: 8, network: {}, sites: [{ id: 'site', name: 'Site', base_url: 'https://example.test/v1', keys: [{ id: 'key', name: 'Key', secret: 'returned-model-key-fixture', has_secret: true, models: [{ id: 'selected', name: 'Selected', model: 'selected-fixture', strict_tools: true, omit_tool_strict }, { id: 'other', name: 'Other', model: 'other-fixture', strict_tools: false, omit_tool_strict: !omit_tool_strict }] }] }] } as unknown as AppConfig;
    const fetch = vi.fn().mockImplementation((_url, init) => Promise.resolve(new Response(JSON.stringify(init.method === 'PUT' ? { ...JSON.parse(init.body), revision: 9 } : config))));
    vi.stubGlobal('fetch', fetch);
    const normalized = sanitizeConfig(await api.config());
    const saved = sanitizeConfig(await api.saveConfig(normalized));
    const sent = JSON.parse(fetch.mock.calls[1][1].body);
    expect(sent.sites[0].keys[0].models.map((model: { omit_tool_strict: boolean }) => model.omit_tool_strict)).toEqual([omit_tool_strict, !omit_tool_strict]);
    expect(sent.sites[0].keys[0].models.map((model: { strict_tools: boolean }) => model.strict_tools)).toEqual([true, false]);
    expect(saved.sites[0].keys[0].models).toEqual(normalized.sites[0].keys[0].models);
    expect(fetch.mock.calls[1][1]).toMatchObject({ method: 'PUT', credentials: 'same-origin', headers: { 'X-JustAI-Request': '1' } });
    expect(JSON.stringify(sent)).not.toContain('returned-model-key-fixture');
    expect(config.sites[0].keys[0].secret).toBe('returned-model-key-fixture');
  });
  it('scopes quality records and guarded paid start by exact selection and stable client request ID', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response('{}', { status: 200 })); vi.stubGlobal('fetch', fetch);
    const selection = { site_id: 'site/one', key_id: 'key+two', model_id: 'model three' };
    const controller = new AbortController();
    await api.qualityRuns(selection, controller.signal);
    const query = new URL(String(fetch.mock.calls[0][0]), 'https://fixture.invalid').searchParams;
    expect(Object.fromEntries(query)).toEqual(selection); expect(fetch.mock.calls[0][1]).toMatchObject({ method: 'GET', credentials: 'same-origin', signal: controller.signal });
    const input = { selection, groups: ['cache'] as ['cache'], instruction_probes: [] as [], context_size: 'small' as const, extended_cache: false, pricing: null, client_request_id: '4ca04012-5ce5-4b99-a842-e4bac75a1b12' };
    fetch.mockResolvedValue(new Response('{}', { status: 200 })); await api.startQuality(input);
    expect(fetch.mock.calls[1][0]).toBe('/api/model-checks'); expect(JSON.parse(fetch.mock.calls[1][1].body)).toEqual(input);
    expect(fetch.mock.calls[1][1]).toMatchObject({ method: 'POST', credentials: 'same-origin', headers: { 'X-JustAI-Request': '1' } });
    fetch.mockResolvedValue(new Response('{}', { status: 200 })); await api.exportQuality('run/one');
    expect(fetch.mock.calls[2][0]).toBe('/api/model-checks/run%2Fone/export'); expect(fetch.mock.calls[2][1]).toMatchObject({ method: 'GET', credentials: 'same-origin' });
  });
  it('discards any secret mistakenly returned by a backend', () => {
    const config = { revision: 7, network: {}, sites: [{ id: 's', name: 'Site', base_url: 'https://example.test/v1', keys: [{ id: 'k', name: 'Key', secret: 'not-a-real-key-fixture', has_secret: true, models: [] }] }] } as AppConfig;
    expect(sanitizeConfig(config).sites[0].keys[0]).toEqual({ id: 'k', name: 'Key', has_secret: true, models: [] });
    expect(config.sites[0].keys[0].secret).toBe('not-a-real-key-fixture');
  });
});
