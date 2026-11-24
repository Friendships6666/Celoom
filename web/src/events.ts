import type { AgentEvent, AssistantSource, ComputerActivity, DeliveryMode, FileChangeKind, Goal, ImageInput, Question, Session, SubagentSource, TurnFile } from './types';
import { computerActivity, safeComputerText } from './computer';

export type TimelineItem =
  | { kind: 'user'; key: string; text: string; images: ImageInput[]; time: string; clientId?: string; queued?: boolean; deliveryMode?: DeliveryMode; interrupted?: boolean }
  | { kind: 'assistant'; key: string; text: string; reasoning?: string; provisional: boolean; source?: AssistantSource; failed?: string; time: string }
  | { kind: 'tool'; key: string; sessionId: string; name: string; args: unknown; output?: string; capture?: ComputerActivity; error: boolean; finished: boolean; time: string }
  | { kind: 'subagent'; key: string; id: string; task: string; status: string; source?: SubagentSource; time: string }
  | { kind: 'files'; key: string; runId: string; files: TurnFile[]; time: string }
  | { kind: 'notice'; key: string; text: string; tone: 'error' | 'info' | 'warning'; time: string };

const str = (value: unknown) => typeof value === 'string' ? value : '';
const isRecord = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value);
const record = (value: unknown): Record<string, unknown> => {
  if (typeof value === 'string') { try { value = JSON.parse(value); } catch { return {}; } }
  return isRecord(value) ? value : {};
};
function mergeSubagentSource(previous: SubagentSource | undefined, data: Record<string, unknown>): SubagentSource | undefined {
  const source: SubagentSource = { ...previous };
  for (const key of ['site_name', 'model_name', 'model'] as const) if (!source[key] && str(data[key])) source[key] = str(data[key]);
  for (const key of ['reasoning_effort', 'model_id'] as const) if (source[key] === undefined && (data[key] === null || typeof data[key] === 'string')) source[key] = data[key];
  return Object.keys(source).length ? source : undefined;
}
export const isRelativeFilePath = (path: string) => !!path && !path.startsWith('/') && !path.includes('\\') && !path.split('/').some(part => !part || part === '.' || part === '..') && !/^[a-z]:/i.test(path) && !/[\u0000-\u001f]/.test(path);
const changeKind = (kind: unknown): kind is FileChangeKind => kind === 'new' || kind === 'modified' || kind === 'deleted';
export function turnFiles(value: unknown): TurnFile[] {
  return Array.isArray(value) ? value.filter(isRecord).filter(file => isRelativeFilePath(str(file.path)) && changeKind(file.kind)).map(file => ({ path: str(file.path), name: str(file.name) || str(file.path).split('/').at(-1)!, kind: file.kind as FileChangeKind })) : [];
}
export function mergeEvents(current: AgentEvent[], incoming: AgentEvent[]): AgentEvent[] {
  if (incoming.length === 1 && current.length > 0) {
    const ev = incoming[0];
    if (Number.isSafeInteger(ev.seq) && ev.seq > current[current.length - 1].seq) {
      return [...current, ev];
    }
  }
  const bySeq = new Map(current.map(event => [event.seq, event]));
  for (const event of incoming) if (Number.isSafeInteger(event.seq) && !bySeq.has(event.seq)) bySeq.set(event.seq, event);
  return [...bySeq.values()].sort((a, b) => a.seq - b.seq);
}
// Selection events update the live controls only; assistant provenance stays on its attempt.
export function sessionFromEvent(current: Session, event: AgentEvent): Session {
  const d = event.data;
  if (event.type === 'status' && typeof d.status === 'string') {
    // A normal turn completion cannot release a live blocking-question gate.
    if (current.status === 'waiting_for_answer' && d.status === 'completed') return current;
    return { ...current, status: d.status as Session['status'], updated_at: event.created_at };
  }
  if (event.type === 'compacted' && typeof d.context_epoch === 'number') return { ...current, context_epoch: d.context_epoch };
  if (event.type === 'session_settings' && isRecord(d.selection) && ['site_id', 'key_id', 'model_id'].every(key => typeof (d.selection as Record<string, unknown>)[key] === 'string') && (d.reasoning_effort === null || typeof d.reasoning_effort === 'string')) {
    return { ...current, selection: { site_id: str(d.selection.site_id), key_id: str(d.selection.key_id), model_id: str(d.selection.model_id) }, reasoning_effort: d.reasoning_effort, updated_at: event.created_at };
  }
  if (event.type === 'activator' && typeof d.enabled === 'boolean') {
    return { ...current, activator_enabled: d.enabled, activator_prompt: typeof d.prompt === 'string' ? d.prompt : (current.activator_prompt ?? ''), updated_at: event.created_at };
  }
  return current;
}
export function timeline(events: AgentEvent[]): TimelineItem[] {
  const items: TimelineItem[] = [];
  const users = new Map<string, Extract<TimelineItem, { kind: 'user' }>>();
  const delivered = new Set(events.filter(event => event.type === 'message_delivered').map(event => `${event.session_id}\0${str(event.data.client_message_id)}`));
  const interrupted = new Set(events.filter(event => event.type === 'message_interrupted').map(event => `${event.session_id}\0${str(event.data.client_message_id)}`));
  const attempts = new Map<string, Extract<TimelineItem, { kind: 'assistant' }>>();
  const tools = new Map<string, Extract<TimelineItem, { kind: 'tool' }>>();
  const children = new Map<string, Extract<TimelineItem, { kind: 'subagent' }>>();
  const rounds = new Map<string, Extract<TimelineItem, { kind: 'files' }>>();
  function child(data: Record<string, unknown>, key: string, time: string) {
    const id = str(data.id); if (!id) return;
    const previous = children.get(id);
    if (previous) {
      if (str(data.status)) previous.status = str(data.status);
      if (str(data.task)) previous.task = str(data.task);
      previous.source = mergeSubagentSource(previous.source, data);
    } else {
      const item: Extract<TimelineItem, { kind: 'subagent' }> = { kind: 'subagent', key: id || key, id, task: str(data.task), status: str(data.status), source: mergeSubagentSource(undefined, data), time };
      children.set(id, item); items.push(item);
    }
  }
  for (const event of mergeEvents([], events)) {
    const d = event.data;
    const key = String(event.seq);
    const time = event.created_at;
    switch (event.type) {
      case 'user_message': {
        const clientId = str(d.client_message_id); const identity = `${event.session_id}\0${clientId}`;
        if (clientId && users.has(identity)) break;
        const item: Extract<TimelineItem, { kind: 'user' }> = { kind: 'user', key: clientId || key, text: str(d.text), images: Array.isArray(d.images) ? d.images as ImageInput[] : [], time, deliveryMode: d.delivery_mode === 'immediate' || interrupted.has(identity) ? 'immediate' : 'after_call', ...(clientId ? { clientId } : {}), ...(d.interrupted === true || interrupted.has(identity) ? { interrupted: true } : {}), ...(d.queued === true ? { queued: !delivered.has(identity) } : {}) };
        if (clientId) users.set(identity, item); items.push(item); break;
      }
      case 'assistant_source':
      case 'assistant_reasoning_delta':
      case 'assistant_delta':
      case 'assistant_message': {
        const id = str(d.attempt_id) || str(d.id) || key;
        let item = attempts.get(id);
        if (!item) { item = { kind: 'assistant', key: id, text: '', provisional: true, time }; attempts.set(id, item); items.push(item); }
        if (event.type === 'assistant_source') {
          // The first actual request source wins, even when route settings change later.
          item.source ??= { attempt_id: id, site_name: str(d.site_name), model_name: str(d.model_name), model: str(d.model), reasoning_effort: typeof d.reasoning_effort === 'string' ? d.reasoning_effort : null };
        } else if (event.type === 'assistant_message') {
          item.text = str(d.text);
          if (typeof d.reasoning === 'string' && d.reasoning) item.reasoning = d.reasoning;
          item.provisional = false;
        } else if (event.type === 'assistant_reasoning_delta') {
          if (item.provisional) item.reasoning = (item.reasoning ?? '') + str(d.text);
        } else if (item.provisional) item.text += str(d.text);
        break;
      }
      case 'attempt_failed': {
        const item = attempts.get(str(d.attempt_id));
        if (item) { item.failed = str(d.message); item.provisional = false; }
        else items.push({ kind: 'notice', key, text: str(d.message) || '生成尝试失败', tone: 'warning', time });
        break;
      }
      case 'tool_start': {
        const id = str(d.call_id);
        const item: Extract<TimelineItem, { kind: 'tool' }> = { kind: 'tool', key: id, sessionId: event.session_id, name: str(d.name), args: d.args, finished: false, error: false, time };
        tools.set(id, item); items.push(item); break;
      }
      case 'tool_output':
      case 'tool_end': {
        const item = tools.get(str(d.call_id));
        if (item && item.sessionId === event.session_id) {
          const computer = item.name.replace(/^functions\./, '') === 'computer';
          item.output = computer ? safeComputerText(str(d.output ?? d.text)) : str(d.output ?? d.text);
          if (event.type === 'tool_end') {
            item.finished = true; item.error = d.is_error === true;
            if (!item.error && item.name.replace(/^functions\./, '') === 'subagent') {
              const args = record(item.args); const result = record(d.output ?? d.text);
              if (str(args.action) === 'spawn' && str(result.id)) child({ ...result, task: str(result.task) || str(args.task) }, key, time);
            }
            if (computer && typeof d.capture_id === 'string') item.capture = computerActivity({ ...event, type: 'computer', data: { action: 'screenshot', target: 'computer', summary: '', is_error: d.is_error === true, ...d } }) ?? item.capture;
          }
        }
        break;
      }
      case 'computer': {
        const activity = computerActivity(event);
        if (activity) {
          // A computer event is emitted during its serialized tool action. Do not
          // attach a capture to an older finished call or a different session.
          const item = [...tools.values()].reverse().find(tool => tool.sessionId === event.session_id && tool.name.replace(/^functions\./, '') === 'computer' && !tool.finished);
          if (item) item.capture = activity;
        }
        break;
      }
      case 'subagent': { child(d, key, time); break; }
      case 'turn_files': {
        const runId = str(d.run_id) || key;
        const previous = rounds.get(runId);
        if (previous) previous.files = turnFiles(d.files);
        else { const item: Extract<TimelineItem, { kind: 'files' }> = { kind: 'files', key, runId, files: turnFiles(d.files), time }; rounds.set(runId, item); items.push(item); }
        break;
      }
      case 'network_wait': items.push({ kind: 'notice', key, text: '网络暂不可用 · 自动等待恢复，可继续发消息或停止', tone: 'warning', time }); break;
      case 'network_restored': items.push({ kind: 'notice', key, text: '网络已恢复 · 正从已保存的进度继续', tone: 'info', time }); break;
      case 'error': items.push({ kind: 'notice', key, text: str(d.message), tone: 'error', time }); break;
      case 'context_warning': items.push({ kind: 'notice', key, text: str(d.message), tone: 'warning', time }); break;
      case 'retry': items.push({ kind: 'notice', key, text: `准备第 ${Number(d.attempt)} 次尝试 · ${Math.round(Number(d.delay_ms) / 1000)} 秒后重试${str(d.error) ? ` · ${str(d.error)}` : ''}`, tone: 'warning', time }); break;
      case 'compacted': items.push({ kind: 'notice', key, text: `上下文已压缩${str(d.summary) ? `\n${str(d.summary)}` : ''}`, tone: 'info', time }); break;
    }
  }
  return items;
}
export function assistantLabel(source?: AssistantSource, fallback?: { site: { name: string }; model: { name: string; model: string } }): string {
  return source ? `${source.site_name || '未知站点'} - ${source.model_name || source.model || '未知模型'}` : fallback ? `${fallback.site.name} - ${fallback.model.name || fallback.model.model}` : '站点 - 模型未记录';
}
export function subagentLabel(source?: SubagentSource): string {
  return [source?.site_name, source?.model_name || source?.model].filter(Boolean).join(' - ');
}
// Keep the exact first command line, including indentation and shell syntax.
export function toolSummary(name: string, args: unknown): string {
  let value = args;
  if (typeof args === 'string') { try { value = JSON.parse(args); } catch { /* A shell command can be a plain string. */ } }
  const a = isRecord(value) ? value : {};
  const first = (...keys: string[]) => keys.map(key => str(a[key])).find(Boolean) || '';
  const tool = name.replace(/^functions\./, '');
  if (tool === 'bash') return (first('command', 'cmd') || (typeof value === 'string' ? value : '')).split(/\r?\n/).find(line => line.trim()) || '';
  if (['read', 'write', 'edit', 'read_image'].includes(tool)) return first('file_path', 'path');
  if (tool === 'ask_user') return [first('action'), first('question', 'id')].filter(Boolean).join(' · ');
  if (tool === 'goal') return [first('action'), first('title', 'objective', 'progress', 'id')].filter(Boolean).join(' · ');
  if (tool === 'computer') {
    const argv = Array.isArray(a.argv) ? a.argv.filter((value): value is string => typeof value === 'string').join(' ') : '';
    return safeComputerText([first('action'), first('target') || 'browser', first('ref_id', 'ref', 'url') || argv].filter(Boolean).join(' · ').split(/\r?\n/)[0]);
  }
  if (tool === 'web_search') return first('query');
  if (tool === 'web_fetch') return first('url');
  if (tool === 'dag') {
    const nodes = Array.isArray(a.nodes) ? a.nodes : [];
    const ids = nodes.map(n => (isRecord(n) ? str(n.id) : '')).filter(Boolean);
    return nodes.length ? `${nodes.length} 节点流水线${ids.length ? ` · ${ids.slice(0, 4).join(' → ')}${ids.length > 4 ? '…' : ''}` : ''}` : 'DAG 分支任务图';
  }
  if (tool === 'grep') return first('pattern', 'query');
  if (tool === 'glob') return first('pattern');
  if (tool === 'job' || tool.startsWith('job_')) return [first('action') || tool.slice(4), first('job_id', 'id')].filter(Boolean).join(' · ');
  if (tool.includes('subagent')) {
    const action = first('action');
    if (action === 'models') return '查看可用子代理模型';
    const task = first('description', 'task', 'prompt').split(/\r?\n/).find(line => line.trim()) || '';
    if (action === 'spawn') return task || '启动子代理';
    return task || [action, first('id')].filter(Boolean).join(' · ');
  }
  return first('path', 'query', 'action', 'description');
}
export interface FileMarker { status?: 'editing' | 'new' | 'modified' | 'deleted'; isNew: boolean }
export function fileMarkers(events: AgentEvent[]): Record<string, FileMarker> {
  const markers: Record<string, FileMarker> = {};
  for (const event of mergeEvents([], events)) {
    const d = event.data; const path = str(d.path);
    if (event.type === 'user_message' && d.queued !== true) { for (const key of Object.keys(markers)) delete markers[key]; }
    else if (event.type === 'file_activity' && isRelativeFilePath(path)) {
      const isNew = d.is_new === true || markers[path]?.isNew === true;
      markers[path] = { status: d.status === 'editing' ? 'editing' : isNew ? 'new' : 'modified', isNew };
    } else if (event.type === 'file_changed' && isRelativeFilePath(path) && changeKind(d.kind)) {
      const isNew = d.kind === 'new' || (d.kind !== 'deleted' && markers[path]?.isNew === true);
      markers[path] = { status: d.kind === 'deleted' ? 'deleted' : markers[path]?.status === 'editing' ? 'editing' : isNew ? 'new' : 'modified', isNew };
    } else if (event.type === 'turn_files') {
      for (const file of turnFiles(d.files)) markers[file.path] = { status: file.kind, isNew: file.kind === 'new' };
    }
  }
  return markers;
}
export function latestSequence(events: AgentEvent[]): number { return events.reduce((max, e) => Math.max(max, e.seq), 0); }

