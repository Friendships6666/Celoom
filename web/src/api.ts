import type { QualityCheckInput, QualityPlan, QualityRun } from './qualityChecks';
import { defaultSearchConfig, type AppConfig, type Bootstrap, type ComputerStatus, type Diagnostic, type FileListing, type Goal, type GoalAction, type Question, type RuntimeOverview, type MessageInput, type InterruptionResult, type Selection, type Session, type SessionDetail, type SessionSettings } from './types';

export class ApiError extends Error {
  constructor(message: string, public status: number, public code?: string) { super(message); this.name = 'ApiError'; }
}
export async function request<T>(path: string, method = 'GET', body?: unknown, signal?: AbortSignal): Promise<T> {
  const response = await fetch(`/api${path}`, {
    method, credentials: 'same-origin', signal,
    headers: { Accept: 'application/json', ...(method === 'GET' ? {} : { 'X-Celoom-Request': '1' }), ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  let data: unknown;
  try { data = await response.json(); } catch { /* Handled below, never render raw HTML or credentials. */ }
  if (!response.ok) {
    if (response.status === 401) window.dispatchEvent(new Event('celoom:unauthorized'));
    const error = data as { error?: unknown; code?: string } | undefined;
    throw new ApiError(typeof error?.error === 'string' ? error.error : `请求失败（HTTP ${response.status}）`, response.status, error?.code);
  }
  if (data === undefined) throw new ApiError('服务器返回了无法解析的响应，请重试。', response.status);
  return data as T;
}
export const api = {
  bootstrap: () => request<Bootstrap>('/bootstrap'),
  auth: (mode: 'setup' | 'login', password: string) => request(`/${mode}`, 'POST', { password }),
  logout: () => request('/logout', 'POST'),
  password: (current_password: string, new_password: string) => request('/password', 'POST', { current_password, new_password }),
  config: () => request<AppConfig>('/config'),
  saveConfig: (config: AppConfig) => request<AppConfig>('/config', 'PUT', config),
  diagnose: (selection: Selection) => request<Diagnostic>('/network/test', 'POST', { selection }),
  qualityPlan: (input: QualityCheckInput, signal?: AbortSignal) => request<QualityPlan>('/model-checks/plan', 'POST', input, signal),
  startQuality: (input: QualityCheckInput) => request<QualityRun>('/model-checks', 'POST', input),
  qualityRuns: (selection: Selection, signal?: AbortSignal) => request<QualityRun[]>(`/model-checks?${new URLSearchParams({ site_id: selection.site_id, key_id: selection.key_id, model_id: selection.model_id })}`, 'GET', undefined, signal),
  qualityRun: (id: string, signal?: AbortSignal) => request<QualityRun>(`/model-checks/${encodeURIComponent(id)}`, 'GET', undefined, signal),
  cancelQuality: (id: string) => request<QualityRun>(`/model-checks/${encodeURIComponent(id)}/cancel`, 'POST', {}),
  deleteSession: (id: string) => request<{ deleted: boolean }>(`/sessions/${encodeURIComponent(id)}`, 'DELETE'),
  renameSession: (id: string, title: string) => request<Session>(`/sessions/${encodeURIComponent(id)}`, 'PATCH', { title }),
  setSessionEncryption: (id: string, mode: 'none' | 'half' | 'full', password?: string) => request<Session>(`/sessions/${encodeURIComponent(id)}/encryption`, 'POST', password === undefined ? { mode } : { mode, password }),
  unlockSession: (id: string, password: string) => request<Session>(`/sessions/${encodeURIComponent(id)}/unlock`, 'POST', { password }),
  lockSession: (id: string) => request<Session>(`/sessions/${encodeURIComponent(id)}/lock`, 'POST', {}),
  exportQuality: (id: string) => request<unknown>(`/model-checks/${encodeURIComponent(id)}/export`),
  sessions: () => request<Session[]>('/sessions'),
  createSession: (input: { title?: string; cwd?: string; selection: Selection }) => request<Session>('/sessions', 'POST', input),
  detail: (id: string, signal?: AbortSignal) => request<SessionDetail>(`/sessions/${encodeURIComponent(id)}`, 'GET', undefined, signal),
  message: (id: string, input: MessageInput, signal?: AbortSignal) => request<{ run_id: string }>(`/sessions/${encodeURIComponent(id)}/messages`, 'POST', input, signal),
  interruptMessage: (id: string, client_message_id: string, signal?: AbortSignal) => request<InterruptionResult>(`/sessions/${encodeURIComponent(id)}/messages/interrupt`, 'POST', { client_message_id }, signal),
  sessionSettings: (id: string, settings: SessionSettings) => request<Session>(`/sessions/${encodeURIComponent(id)}/settings`, 'PATCH', settings),
  files: (id: string, path = '', signal?: AbortSignal) => request<FileListing>(`/sessions/${encodeURIComponent(id)}/files?path=${encodeURIComponent(path)}`, 'GET', undefined, signal),
  questions: (id: string, signal?: AbortSignal) => request<Question[]>(`/sessions/${encodeURIComponent(id)}/questions`, 'GET', undefined, signal),
  answerQuestion: (id: string, questionId: string, answer: string) => request<Question>(`/sessions/${encodeURIComponent(id)}/questions/${encodeURIComponent(questionId)}/answer`, 'POST', { answer }),
  goals: (id: string, signal?: AbortSignal) => request<Goal[]>(`/sessions/${encodeURIComponent(id)}/goals`, 'GET', undefined, signal),
  createGoal: (id: string, input: { title: string; objective: string }) => request<Goal>(`/sessions/${encodeURIComponent(id)}/goals`, 'POST', input),
  changeGoal: (id: string, goalId: string, action: GoalAction) => request<Goal>(`/sessions/${encodeURIComponent(id)}/goals/${encodeURIComponent(goalId)}`, 'PATCH', { action }),
  computer: (signal?: AbortSignal) => request<ComputerStatus>('/computer', 'GET', undefined, signal),
  stopComputer: () => request<{ ok: true }>('/computer/stop', 'POST'),
  computerHost: (enabled: boolean) => request<unknown>('/computer/host', 'POST', { enabled }),
  computerCookies: (signal?: AbortSignal) => request<{ count: number; domains: string[] }>('/computer/cookies', 'GET', undefined, signal),
  importComputerCookies: (input: { cookies_json?: string; cookie_header?: string; url?: string }) => request<{ ok: true; count: number; domains: string[] }>('/computer/cookies', 'POST', input),
  clearComputerCookies: () => request<{ ok: true }>('/computer/cookies', 'DELETE'),
  runtimeOverview: (sessionId?: string | null, signal?: AbortSignal) => request<RuntimeOverview>(`/runtime/overview${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, 'GET', undefined, signal),
  resumeSubagent: (id: string) => request<{ ok: true; id: string; run_id: string }>(`/subagents/${encodeURIComponent(id)}/resume`, 'POST'),
  cancelSubagent: (id: string) => request<{ ok: true; id: string }>(`/subagents/${encodeURIComponent(id)}/cancel`, 'POST'),
  cancelAllSubagents: () => request<{ ok: true; cancelled: number }>('/subagents/cancel-all', 'POST'),
  resumeLastSubagents: (sessionId?: string | null) => request<{ ok: true; resumed: string[] }>(`/subagents/resume-last${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ''}`, 'POST'),
  cancel: (id: string) => request<{ ok: true }>(`/sessions/${encodeURIComponent(id)}/cancel`, 'POST'),
  compact: (id: string) => request<{ run_id: string }>(`/sessions/${encodeURIComponent(id)}/compact`, 'POST'),
};
export const filePreviewUrl = (id: string, path: string) => `/api/sessions/${encodeURIComponent(id)}/file?path=${encodeURIComponent(path)}`;
export const computerImageUrl = (id: string, captureId: string) => `/api/sessions/${encodeURIComponent(id)}/computer/image/${encodeURIComponent(captureId)}`;
export const errorMessage = (error: unknown) => error instanceof Error ? error.message : '操作未完成，请稍后重试。';

// A response must never repopulate an input with a secret, even if an older backend returns one.
export function sanitizeConfig(config: AppConfig): AppConfig {
  const { api_key: _apiKey, ...search } = { ...defaultSearchConfig(), ...config.search };
  // Pool entries are references only; never retain accidentally returned credentials.
  const subagent_models = (config.subagent_models ?? []).map(({ id, name, selection, reasoning_effort }) => ({
    id, name, selection: { site_id: selection.site_id, key_id: selection.key_id, model_id: selection.model_id }, reasoning_effort,
  }));
  return { ...config, search, subagent_models, sites: config.sites.map(site => ({ ...site, keys: site.keys.map(key => {
    const { secret: _secret, ...safe } = key;
    return { ...safe, models: safe.models.map(model => ({ ...model, omit_tool_strict: model.omit_tool_strict !== false })) };
  }) })) };
}
