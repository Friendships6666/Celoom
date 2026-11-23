import { describe, expect, it } from 'vitest';
import { assistantLabel, fileMarkers, goalSnapshot, isRelativeFilePath, latestSequence, mergeEvents, mergeSnapshots, questionSnapshot, sessionFromEvent, subagentLabel, timeline, toolSummary } from '../src/events';
import { directoryRevisions } from '../src/Files';
import { activeStatus, type AgentEvent, type Goal, type Question, type Session } from '../src/types';

const event = (seq: number, type: string, data: Record<string, unknown>): AgentEvent => ({ seq, type, data, session_id: 'session-test', created_at: '2026-11-13T12:00:00Z' });

describe('session routing and file event contracts', () => {
  it('updates session selection and explicit/omitted/inherited effort without losing status', () => {
    const session = { id: 'session-test', selection: { site_id: 'old', key_id: 'old', model_id: 'old' }, reasoning_effort: null, status: 'running', context_epoch: 2 } as Session;
    const selection = { site_id: 'new-site', key_id: 'new-key', model_id: 'new-model' };
    for (const reasoning_effort of [null, '', 'high']) {
      expect(sessionFromEvent(session, event(8, 'session_settings', { selection, reasoning_effort }))).toMatchObject({ selection, reasoning_effort, status: 'running', context_epoch: 2 });
    }
    expect(sessionFromEvent(session, event(9, 'session_settings', { selection: { site_id: 'broken' }, reasoning_effort: null }))).toBe(session);
  });
  it('preserves real per-attempt source across route changes and retries', () => {
    const source = { attempt_id: 'a', site_name: 'Original site', model_name: 'Original model', model: 'actual-model', reasoning_effort: 'high' };
    const result = timeline([event(1, 'assistant_source', source), event(2, 'assistant_delta', { attempt_id: 'a', text: 'partial' }), event(3, 'session_settings', { site_name: 'New site', model_name: 'New model' }), event(4, 'attempt_failed', { attempt_id: 'a', message: 'network' }), event(5, 'assistant_source', { ...source, attempt_id: 'b', site_name: 'New site', model_name: 'New model' }), event(6, 'assistant_message', { attempt_id: 'b', text: 'answer' }), event(7, 'assistant_source', { ...source, site_name: 'must not overwrite' })]);
    expect(result).toHaveLength(2);
    expect(result[0]).toMatchObject({ kind: 'assistant', failed: 'network', source });
    expect(result[1]).toMatchObject({ kind: 'assistant', source: { site_name: 'New site', model_name: 'New model' } });
    expect(assistantLabel(source, { site: { name: 'Current site' }, model: { name: 'Current model', model: 'current' } })).toBe('Original site - Original model');
    expect(assistantLabel(undefined, { site: { name: 'Current site' }, model: { name: 'Current model', model: 'current' } })).toBe('Current site - Current model');
    expect(assistantLabel()).not.toContain('Celoom');
  });
  it('keeps actual files at every round end including empty rounds and rejects unsafe paths', () => {
    const result = timeline([event(1, 'user_message', { text: 'first' }), event(2, 'assistant_message', { attempt_id: 'a', text: 'done' }), event(3, 'turn_files', { run_id: 'first', files: [{ path: 'site/index.html', name: 'index.html', kind: 'new' }, { path: '../secret', name: 'secret', kind: 'new' }] }), event(4, 'user_message', { text: 'second' }), event(5, 'turn_files', { run_id: 'second', files: [] })]);
    expect(result.map(item => item.kind)).toEqual(['user', 'assistant', 'files', 'user', 'files']);
    expect(result[2]).toMatchObject({ runId: 'first', files: [{ path: 'site/index.html', name: 'index.html', kind: 'new' }] });
    expect(result[4]).toMatchObject({ runId: 'second', files: [] });
    for (const path of ['/etc/passwd', '../file', 'a/../../b', 'C:/file', 'a\\\\b', 'a//b', 'a/./b']) expect(isRelativeFilePath(path)).toBe(false);
  });
  it('does not clear the active round file markers when a message is only queued', () => {
    const events = [event(1, 'user_message', { text: 'start' }), event(2, 'file_activity', { path: 'new.txt', status: 'editing', is_new: true }), event(3, 'user_message', { text: 'followup', queued: true, client_message_id: 'queued' }), event(4, 'message_delivered', { client_message_id: 'queued' })];
    expect(fileMarkers(events)['new.txt']).toEqual({ status: 'editing', isNew: true });
  });
  it('tracks editing/new markers only for the current turn and finalizes at round end', () => {
    const events = [event(1, 'user_message', { text: 'start' }), event(2, 'file_activity', { path: 'src/new.ts', status: 'editing', is_new: true }), event(3, 'file_changed', { path: 'src/new.ts', kind: 'modified', is_dir: false })];
    expect(fileMarkers(events)['src/new.ts']).toEqual({ status: 'editing', isNew: true });
    expect(fileMarkers([...events, event(4, 'file_activity', { path: 'src/new.ts', status: 'done', is_new: true })])['src/new.ts']).toEqual({ status: 'new', isNew: true });
    expect(fileMarkers([...events, event(5, 'turn_files', { run_id: 'run', files: [{ path: 'src/new.ts', name: 'new.ts', kind: 'new' }] })])['src/new.ts']).toEqual({ status: 'new', isNew: true });
    expect(fileMarkers([...events, event(6, 'user_message', { text: 'next' })])).toEqual({});
  });
  it('invalidates changed ancestors without touching unrelated directories', () => {
    expect(directoryRevisions([event(2, 'file_changed', { path: 'src/deep/a.ts', kind: 'new', is_dir: false }), event(3, 'turn_files', { run_id: 'run', files: [{ path: 'docs/result.svg', name: 'result.svg', kind: 'new' }] })])).toEqual({ '': 3, src: 2, 'src/deep': 2, docs: 3 });
    expect(directoryRevisions([event(4, 'file_changed', { path: 'src/deep', kind: 'modified', is_dir: true })])).toEqual({ '': 4, src: 4, 'src/deep': 4 });
  });
  it.each([
    ['bash', { command: '\n  printf "a && b"\npwd', description: 'wrong summary' }, '  printf "a && b"'],
    ['read', { path: 'src/main.rs' }, 'src/main.rs'],
    ['functions.edit', { file_path: 'web/src/App.tsx' }, 'web/src/App.tsx'],
    ['write', JSON.stringify({ path: 'index.html' }), 'index.html'],
    ['web_search', { query: 'JustAIAgent tools', max_results: 5 }, 'JustAIAgent tools'],
    ['functions.web_fetch', JSON.stringify({ url: 'https://example.test/docs', max_chars: 12000 }), 'https://example.test/docs'],
    ['read_image', { path: 'assets/logo.png' }, 'assets/logo.png'],
    ['grep', { pattern: 'TODO|FIXME', path: 'src' }, 'TODO|FIXME'],
    ['glob', { pattern: '**/*.ts' }, '**/*.ts'],
    ['job_output', { job_id: 'job-123' }, 'output · job-123'],
    ['job', { action: 'poll', job_id: 'job-123' }, 'poll · job-123'],
    ['subagent', { action: 'poll', id: 'child-123' }, 'poll · child-123'],
    ['subagent', { description: 'Inspect layout', prompt: 'long prompt' }, 'Inspect layout'],
    ['subagent', { action: 'models', model_id: null }, '查看可用子代理模型'],
    ['functions.subagent', { action: 'spawn', model_id: 'private-pool-id', task: 'Inspect layout\nMore instructions' }, 'Inspect layout'],
    ['subagent', { action: 'spawn', model_id: null }, '启动子代理'],
  ])('summarizes %s by its core operation', (name, args, expected) => expect(toolSummary(name, args)).toBe(expected));
});

