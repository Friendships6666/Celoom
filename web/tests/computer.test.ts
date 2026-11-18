import { describe, expect, it } from 'vitest';
import { computerActivity, computerCapability, latestComputerActivity, parseComputerStatus, safeComputerText } from '../src/computer';
import { timeline, toolSummary } from '../src/events';
import type { AgentEvent } from '../src/types';

const event = (seq: number, type: string, data: Record<string, unknown>, session_id = 'this-session'): AgentEvent => ({ seq, type, data, session_id, created_at: '2026-11-14T00:00:00Z' });
const capture = { action: 'screenshot', target: 'browser', summary: 'Captured browser', is_error: false, capture_id: 'capture-id', width: 1280, height: 800 };
const status = { browser: { available: true, running: false }, desktop: { available: false }, policy: 'isolated_first', busy: false, privilege: { uid: 1000, mode: 'user', root_available: false }, host: { enabled: false, lease_remaining_secs: 0, physical_monitor: false, human_idle_ms: null } };

describe('computer contract and isolation', () => {
  it('does not fabricate capabilities or root authorization', () => {
    expect(parseComputerStatus(status).privilege.root_available).toBe(false);
    expect(computerCapability(undefined)).toMatchObject({ label: '能力未确认', tone: 'unknown' });
    expect(computerCapability({ binary: '/usr/bin/chromium' }).tone).toBe('unknown');
    expect(computerCapability({ available: false, running: true, error: 'Missing dependency' })).toMatchObject({ label: '不可用', detail: 'Missing dependency' });
    expect(computerCapability({ available: true, running: false }).label).toBe('可用 · 未启动');
    expect(computerCapability({ running: true }).label).toBe('运行中');
    expect(() => parseComputerStatus({ ...status, privilege: undefined })).toThrow('状态响应不完整');
    expect(() => parseComputerStatus({ ...status, host: { ...status.host, enabled: 'false' } })).toThrow();
    expect(() => parseComputerStatus({ ...status, busy: 'false' })).toThrow();
  });
  it('selects only the current session latest activity/capture, never an image_path', () => {
    const records = [event(1, 'computer', capture), event(3, 'computer', { ...capture, capture_id: 'other' }, 'other-session'), event(2, 'computer', { ...capture, action: 'click', capture_id: undefined })];
    expect(latestComputerActivity(records, 'this-session')?.action).toBe('click');
    expect(latestComputerActivity(records, 'this-session', true)?.capture_id).toBe('capture-id');
    expect(latestComputerActivity(records, 'new-session', true)).toBeNull();
    expect(computerActivity(event(4, 'computer', { ...capture, capture_id: '../secret', image_path: '/tmp/private' }))?.capture_id).toBeUndefined();
    expect(computerActivity(event(5, 'computer', { action: 'screenshot' }))).toBeNull();
  });
  it('folds screenshot metadata into its computer tool until tool_end', () => {
    const start = event(1, 'tool_start', { call_id: 'c', name: 'computer', args: { action: 'screenshot', target: 'browser' } });
    const shot = event(2, 'computer', capture);
    const end = event(3, 'tool_end', { call_id: 'c', output: JSON.stringify({ data: 'base64-fixture-secret', text: 'Capture completed' }), is_error: false });
    expect(timeline([start, shot])[0]).toMatchObject({ finished: false, capture: { capture_id: 'capture-id' } });
    const result = timeline([start, shot, end]);
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ finished: true, capture: { capture_id: 'capture-id', session_id: 'this-session' } });
    expect(JSON.stringify(result)).not.toContain('base64-fixture-secret');
    expect(timeline([start, { ...shot, session_id: 'other-session' }, end])[0]).not.toHaveProperty('capture');
  });
  it('accepts a capture_id directly in tool_end without embedding image bytes', () => {
    const result = timeline([event(1, 'tool_start', { call_id: 'c', name: 'functions.computer', args: { action: 'screenshot' } }), event(2, 'tool_end', { call_id: 'c', output: 'screenshot', ...capture })]);
    expect(result[0]).toMatchObject({ finished: true, capture: { capture_id: 'capture-id' } });
    expect(safeComputerText('result data:image/png;base64,aGVsbG8=')).not.toContain('aGVsbG8');
    expect(safeComputerText('{"image":{"data":"secret","mime":"image/png"},"text":"ok"}')).not.toContain('secret');
  });
  it.each([
    [{ action: 'click', target: 'browser', ref_id: 'ref-7', url: 'https://example.test' }, 'click · browser · ref-7'],
    [{ action: 'open', target: 'browser', url: 'https://example.test/page\nsecret' }, 'open · browser · https://example.test/page'],
    [{ action: 'launch', target: 'desktop', argv: ['app', '--private', 'test\nsecond line'] }, 'launch · desktop · app --private test'],
    [{ action: 'observe', target: null }, 'observe · browser'],
  ])('summarizes action target and reference/url/argv first line', (args, expected) => expect(toolSummary('functions.computer', args)).toBe(expected));
});
