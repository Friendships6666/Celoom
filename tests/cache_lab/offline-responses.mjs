#!/usr/bin/env node
/** Offline, synthetic-only request serialization laboratory. No server and no real fetch. */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createRequire, syncBuiltinESMExports } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import net from 'node:net';
import http from 'node:http';
import https from 'node:https';
import http2 from 'node:http2';
import dgram from 'node:dgram';
import dns from 'node:dns';
import dnsPromises from 'node:dns/promises';

const ROOT = '/usr/lib/node_modules/@deepseek-ai/dsh';
const HERE = dirname(fileURLToPath(import.meta.url));
const OUT = join(HERE, 'fixtures');
const networkAttempts = [];
function denyNetwork(...args) {
  networkAttempts.push({ transport: 'socket/http', argumentType: typeof args[0] });
  throw new Error('CACHE_LAB: all real network disabled, including localhost');
}
// Fail closed even if a future adapter bypasses supplied/global fetch.
net.Socket.prototype.connect = denyNetwork;
http.request = http.get = https.request = https.get = denyNetwork;
http2.connect = denyNetwork;
dgram.Socket.prototype.send = dgram.Socket.prototype.connect = denyNetwork;
for (const api of [dns, dnsPromises]) {
  for (const key of Object.keys(api)) if (key === 'lookup' || key === 'lookupService' || key.startsWith('resolve')) api[key] = denyNetwork;
}
syncBuiltinESMExports();