describe('autonomy snapshot contracts', () => {
  const question: Question = { id: 'q', session_id: 'session-test', call_id: 'call', question: 'Which approach?', blocking: true, options: ['A', 'B'], status: 'pending', answer: null, delivered: false, created_at: '2026-11-13T12:00:00Z', updated_at: '2026-11-13T12:00:00Z' };
  const goal: Goal = { id: 'g', session_id: 'session-test', title: 'Ship', objective: 'Verify and ship', status: 'active', progress: 'Verified', rounds: 4, created_at: question.created_at, updated_at: question.updated_at };
  it('keeps waiting and automatic compaction busy but paused idle', () => {
    for (const status of ['running', 'compacting', 'auto_compacting', 'waiting_for_answer']) expect(activeStatus(status)).toBe(true);
    for (const status of ['paused', 'completed', undefined]) expect(activeStatus(status)).toBe(false);
  });
  it('does not replace a waiting gate with ordinary completed status', () => {
    const waiting = { status: 'waiting_for_answer' } as Session;
    expect(sessionFromEvent(waiting, event(1, 'status', { status: 'completed' }))).toBe(waiting);
    expect(sessionFromEvent(waiting, event(2, 'status', { status: 'running' })).status).toBe('running');
  });
  it('accepts full question snapshots and rejects cross-session or malformed ones', () => {
    for (const type of ['question', 'question_answer', 'question_withdrawn']) expect(questionSnapshot(event(1, type, { question }))).toEqual(question);
    expect(questionSnapshot(event(1, 'question', { question: { ...question, session_id: 'other' } }))).toBeNull();
    expect(questionSnapshot(event(1, 'question', { question: { ...question, options: [1] } }))).toBeNull();
    expect(questionSnapshot(event(1, 'question', { id: 'q' }))).toBeNull();
  });
  it('upserts durable answers without duplicate or stale pending regressions', () => {
    const answered: Question = { ...question, status: 'answered', answer: 'A', updated_at: '2026-11-13T12:01:00Z' };
    expect(mergeSnapshots([question], [answered, question, answered])).toEqual([answered]);
    expect(mergeSnapshots([answered], [{ ...question, updated_at: answered.updated_at }])).toEqual([answered]);
  });
  it('folds goal progress, rounds and agent completion from snapshots', () => {
    expect(goalSnapshot(event(1, 'goal', { goal }))).toEqual(goal);
    expect(goalSnapshot(event(1, 'goal', { goal: { ...goal, session_id: 'other' } }))).toBeNull();
    const completed: Goal = { ...goal, status: 'completed', rounds: 5 };
    expect(mergeSnapshots([goal], [completed, goal])).toEqual([completed]);
  });
  it('labels compaction neutrally and summarizes autonomy tools without losing old tools', () => {
    expect(timeline([event(1, 'compacted', { summary: 'Safe summary' })])[0]).toMatchObject({ text: '上下文已压缩\nSafe summary' });
    expect(toolSummary('ask_user', { action: 'ask', question: 'Choose A?' })).toBe('ask · Choose A?');
    expect(toolSummary('functions.goal', { action: 'complete', progress: 'Tests passed' })).toBe('complete · Tests passed');
  });
});

