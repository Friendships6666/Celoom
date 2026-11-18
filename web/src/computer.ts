import type { AgentEvent, ComputerActivity, ComputerStatus } from './types';

export const computerRecord = (value: unknown): Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {};
const finite = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value) && value >= 0;

// A malformed/stale backend must not become an affirmative capability or authorization.
export function parseComputerStatus(value: unknown): ComputerStatus {
  const status = computerRecord(value); const privilege = computerRecord(status.privilege); const host = computerRecord(status.host);
  if (status.policy !== 'isolated_first' || typeof status.busy !== 'boolean' || !('browser' in status) || !('desktop' in status)
    || !Number.isSafeInteger(privilege.uid) || !finite(privilege.uid) || !['root', 'sudo', 'user'].includes(String(privilege.mode)) || typeof privilege.root_available !== 'boolean'
    || typeof host.enabled !== 'boolean' || typeof host.physical_monitor !== 'boolean' || !finite(host.lease_remaining_secs) || !(host.human_idle_ms === null || finite(host.human_idle_ms))) {
    throw new Error('计算机状态响应不完整，请刷新重试。');
  }
  return value as ComputerStatus;
}

export function computerActivity(event: AgentEvent): ComputerActivity | null {
  const d = event.data;
  if (event.type !== 'computer' || typeof d.action !== 'string' || typeof d.target !== 'string' || typeof d.summary !== 'string' || typeof d.is_error !== 'boolean') return null;
  return { session_id: event.session_id, seq: event.seq, action: d.action, target: d.target, summary: safeComputerText(d.summary), is_error: d.is_error,
    ...(typeof d.capture_id === 'string' && d.capture_id && !/[\u0000-\u001f/\\]/.test(d.capture_id) && !['.', '..'].includes(d.capture_id) ? { capture_id: d.capture_id } : {}),
    ...(finite(d.width) && d.width > 0 ? { width: d.width } : {}), ...(finite(d.height) && d.height > 0 ? { height: d.height } : {}), created_at: event.created_at };
}
export function latestComputerActivity(events: AgentEvent[], sessionId: string, captureOnly = false): ComputerActivity | null {
  return events.filter(event => event.session_id === sessionId).map(computerActivity).filter((activity): activity is ComputerActivity => !!activity && (!captureOnly || !!activity.capture_id)).sort((a, b) => b.seq - a.seq)[0] ?? null;
}

export function computerCapability(value: unknown): { label: string; tone: 'ready' | 'unavailable' | 'unknown'; detail: string } {
  const status = computerRecord(value);
  const detail = ['error', 'reason', 'message'].map(key => status[key]).find(value => typeof value === 'string' && value.length) as string | undefined;
  // Missing availability is unknown, even if a binary name or isolation label exists.
  if (status.available === false || status.supported === false) return { label: '不可用', tone: 'unavailable', detail: safeComputerText(detail || '所需组件暂不可用，请检查依赖。') };
  if (status.running === true) return { label: '运行中', tone: 'ready', detail: safeComputerText(detail || '运行状态由服务器报告。') };
  if (status.available === true) return { label: '可用 · 未启动', tone: 'ready', detail: safeComputerText(detail || 'Agent 按任务需要启动，无需手动开启。') };
  return { label: '能力未确认', tone: 'unknown', detail: safeComputerText(detail || '等待服务器报告可用能力。') };
}

// Computer outputs never expose embedded screenshots as base64 in a tool fold.
export function safeComputerText(value: string): string {
  const redact = (input: unknown): unknown => {
    if (Array.isArray(input)) return input.map(redact);
    if (input !== null && typeof input === 'object') return Object.fromEntries(Object.entries(input).map(([key, item]) => [key, /^(data|base64|png|image_data)$/i.test(key) && typeof item === 'string' ? '[图片数据已省略]' : redact(item)]));
    return typeof input === 'string' ? input.replace(/data:image\/[a-z0-9.+-]+;base64,[a-z0-9+/=_-]+/gi, '[图片数据已省略]').replace(/iVBORw0KGgo[a-z0-9+/=\r\n]*/gi, '[PNG 数据已省略]') : input;
  };
  try { return JSON.stringify(redact(JSON.parse(value)), null, 2); } catch { return redact(value) as string; }
}