let activeCapture;
const captures = [];
const syntheticReplyItems = [
  { type: 'reasoning', id: 'rs_sse_fixture', summary: [{ type: 'summary_text', text: 'Synthetic reasoning summary; not a real model output.' }] },
  { type: 'message', id: 'msg_sse_fixture', role: 'assistant', phase: 'commentary', status: 'completed', content: [{ type: 'output_text', text: 'Synthetic replay check.', annotations: [] }] },
  { type: 'function_call', id: 'fc_sse_fixture', call_id: 'call_sse_fixture', name: 'lab_lookup', arguments: '{\n  "index" : 99, "label" : "sse fixture"\n}' },
];
const terminalReplyItems = structuredClone(syntheticReplyItems);
terminalReplyItems[0].encrypted_content = 'SYNTHETIC_OPAQUE_REASONING_NOT_VALID_ON_ANY_SERVER';
function makeSSE() {
  const events = [{ type: 'response.created', response: { id: 'resp_synthetic_fixture' } }];
  for (const [output_index, item] of syntheticReplyItems.entries()) {
    events.push({ type: 'response.output_item.added', output_index, item });
    events.push({ type: 'response.output_item.done', output_index, item });
  }
  events.push({ type: 'response.completed', response: {
    id: 'resp_synthetic_fixture', status: 'completed', model: 'synthetic-response-model',
    output: terminalReplyItems,
    usage: { input_tokens: 0, output_tokens: 0, total_tokens: 0, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 } },
  } });
  return events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('') + 'data: [DONE]\n\n';
}
async function mockFetch(input, init = {}) {
  const url = new URL(typeof input === 'string' || input instanceof URL ? input : input.url);
  assert.ok(['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname), `non-local URL blocked: ${url.origin}`);
  assert.equal(url.protocol, 'http:');
  assert.ok(activeCapture, 'unexpected fetch outside a capture');
  const rawBody = init.body ?? (input instanceof Request ? await input.text() : undefined);
  assert.equal(typeof rawBody, 'string', 'capture the actual SDK JSON string');
  const entry = { label: activeCapture, url: url.href, method: init.method ?? input.method ?? 'GET', rawBody, body: JSON.parse(rawBody) };
  // Record names and a strict static-value allowlist only; never retain auth/cookie values.
  const headers = new Headers(init.headers ?? (input instanceof Request ? input.headers : undefined));
  const staticHeader = /^(accept|content-type|user-agent|originator|openai-beta|openai-organization|openai-project|x-stainless-(lang|package-version|os|arch|runtime|runtime-version|retry-count|timeout)|http-referer|x-openrouter-title|x-openrouter-categories|x-opencode-client|x-billing-invoke-origin)$/;
  entry.headers = Object.fromEntries([...headers].map(([name, value]) => [name,
    /authorization|cookie|token|api[-_]?key/i.test(name) ? '<present; value never saved>' :
      staticHeader.test(name) ? value : '<present; not on static-value allowlist>']));
  captures.push(entry);
  return new Response(makeSSE(), { status: 200, headers: { 'content-type': 'text/event-stream', 'x-request-id': 'req_synthetic_fixture' } });
}
globalThis.fetch = mockFetch; // Original fetch is neither stored nor called.

const requireDSH = createRequire(join(ROOT, 'package.json'));
// pi-ai exposes import-only entry points, so CommonJS require.resolve('.') is unavailable.
const piEntry = join(ROOT, 'node_modules/@earendil-works/pi-ai/dist/index.js');
const PI = dirname(piEntry);
const adapterPath = requireDSH.resolve('@deepseek-ai/dsh-llm-pi-ai');
const sdkEntry = createRequire(piEntry).resolve('openai');
const { PiAiAdapter } = await import(pathToFileURL(adapterPath));
const { markAgentLoopRequest } = await import(pathToFileURL(requireDSH.resolve('@deepseek-ai/dsh-llm')));
const { createModels } = await import(pathToFileURL(join(PI, 'models.js')));
const responses = await import(pathToFileURL(join(PI, 'api/openai-responses.js')));
const { convertResponsesMessages, convertResponsesTools } = await import(pathToFileURL(join(PI, 'api/openai-responses-shared.js')));
const { default: OpenAI } = await import(pathToFileURL(sdkEntry));
const plain = value => JSON.parse(JSON.stringify(value));
const hash = text => createHash('sha256').update(text).digest('hex');
const usage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
const tools = [
  { name: 'lab_lookup', description: 'Synthetic lookup. Never executed.', parameters: { type: 'object', properties: { index: { type: 'integer' }, label: { type: 'string' } }, required: ['index'] } },
  { name: 'lab_empty', description: 'Synthetic no-argument operation. Never executed.', parameters: { type: 'object', properties: {} } },
];
function modelConfig(id, overrides = {}) {
  return { id, name: `Synthetic ${id}`, api: 'openai-responses', provider: 'openai-responses', baseUrl: 'http://127.0.0.1:1/v1', reasoning: true,
    input: ['text'], cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 262144, maxTokens: 4096, ...overrides };
}
function replayEnvelope(model, blocks, overrides = {}) {
  return { response: { kind: 'pi-ai', version: 2, api: model.api, provider: model.provider, model: model.id, stopReason: 'toolUse', ...overrides }, blocks };
}
function syntheticRequest(model) {
  const messages = [
    { role: 'system', content: [{ type: 'text', text: 'Synthetic leading history system A.' }, { type: 'text', text: 'Synthetic leading history system B.' }] },
    { role: 'user', content: [{ type: 'text', text: 'Static serialization fixture only; no task or external content.' }] },
    { role: 'user', content: [] },
  ];
  for (let round = 0; round < 8; round++) {
    const reasoning = { type: 'reasoning', id: `rs_round_${round}`, summary: [{ type: 'summary_text', text: `Synthetic summary ${round}.` }], encrypted_content: `SYNTHETIC_OPAQUE_${round}` };
    const content = [{ type: 'reasoning', text: `Visible synthetic reasoning ${round}; signature is authoritative for native replay.` }];
    const meta = [{ type: 'reasoning', thinkingSignature: JSON.stringify(reasoning) }];
    for (let index = 0; index < 6; index++) {
      content.push({ type: 'tool-call', id: `call_${round}_${index}|fc_round_${round}_${index}`, name: index % 2 ? 'lab_empty' : 'lab_lookup',
        arguments: index % 2 ? '{ \n }' : `{\n  "index" : ${round * 6 + index},\n  "label" : "synthetic-${round}-${index}"\n}` });
      meta.push({ type: 'tool-call' });
      if (index === 2) {
        content.push({ type: 'text', text: round === 0 ? '' : `Synthetic interleaved text ${round}. ` + 'fixed-token-fixture '.repeat(96) });
        meta.push({ type: 'text', textSignature: JSON.stringify({ v: 1, id: `msg_round_${round}`, phase: 'commentary' }) });
      }
    }
    content.push({ type: 'text', text: `Synthetic trailing text ${round}.` });
    meta.push({ type: 'text', textSignature: `msg_legacy_${round}` });
    messages.push({ role: 'assistant', content, source: { provider: model.provider, model: model.id, replayState: replayEnvelope(model, meta) } });
    for (let index = 5; index >= 0; index--) messages.push({ role: 'tool', toolCallId: `call_${round}_${index}|fc_round_${round}_${index}`, content:
      index === 0 ? [] : index === 1 ? [{ type: 'text', text: ' \n\t ' }] : [{ type: 'text', text: `synthetic output ${round}:${index}; first` }, { type: 'text', text: '; second' }] });
    if (round === 3) messages.push({ role: 'system', content: [{ type: 'text', text: 'Synthetic mid-history system </system-reminder> escaping check.' }] });
    messages.push({ role: 'user', content: [{ type: 'text', text: `Synthetic continuation ${round}.` }] });
  }
  messages.push({ role: 'assistant', content: [], source: { provider: model.provider, model: model.id } });
  return { provider: model.provider, model: model.id, system: 'Synthetic request-level system instructions.', messages, tools,
    sessionId: 'cache-lab-static-session', reasoningEffort: 'high' };
}

async function capture(label, fn) {
  const before = captures.length;
  activeCapture = label;
  try {
    const result = await fn();
    assert.equal(captures.length, before + 1, `${label}: expected exactly one mocked fetch`);
    return { capture: captures[before], result };
  } finally { activeCapture = undefined; }
}
async function runDSH(label, model, request, profileOverrides = {}) {
  let nativeContext;
  let simpleOptions;
  const onReplayDegrade = [];
  // Explicit in-memory auth prevents ambient credentials, files, and user profile access.
  const models = createModels({ authContext: { env: async () => { throw new Error('Ambient env lookup forbidden'); }, fileExists: async () => { throw new Error('Auth file access forbidden'); } } });
  models.setProvider({ id: model.provider, name: 'Synthetic route', getModels: () => [model],
    auth: { apiKey: { name: 'Synthetic auth', resolve: async ({ credential }) => ({ auth: { apiKey: credential.key }, env: {}, source: 'synthetic-only' }) } },
    streamSimple: (m, context, options) => {
      nativeContext = plain(context);
      // DSH aborts its consumer signal during stream cleanup; it is not reusable.
      const { signal: _completedRequestSignal, ...reusableOptions } = options;
      simpleOptions = reusableOptions;
      return responses.streamSimple(m, context, { ...options, fetch: mockFetch });
    },
    stream: responses.stream,
  });
  const profile = { modelErrors: new Map(), configuredMaxTokens: new Map(), streamIdleTimeoutMs: 60000, ...profileOverrides };
  const snapshot = { profiles: new Map([[model.provider, profile]]), models };
  const adapter = new PiAiAdapter({ resolveApiKey: async () => 'sk-SYNTHETIC-NOT-A-REAL-KEY', onReplayDegrade: event => onReplayDegrade.push(event.reason) });
  const captured = await capture(label, async () => {
    const chunks = [];
    for await (const chunk of adapter.streamWithSnapshot(request, snapshot)) chunks.push(chunk);
    const finish = chunks.at(-1);
    assert.equal(finish.type, 'finish');
    assert.equal(finish.reason.kind, 'tool-calls', JSON.stringify(finish));
    return chunks;
  });
  return { ...captured, nativeContext, simpleOptions, onReplayDegrade, model, request };
}
function diffs(a, b, path = '$') {
  if (Object.is(a, b)) return [];
  if (!a || !b || typeof a !== 'object' || typeof b !== 'object' || Array.isArray(a) !== Array.isArray(b)) return [{ path, dsh: a === undefined ? '<absent>' : a, native: b === undefined ? '<absent>' : b }];
  return [...new Set([...Object.keys(a), ...Object.keys(b)])].flatMap(key => diffs(a[key], b[key], `${path}${Array.isArray(a) ? `[${key}]` : `.${key}`}`));
}
function fieldReport(a, b) {
  return Object.fromEntries(['model', 'input', 'instructions', 'stream', 'store', 'tools', 'tool_choice', 'reasoning', 'include', 'max_output_tokens', 'prompt_cache_key', 'prompt_cache_retention', 'prompt_cache_options', 'previous_response_id', 'temperature'].map(key => [key, {
    equal: diffs(a[key], b[key]).length === 0,
    dsh: a[key] === undefined ? '<absent>' : key === 'input' ? { count: a.input.length, sha256: hash(JSON.stringify(a.input)) } : a[key],
    native: b[key] === undefined ? '<absent>' : key === 'input' ? { count: b.input.length, sha256: hash(JSON.stringify(b.input)) } : b[key],
  }]));
}
function summary(body) {
  const counts = {};
  for (const item of body.input) counts[item.type ?? item.role] = (counts[item.type ?? item.role] ?? 0) + 1;
  return { keys: Object.keys(body), bytes: Buffer.byteLength(JSON.stringify(body)), inputCounts: counts,
    firstItem: body.input[0], reasoning: body.reasoning ?? '<absent>', include: body.include ?? '<absent>',
    cacheRetention: body.prompt_cache_retention ?? '<absent>', strict: body.tools.map(tool => tool.strict ?? '<absent>') };
}

const cases = {};
const full = {};
for (const id of ['gpt6sol', 'deepseekflash']) {
  const model = modelConfig(id);
  for (const retention of ['default', 'long']) {
    const label = `${id}-${retention}`;
    const run = await runDSH(label, model, syntheticRequest(model), retention === 'default' ? {} : { cacheRetention: retention });
    full[label] = run;
    cases[label] = run.capture.body;
  }
}
// Parent supplied these sanitized public route/model facts; no profile is read here.
// Materialized scalar defaults follow installed resolveProfiles/resolveRouteModels lines 694-711,1157-1158.
const nosafeRuns = {};
for (const [id, contextWindow] of [['gpt-6.1-sol', 1000000], ['deepseek-v4.1-flash-abliterated', 258400]]) {
  const m = modelConfig(id, { name: id, provider: 'nosafe', contextWindow, maxTokens: 128000, input: ['text', 'image'],
    thinkingLevelMap: { minimal: null, low: 'low', medium: 'medium', high: 'high', xhigh: null, max: null } });
  const req = syntheticRequest(m);
  const run = await runDSH(`nosafe-${id}-default`, m, req);
  nosafeRuns[id] = run;
  cases[`nosafe-${id}-default`] = run.capture.body;
}
const markedNosafe = await runDSH('nosafe-markAgentLoopRequest', nosafeRuns['gpt-6.1-sol'].model,
  markAgentLoopRequest(structuredClone(nosafeRuns['gpt-6.1-sol'].request)));
const primary = full['gpt6sol-long'];
const model = primary.model;
const request = primary.request;
for (const [label, override, profile, mutate] of [
  ['gpt6sol-none', {}, { cacheRetention: 'none' }, () => {}],
  ['gpt6sol-strict-supported', { compat: { supportsStrictMode: true } }, {}, () => {}],
  ['gpt6sol-nonreasoning', { reasoning: false }, {}, req => { delete req.reasoningEffort; }],
  ['gpt6sol-reasoning-default', {}, {}, req => { delete req.reasoningEffort; }],
  ['gpt6sol-reasoning-off', {}, {}, req => { req.reasoningEffort = 'off'; }],
  ['gpt6sol-long-unsupported', { compat: { supportsLongCacheRetention: false } }, { cacheRetention: 'long' }, () => {}],
  ['gpt6sol-foreign-history', {}, {}, req => { for (const msg of req.messages) if (msg.role === 'assistant') delete msg.source.replayState; }],
  ['gpt6sol-bad-replay', {}, {}, req => { const msg = req.messages.find(msg => msg.role === 'assistant' && msg.content.length); msg.source.replayState.response.version = 999; }],
]) {
  const m = modelConfig('gpt6sol', override);
  const req = syntheticRequest(m);
  mutate(req);
  const run = await runDSH(label, m, req, profile);
  full[label] = run;
  cases[label] = run.capture.body;
}
// Prove real stream -> DSH finish.replayState -> next request native signatures, with no guessed replay state.
const replyBlocks = primary.result.filter(chunk => chunk.type === 'block-end').map(chunk => chunk.block);
const finish = primary.result.at(-1);
const replayRequest = { ...request, messages: [...request.messages,
  { role: 'assistant', content: replyBlocks, source: { provider: model.provider, model: model.id, replayState: finish.replayState } },
  { role: 'tool', toolCallId: 'call_sse_fixture|fc_sse_fixture', content: [{ type: 'text', text: 'Synthetic follow-up result.' }] },
] };
const replayRun = await runDSH('gpt6sol-sse-roundtrip', model, replayRequest);
cases['gpt6sol-sse-roundtrip'] = replayRun.capture.body;

// Native positive control: use the REAL converter for input/tools, not hand-written serialization.
// Other top-level params come from an actual adapter capture, not assumptions about provider defaults.
const directInput = plain(convertResponsesMessages(model, primary.nativeContext, new Set(['openai', 'openai-codex', 'opencode']), {
  supportsMidConvoSystemMessages: false, supportsAdditionalTools: false, supportsToolSearch: false,
  toolOptions: { supportsStrictMode: false, supportsOpenAIGrammarTools: false },
}));
assert.deepEqual(directInput, primary.capture.body.input);
const nativeParams = { ...primary.capture.body, input: directInput, tools: convertResponsesTools(tools, { supportsStrictMode: false, supportsOpenAIGrammarTools: false }) };
const native = await capture('raw-native-equivalent', async () => {
  const client = new OpenAI({ apiKey: 'sk-SYNTHETIC-NOT-A-REAL-KEY', baseURL: model.baseUrl, fetch: mockFetch, maxRetries: 0 });
  const stream = await client.responses.create(nativeParams);
  for await (const event of stream) { assert.equal(typeof event.type, 'string'); }
});
// Preserve original argument-string whitespace in an explicitly NON-equivalent native contrast.
const originalArgsById = new Map(request.messages.flatMap(msg => msg.role === 'assistant' ? msg.content.filter(block => block.type === 'tool-call').map(block => [block.id.split('|')[0], block.arguments]) : []));
const preservedParams = structuredClone(nativeParams);
for (const item of preservedParams.input) if (item.type === 'function_call') item.arguments = originalArgsById.get(item.call_id);
const nativePreserved = await capture('raw-native-original-arguments-NOT-equivalent', async () => {
  const client = new OpenAI({ apiKey: 'sk-SYNTHETIC-NOT-A-REAL-KEY', baseURL: model.baseUrl, fetch: mockFetch, maxRetries: 0 });
  const stream = await client.responses.create(preservedParams);
  for await (const event of stream) { assert.equal(typeof event.type, 'string'); }
});
// A separate direct streamSimple control reaches the same body from the captured native context/options.
const simple = await capture('pi-streamSimple-equivalent', async () => {
  const events = responses.streamSimple(model, primary.nativeContext, { ...primary.simpleOptions, fetch: mockFetch });
  const reply = await events.result();
  assert.equal(reply.stopReason, 'toolUse');
});

const checks = [];
function check(name, fn) { fn(); checks.push(name); }
check('SDK native equivalent has zero structural field differences', () => assert.deepEqual(native.capture.body, primary.capture.body));
check('direct pi-ai streamSimple body equals actual DSH body', () => assert.deepEqual(simple.capture.body, primary.capture.body));
check('primary has 48 function_calls and 48 separate function_call_outputs', () => {
  assert.equal(primary.capture.body.input.filter(item => item.type === 'function_call').length, 48);
  assert.equal(primary.capture.body.input.filter(item => item.type === 'function_call_output').length, 48);
});
check('default cache retention omits retention; long adds only 24h', () => {
  for (const id of ['gpt6sol', 'deepseekflash']) {
    assert.equal(cases[`${id}-default`].prompt_cache_retention, undefined);
    assert.equal(cases[`${id}-long`].prompt_cache_retention, '24h');
    assert.deepEqual(diffs(cases[`${id}-default`], cases[`${id}-long`]).map(d => d.path), ['$.prompt_cache_retention']);
  }
});
check('none removes cache key; unsupported long omits retention', () => {
  assert.equal(cases['gpt6sol-none'].prompt_cache_key, undefined);
  assert.equal(cases['gpt6sol-long-unsupported'].prompt_cache_retention, undefined);
});
check('strict omitted by default, false when compatibility support is enabled', () => {
  assert.ok(primary.capture.body.tools.every(tool => !Object.hasOwn(tool, 'strict')));
  assert.ok(cases['gpt6sol-strict-supported'].tools.every(tool => tool.strict === false));
  assert.equal(convertResponsesTools(tools)[0].strict, false);
});
check('reasoning enabled sends high + summary auto + encrypted include; omitted/off sends none only', () => {
  assert.deepEqual(primary.capture.body.reasoning, { effort: 'high', summary: 'auto' });
  assert.deepEqual(primary.capture.body.include, ['reasoning.encrypted_content']);
  for (const label of ['gpt6sol-reasoning-default', 'gpt6sol-reasoning-off']) {
    assert.deepEqual(cases[label].reasoning, { effort: 'none' });
    assert.equal(cases[label].include, undefined);
  }
  assert.equal(cases['gpt6sol-nonreasoning'].reasoning, undefined);
});
check('system goes in developer input, never top-level instructions, leading text concatenates', () => {
  assert.equal(primary.capture.body.instructions, undefined);
  assert.deepEqual(primary.capture.body.input[0], { role: 'developer', content: 'Synthetic request-level system instructions.\n\nSynthetic leading history system A.Synthetic leading history system B.' });
});
check('empty user string and empty assistant text block survive; empty assistant message skipped', () => {
  assert.ok(primary.capture.body.input.some(item => item.role === 'user' && item.content[0].text === ''));
  assert.ok(primary.capture.body.input.some(item => item.role === 'assistant' && item.content[0].text === ''));
});
check('interleaved assistant blocks flatten in original order; reverse tool-result order preserved', () => {
  const input = primary.capture.body.input;
  const start = input.findIndex(item => item.id === 'rs_round_0');
  assert.deepEqual(input.slice(start, start + 9).map(item => item.type), ['reasoning', 'function_call', 'function_call', 'function_call', 'message', 'function_call', 'function_call', 'function_call', 'message']);
  assert.deepEqual(input.slice(start + 9, start + 15).map(item => item.call_id), ['call_0_5', 'call_0_4', 'call_0_3', 'call_0_2', 'call_0_1', 'call_0_0']);
});
check('DSH empty tool text becomes (no output), whitespace-only becomes (no tool output), blocks concatenate', () => {
  const out = primary.capture.body.input.filter(item => item.type === 'function_call_output');
  assert.equal(out.find(item => item.call_id === 'call_0_0').output, '(no output)');
  assert.equal(out.find(item => item.call_id === 'call_0_1').output, '(no tool output)');
  assert.equal(out.find(item => item.call_id === 'call_0_2').output, 'synthetic output 0:2; first; second');
});
check('tool argument original whitespace is lost by parse/stringify', () => {
  const call = primary.capture.body.input.find(item => item.type === 'function_call');
  const original = request.messages.find(msg => msg.role === 'assistant' && msg.content.length).content[1].arguments;
  assert.notEqual(call.arguments, original);
  assert.equal(call.arguments, JSON.stringify(JSON.parse(original)));
});
check('same-model signed reasoning and text id/phase replay; foreign replay drops reasoning/signatures', () => {
  assert.deepEqual(primary.capture.body.input.find(item => item.id === 'rs_round_0'), JSON.parse(request.messages[3].source.replayState.blocks[0].thinkingSignature));
  assert.equal(primary.capture.body.input.find(item => item.id === 'msg_round_0').phase, 'commentary');
  assert.ok(!cases['gpt6sol-foreign-history'].input.some(item => item.type === 'reasoning'));
  assert.ok(!cases['gpt6sol-foreign-history'].input.some(item => item.id === 'msg_round_0'));
  assert.equal(full['gpt6sol-bad-replay'].onReplayDegrade.length, 1);
});
check('real synthetic SSE terminal backfills encrypted reasoning and persists/replays text phase/id', () => {
  const input = replayRun.capture.body.input;
  assert.equal(input.find(item => item.id === 'rs_sse_fixture').encrypted_content, terminalReplyItems[0].encrypted_content);
  assert.equal(input.find(item => item.id === 'msg_sse_fixture').phase, 'commentary');
  assert.equal(input.find(item => item.id === 'fc_sse_fixture').arguments, '{"index":99,"label":"sse fixture"}');
  assert.equal(replayRun.capture.body.previous_response_id, undefined);
});

// Direct converter edge cases: distinguish native empty arrays from DSH's string path.
const nativeEdges = plain(convertResponsesMessages(model, { systemPrompt: '', messages: [
  { role: 'user', content: [], timestamp: 0 }, { role: 'user', content: '', timestamp: 0 },
  { role: 'assistant', api: model.api, provider: model.provider, model: model.id, content: [], stopReason: 'stop', timestamp: 0, usage },
  { role: 'assistant', api: model.api, provider: model.provider, model: model.id, content: [{ type: 'thinking', thinking: 'unsigned readable reasoning is not emitted by Responses conversion' }, { type: 'toolCall', id: 'call_orphan|fc_orphan', name: 'lab_empty', arguments: {} }], stopReason: 'toolUse', timestamp: 0, usage },
  { role: 'user', content: 'orphan closure check', timestamp: 0 },
] }, new Set(['openai', 'openai-codex', 'opencode'])));
check('native empty user array skipped; unsigned thinking omitted; orphan output synthesized', () => {
  assert.equal(nativeEdges.filter(item => item.role === 'user').length, 2);
  assert.ok(!nativeEdges.some(item => item.type === 'reasoning'));
  assert.equal(nativeEdges.find(item => item.type === 'function_call_output').output, 'No result provided');
});
// streamSimple deliberately does not forward reasoningSummary (not in Simple options vocabulary).
const summarySimple = await capture('simple-reasoningSummary-not-forwarded', async () => {
  const events = responses.streamSimple(model, primary.nativeContext, { ...primary.simpleOptions, fetch: mockFetch, reasoningSummary: 'detailed' });
  await events.result();
});
const summaryFull = await capture('full-stream-reasoningSummary', async () => {
  const events = responses.stream(model, primary.nativeContext, { apiKey: 'sk-SYNTHETIC-NOT-A-REAL-KEY', fetch: mockFetch, env: {}, maxRetries: 0, reasoningEffort: 'high', reasoningSummary: 'detailed' });
  await events.result();
});
check('streamSimple summary defaults auto; full stream accepts detailed', () => {
  assert.equal(summarySimple.capture.body.reasoning.summary, 'auto');
  assert.equal(summaryFull.capture.body.reasoning.summary, 'detailed');
});
check('sanitized nosafe default calls capture cap128000, developer input, no retention and marker equality', () => {
  for (const run of Object.values(nosafeRuns)) {
    assert.equal(run.capture.body.max_output_tokens, 128000);
    assert.equal(run.capture.body.prompt_cache_retention, undefined);
    assert.equal(run.capture.body.input[0].role, 'developer');
    assert.equal(run.capture.body.instructions, undefined);
    assert.deepEqual(run.capture.body.reasoning, { effort: 'high', summary: 'auto' });
  }
  assert.deepEqual(markedNosafe.capture.body, nosafeRuns['gpt-6.1-sol'].capture.body);
  assert.deepEqual(markedNosafe.capture.headers, nosafeRuns['gpt-6.1-sol'].capture.headers);
});
check('original-argument native contrast differs in exactly 48 arguments strings, with JSON values equal', () => {
  const differences = diffs(primary.capture.body, nativePreserved.capture.body);
  assert.equal(differences.length, 48);
  for (const difference of differences) {
    assert.ok(difference.path.endsWith('.arguments'));
    assert.deepEqual(JSON.parse(difference.dsh), JSON.parse(difference.native));
  }
});
check('no socket/http/DNS network attempts occurred', () => assert.deepEqual(networkAttempts, []));
const guardSelfTests = [];
await assert.rejects(mockFetch('https://example.invalid/v1/responses'), /non-local URL blocked/);
guardSelfTests.push('nonlocalhost mock URL rejected before request handling');
assert.throws(() => net.createConnection({ host: 'example.invalid', port: 443 }), /all real network disabled/);
guardSelfTests.push('real socket path rejected synchronously');
assert.equal(networkAttempts.length, 1);
networkAttempts.length = 0; // Distinguish intentional guard test from accidental adapter attempts.

const versions = {};
for (const [name, path] of [
  ['dsh', join(ROOT, 'package.json')],
  ['dsh-llm-pi-ai', join(dirname(dirname(adapterPath)), 'package.json')],
  ['pi-ai', join(dirname(PI), 'package.json')],
  ['openai', join(dirname(sdkEntry), 'package.json')],
]) {
  const pkg = JSON.parse(await readFile(path, 'utf8'));
  versions[name] = { name: pkg.name, version: pkg.version, path };
}
const sourceSpecs = [
  [adapterPath, '28-33,61-92,184-253,586-608,694-711,1157-1158,1204-1219,1271-1388,1778-1790,1993-2032', 'DSH raw arguments, durable replay, context and streamSimple call'],
  [join(PI, 'models.js'), '427-474', 'Actual Models auth/header/context dispatch'],
  [join(PI, 'api/openai-responses.js'), '53-102,133-159,194-205,207-309', 'Cache, compatibility, fetch, request params, reasoning'],
  [join(PI, 'api/openai-responses-shared.js'), '12-57,91-118,169-332,337-369,466-484,626-646', 'Actual converter ordering/empty content/signature/strict semantics'],
  [join(PI, 'api/transform-messages.js'), '85-127,138-211', 'Same-model reasoning/text fidelity and missing tool outputs'],
  [join(PI, 'api/simple-options.js'), '18-42', 'Simple options forwarding and default token cap'],
  [join(PI, 'utils/identity.js'), '37-45,65-99', 'Pi user agent and provider attribution'],
  [join(ROOT, 'node_modules/@deepseek-ai/dsh-llm/lib/index.js'), '372-387,841-878', 'DSH loop-request WeakSet marker and user-agent attribution'],
  [join(dirname(sdkEntry), 'internal/request-options.js'), '25-51', 'Actual SDK JSON stringify'],
];
const sourceManifest = await Promise.all(sourceSpecs.map(async ([path, lines, purpose]) => ({ path, lines, purpose, sha256: hash(await readFile(path, 'utf8')) })));
const headerObservations = {
  dsh: primary.capture.headers, nativeSDK: native.capture.headers, directPiWithDshOptions: simple.capture.headers,
  nosafeDefault: nosafeRuns['gpt-6.1-sol'].capture.headers,
  markedAgentLoopRequestSameHeaders: JSON.stringify(markedNosafe.capture.headers) === JSON.stringify(nosafeRuns['gpt-6.1-sol'].capture.headers),
  externalHeaderWaterfallNotRun: 'PiAiAdapter.prepareHeaders omitted: no live ctx/plugin hooks executed. markAgentLoopRequest only adds WeakSet identity and did not change body/default headers in this adapter. Real deployment plugins may still contribute headers.',
  differences: diffs(primary.capture.headers, native.capture.headers),
  interpretation: 'Same body does not imply equal HTTP headers. DSH user-agent wins over pi identity; native SDK-only default user-agent/x-stainless are captured, not guessed. originator and provider attribution are absent unless shown. Auth values are never persisted. Session header values are redacted even though synthetic.',
  paidABProposalOnly: ['Hold exact body, credential, endpoint, session/cache keys, tool schemas and reasoning state fixed; alternate only captured static user-agent.', 'Hold body fixed; compare SDK x-stainless header group present vs absent.', 'Only if measured header exists, alternate originator/provider-attribution separately; do not invent a claim it exists.'],
};
const report = {
  mode: 'OFFLINE_SYNTHETIC_ONLY_NOT_A_PAID_CACHE_TEST', versions, sourceManifest, headerObservations, guardSelfTests,
  originalArgumentsNativeContrast: { structuralDiffs: diffs(primary.capture.body, nativePreserved.capture.body), note: 'Original whitespace preserved in separate native contrast, intentionally not equivalent to DSH. JSON values are equal.' },
  callChain: 'installed PiAiAdapter.streamWithSnapshot -> actual DSH context/replay/profileOptions -> actual pi-ai Models.streamSimple -> installed Responses.streamSimple/convertResponsesMessages -> installed OpenAI SDK -> supplied/global mockFetch -> synthetic Response SSE',
  substitutions: ['synthetic model/profile snapshot only; catalog/profile resolution not run', 'in-memory auth with explicit dummy key and empty env', 'supplied fetch injected at provider boundary and global fetch replaced; actual SDK JSON serialization retained', 'socket/http/http2/dgram disabled; no server; no proxy env changes'],
  primary: 'gpt6sol-long', fixtureSize: summary(primary.capture.body),
  sanitizedNosafeDefault: {
    provenance: 'Parent supplied sanitized config facts in message; no profile/config/runtime read. Parent endpoint https://api.nosafe.ai/v1 replaced by loopback URL before all mock calls.',
    materialization: 'Custom provider nosafe has no builtin catalog; declared reasoningEfforts materialize model.reasoning/thinkingLevelMap. Absent model/provider maxTokens -> installed default128000 (adapter lines694-711,1157-1158). No synthetic long retention applied to these cases.',
    models: Object.fromEntries(Object.entries(nosafeRuns).map(([id, run]) => [id, { materializedModel: run.model, capturedBodySummary: summary(run.capture.body) }])),
    markedRequestBodyEqual: diffs(markedNosafe.capture.body, nosafeRuns['gpt-6.1-sol'].capture.body).length === 0,
  },
  nativeEquivalent: { structuralDiffs: diffs(primary.capture.body, native.capture.body), rawBytesEqual: primary.capture.rawBody === native.capture.rawBody, fields: fieldReport(primary.capture.body, native.capture.body),
    limitation: 'Positive control: native SDK reuses captured non-input top-level fields; input/tools rebuilt with real converters. Not an independent cache-efficiency experiment.' },
  cases: Object.fromEntries(Object.entries(cases).map(([label, body]) => [label, { summary: summary(body), diffsFromPrimary: diffs(primary.capture.body, body).map(d => d.path) }])),
  gptVsDeepseekSyntheticConfigDiffs: diffs(cases['gpt6sol-default'], cases['deepseekflash-default']),
  originalArguments: { original: request.messages[3].content[1].arguments, outbound: primary.capture.body.input.find(item => item.type === 'function_call').arguments },
  sseRoundtrip: { replayState: finish.replayState, emittedBlocks: replyBlocks, replayedTail: replayRun.capture.body.input.slice(-4) },
  nativeEdges, checks, mockFetchCalls: captures.length, networkAttempts,
  caveats: [
    'Model ids are synthetic gpt6sol/deepseekflash, not assertions about a private deployed profile, real endpoint, or provider model capability. Provider route is literally openai-responses; api is openai-responses.',
    'Original argument JSON formatting cannot survive DSH parseArguments + Responses JSON.stringify. Retaining original native argument strings would be a separate, intentionally non-equivalent fixture.',
    'Opaque signatures here are synthetic sentinels, not valid model reasoning state. Actual future encrypted_content/ids/state are unknown and cannot be predicted or matched across independent real LLM rounds.',
    'SSE roundtrip uses actual DSH replayState production/reconstruction, but not the separate agent BlockAssembler/session persistence layer; no claim of live session fidelity.',
    'No real LLM, paid request, cache hit, pricing, server acceptance, or latency measured. Zero usage in SSE is a test fixture, not a measurement.',
  ],
};
await mkdir(OUT, { recursive: true });
async function save(name, value) { await writeFile(join(OUT, name), JSON.stringify(value, null, 2) + '\n'); }
await save('dsh-body.json', primary.capture.body);
await save('raw-native-equivalent.json', native.capture.body);
await save('raw-native-original-arguments-NOT-equivalent.json', nativePreserved.capture.body);
await save('header-observations.json', headerObservations);
await save('nosafe-gpt-6.1-sol-default-body.json', nosafeRuns['gpt-6.1-sol'].capture.body);
await save('nosafe-deepseek-flash-default-body.json', nosafeRuns['deepseek-v4.1-flash-abliterated'].capture.body);
await save('source-manifest.json', sourceManifest);
await writeFile(join(OUT, 'dsh-wire-body.txt'), primary.capture.rawBody);
await writeFile(join(OUT, 'raw-native-wire-body.txt'), native.capture.rawBody);
await save('synthetic-input.json', { modelConfigs: ['gpt6sol', 'deepseekflash'].map(id => modelConfig(id)), generateOptions: request });
await save('scenario-bodies.json', cases);
await save('comparison.json', report);
await save('captured-native-context.json', primary.nativeContext);
console.log(JSON.stringify({ ok: true, checks: checks.length, mockFetchCalls: captures.length, realNetworkAttempts: networkAttempts.length, versions: Object.fromEntries(Object.entries(versions).map(([key, value]) => [key, value.version])), primary: report.fixtureSize, nativeEquivalent: { structuralDiffs: 0, rawBytesEqual: report.nativeEquivalent.rawBytesEqual }, outputDirectory: OUT }, null, 2));