describe('committed event projection', () => {
  it('deduplicates global sequences and keeps sorted history', () => {
    const a = event(19, 'status', { status: 'running' });
    const b = event(23, 'status', { status: 'completed' });
    expect(mergeEvents([b], [a, b, a]).map(e => e.seq)).toEqual([19, 23]);
    expect(latestSequence([a, b])).toBe(23);
    expect(latestSequence([])).toBe(0);
  });
  it('projects delivery mode and a single idempotent interruption badge without another user row', () => {
    const accepted = event(1, 'user_message', { text: 'same payload', client_message_id: 'uuid', queued: true, delivery_mode: 'after_call' });
    const interrupt = event(2, 'message_interrupted', { client_message_id: 'uuid', delivery_mode: 'immediate' });
    const result = timeline([accepted, interrupt, interrupt, event(3, 'message_interrupted', interrupt.data), event(4, 'user_message', accepted.data)]);
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ kind: 'user', text: 'same payload', queued: true, deliveryMode: 'immediate', interrupted: true });
    expect(timeline([accepted, interrupt, event(5, 'message_delivered', { client_message_id: 'uuid' }), event(6, 'message_delivered', { client_message_id: 'uuid' })])[0]).toMatchObject({ queued: false, deliveryMode: 'immediate', interrupted: true });
  });
  it('defaults old user formats to after_call and isolates interruption and delivery by session', () => {
    const user = event(2, 'user_message', { client_message_id: 'uuid', text: 'legacy', queued: true });
    const other = { ...event(3, 'message_interrupted', { client_message_id: 'uuid' }), session_id: 'other-session' };
    const delivery = { ...event(4, 'message_delivered', { client_message_id: 'uuid' }), session_id: 'other-session' };
    expect(timeline([user, other, delivery])[0]).toMatchObject({ deliveryMode: 'after_call', queued: true });
    expect(timeline([user, other, delivery])[0]).not.toHaveProperty('interrupted');
    expect(timeline([event(1, 'message_interrupted', { client_message_id: 'uuid' }), user])[0]).toMatchObject({ deliveryMode: 'immediate', interrupted: true });
    expect(timeline([event(1, 'user_message', { text: 'manual legacy immediate', delivery_mode: 'immediate' })])[0]).toMatchObject({ deliveryMode: 'immediate' });
  });
  it('replaces accumulated assistant deltas with one authoritative message', () => {
    const events = [event(1, 'assistant_delta', { attempt_id: 'a', text: 'Hello ' }), event(2, 'assistant_delta', { attempt_id: 'a', text: 'world' }), event(3, 'assistant_message', { attempt_id: 'a', id: 'm', text: 'Hello world!' }), event(4, 'assistant_delta', { attempt_id: 'a', text: 'ignored late delta' })];
    const result = timeline([...events, events[2]]);
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ kind: 'assistant', text: 'Hello world!', provisional: false });
  });
  it('keeps failed attempts distinct from a retry and flags them', () => {
    const result = timeline([event(1, 'assistant_delta', { attempt_id: 'failed', text: 'partial' }), event(2, 'attempt_failed', { attempt_id: 'failed', message: 'stream ended' }), event(3, 'assistant_message', { attempt_id: 'retry', id: 'm', text: 'complete' })]);
    expect(result).toHaveLength(2);
    expect(result[0]).toMatchObject({ text: 'partial', failed: 'stream ended', provisional: false });
    expect(result[1]).toMatchObject({ text: 'complete', provisional: false });
  });
  it('keeps actual child source through incomplete updates and later route changes', () => {
    const source = { site_name: 'Actual site', model_name: 'Actual model', model: 'api-model', model_id: 'pool-id', reasoning_effort: '' };
    const result = timeline([event(1, 'subagent', { id: 'child', task: 'Inspect', status: 'running', ...source }), event(2, 'subagent', { id: 'child', status: 'completed', model_id: null, site_name: 'New site', model_name: 'New model', reasoning_effort: 'high' }), event(3, 'subagent', { id: 'child', status: 'completed' })]);
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ kind: 'subagent', task: 'Inspect', status: 'completed', source });
    expect(subagentLabel(source)).toBe('Actual site - Actual model');
    expect(subagentLabel({ site_name: 'Actual site', model: 'wire-model' })).toBe('Actual site - wire-model');
    expect(subagentLabel()).toBe('');
  });
  it('uses successful spawn result provenance and fills missing event fields without duplicate child cards', () => {
    const result = timeline([event(1, 'tool_start', { call_id: 'spawn', name: 'functions.subagent', args: JSON.stringify({ action: 'spawn', task: 'Inspect layout', model_id: 'pool' }) }), event(2, 'subagent', { id: 'child', status: 'running', site_name: 'Event site' }), event(3, 'tool_end', { call_id: 'spawn', output: JSON.stringify({ id: 'child', status: 'running', site_name: 'Result site', model_name: 'Result model', model: 'wire-model', reasoning_effort: null, model_id: 'pool' }), is_error: false }), event(4, 'subagent', { id: 'child', status: 'completed' })]);
    expect(result).toHaveLength(2);
    expect(result[1]).toMatchObject({ kind: 'subagent', task: 'Inspect layout', status: 'completed', source: { site_name: 'Event site', model_name: 'Result model', model: 'wire-model', reasoning_effort: null, model_id: 'pool' } });
  });
  it('supports result-only cards and never creates children for models or failed spawns', () => {
    const result = timeline([event(1, 'tool_start', { call_id: 'spawn', name: 'subagent', args: { action: 'spawn', task: 'Inspect' } }), event(2, 'tool_end', { call_id: 'spawn', output: JSON.stringify({ id: 'child', status: 'running', site_name: 'Site', model_name: 'Model', reasoning_effort: 'high' }), is_error: false }), event(3, 'tool_start', { call_id: 'catalog', name: 'subagent', args: { action: 'models' } }), event(4, 'tool_end', { call_id: 'catalog', output: JSON.stringify({ id: 'not-child' }), is_error: false }), event(5, 'tool_start', { call_id: 'failed', name: 'subagent', args: { action: 'spawn' } }), event(6, 'tool_end', { call_id: 'failed', output: JSON.stringify({ id: 'not-child' }), is_error: true })]);
    expect(result.filter(item => item.kind === 'subagent')).toEqual([expect.objectContaining({ id: 'child', task: 'Inspect', source: { site_name: 'Site', model_name: 'Model', reasoning_effort: 'high' } })]);
  });
  it('folds tool output and subagent status updates into their original cards', () => {
    const result = timeline([event(1, 'tool_start', { call_id: 'tool', name: 'bash', args: { command: 'pwd' } }), event(2, 'tool_output', { call_id: 'tool', text: 'partial' }), event(3, 'tool_end', { call_id: 'tool', name: 'bash', output: 'done', is_error: false }), event(4, 'subagent', { id: 'child', task: 'inspect', status: 'running' }), event(5, 'subagent', { id: 'child', status: 'completed' })]);
    expect(result).toHaveLength(2);
    expect(result[0]).toMatchObject({ kind: 'tool', output: 'done', finished: true, error: false });
    expect(result[1]).toMatchObject({ kind: 'subagent', task: 'inspect', status: 'completed' });
  });
});