// GET, SSE replay and mutation responses can arrive in any order. Never regress
// durable terminal records, including snapshots with equal timestamp precision.
export function mergeSnapshots<T extends Question | Goal>(current: T[], incoming: T[]): T[] {
  const records = new Map(current.map(item => [item.id, item]));
  for (const item of incoming) {
    const previous = records.get(item.id);
    const terminal = (status: string) => ['answered', 'withdrawn', 'completed', 'cancelled'].includes(status);
    if (!previous || (item.updated_at >= previous.updated_at && !(terminal(previous.status) && !terminal(item.status)))) records.set(item.id, item);
  }
  return [...records.values()].sort((a, b) => b.created_at.localeCompare(a.created_at) || b.id.localeCompare(a.id));
}
export function questionSnapshot(event: AgentEvent): Question | null {
  const q = event.data.question;
  return ['question', 'question_answer', 'question_withdrawn'].includes(event.type) && isRecord(q) && typeof q.id === 'string' && q.session_id === event.session_id && typeof q.question === 'string' && typeof q.blocking === 'boolean' && Array.isArray(q.options) && q.options.every(option => typeof option === 'string') && ['pending', 'answered', 'withdrawn'].includes(str(q.status)) && typeof q.updated_at === 'string' && typeof q.created_at === 'string' ? q as unknown as Question : null;
}
export function goalSnapshot(event: AgentEvent): Goal | null {
  const g = event.data.goal;
  return event.type === 'goal' && isRecord(g) && typeof g.id === 'string' && g.session_id === event.session_id && typeof g.title === 'string' && typeof g.objective === 'string' && typeof g.progress === 'string' && typeof g.rounds === 'number' && ['pending', 'active', 'paused', 'completed', 'cancelled'].includes(str(g.status)) && typeof g.updated_at === 'string' && typeof g.created_at === 'string' ? g as unknown as Goal : null;
}
