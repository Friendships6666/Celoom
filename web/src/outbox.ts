import { api, ApiError } from './api';
import { validateMessage } from './messageInput';
import type { DeliveryMode, InterruptionResult, MessageInput } from './types';

export type OutboxFailure = 'network' | 'auth' | 'validation' | 'server';
export interface OutboxEntry {
  key: string; origin: string; session_id: string; client_message_id: string;
  text: string; images: NonNullable<MessageInput['images']>; delivery_mode: DeliveryMode; created_at: number;
  status: 'pending' | 'sending' | 'failed'; attempts: number; failure?: OutboxFailure;
  // Acceptance alone does not acknowledge an upgrade of an older in-flight payload.
  escalation_pending?: boolean; accepted?: boolean; escalation_attempts?: number; interruption_only?: boolean;
}
const DB_NAME = 'celoom-message-outbox';
const STORE = 'messages';
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
export const outboxKey = (origin: string, session: string, id: string) => `${origin}\0${session}\0${id}`;
export function outboxPayload(entry: OutboxEntry): MessageInput {
  if (entry.interruption_only) throw new Error('强制介入请求不能作为用户消息发送。');
  return { text: entry.text, images: entry.images, client_message_id: entry.client_message_id, delivery_mode: entry.delivery_mode };
}
// Reconstruct a strict allowlist: config, credentials, cookies and arbitrary fields
// can never enter IndexedDB, even if a caller passes an over-wide object.
export function safeEntry(value: unknown): OutboxEntry | null {
  if (!value || typeof value !== 'object') return null;
  const v = value as Record<string, unknown>;
  if (typeof v.origin !== 'string' || typeof v.session_id !== 'string' || !v.session_id || v.session_id.length > 512 || typeof v.client_message_id !== 'string' || !uuid.test(v.client_message_id) || typeof v.text !== 'string' || !Array.isArray(v.images) || !['after_call', 'immediate'].includes(String(v.delivery_mode)) || typeof v.created_at !== 'number' || !Number.isFinite(v.created_at) || !['pending', 'sending', 'failed'].includes(String(v.status)) || !Number.isSafeInteger(v.attempts) || Number(v.attempts) < 0) return null;
  let origin: string;
  try { origin = new URL(v.origin).origin; } catch { return null; }
  if (origin !== v.origin || !/^https?:/.test(origin)) return null;
  const images = [];
  for (const image of v.images) {
    if (!image || typeof image !== 'object' || typeof image.mime !== 'string' || typeof image.data !== 'string') return null;
    images.push({ mime: image.mime, data: image.data });
  }
  const entry: OutboxEntry = { key: outboxKey(origin, v.session_id, v.client_message_id), origin, session_id: v.session_id, client_message_id: v.client_message_id, text: v.text, images, delivery_mode: v.delivery_mode as DeliveryMode, created_at: v.created_at, status: v.status as OutboxEntry['status'], attempts: Number(v.attempts) };
  if (v.escalation_pending === true) { entry.escalation_pending = true; entry.delivery_mode = 'immediate'; }
  if (v.accepted === true) entry.accepted = true;
  if (Number.isSafeInteger(v.escalation_attempts) && Number(v.escalation_attempts) >= 0) entry.escalation_attempts = Number(v.escalation_attempts);
  if (['network', 'auth', 'validation', 'server'].includes(String(v.failure))) entry.failure = v.failure as OutboxFailure;
  if (v.interruption_only === true) {
    if (v.accepted !== true || v.escalation_pending !== true || v.delivery_mode !== 'immediate' || images.length) return null;
    // This is a local description, never a recovered or newly submitted body.
    entry.interruption_only = true; entry.text = '强制介入请求待联网'; return entry;
  }
  try { validateMessage(outboxPayload(entry)); } catch { return null; }
  return entry;
}
function database(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    if (typeof indexedDB === 'undefined') { reject(new Error('浏览器不支持本地可靠保存，消息尚未发送。')); return; }
    let request: IDBOpenDBRequest;
    try { request = indexedDB.open(DB_NAME, 1); }
    catch { reject(new Error('本地消息保存失败，请检查浏览器存储权限；输入已保留。')); return; }
    request.onupgradeneeded = () => { request.result.createObjectStore(STORE, { keyPath: 'key' }); };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(new Error('本地消息保存失败，请检查浏览器存储空间；输入已保留。'));
    request.onblocked = () => reject(new Error('本地消息存储被其他页面占用，请关闭旧页面后重试。'));
  });
}
async function transaction<T>(mode: IDBTransactionMode, operation: (store: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  const db = await database();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE, mode);
    const request = operation(tx.objectStore(STORE));
    tx.oncomplete = () => { db.close(); resolve(request.result); };
    tx.onabort = tx.onerror = () => { db.close(); reject(new Error('本地消息保存失败；消息未丢弃，请检查浏览器存储空间。')); };
  });
}
export const outboxStore = {
  async list(origin: string, sessionId: string) {
    const values = await transaction('readonly', store => store.getAll());
    return values.map(safeEntry).filter((entry): entry is OutboxEntry => !!entry && entry.origin === origin && entry.session_id === sessionId).sort((a, b) => a.created_at - b.created_at || a.key.localeCompare(b.key));
  },
  async put(entry: OutboxEntry) {
    const safe = safeEntry(entry); if (!safe) throw new Error('消息格式无效，未保存或发送。');
    await transaction('readwrite', store => store.put(safe));
  },
  async remove(key: string) { await transaction('readwrite', store => store.delete(key)); },
};
export function retryable(reason: unknown) {
  return !(reason instanceof ApiError) || [408, 429, 502, 503, 504].includes(reason.status) || (reason.status >= 200 && reason.status < 300);
}
export const failureLabels: Record<OutboxFailure, string> = { network: '网络暂不可用', auth: '请重新登录后手动重试', validation: '消息未通过验证，请丢弃后修改', server: '服务器未确认操作，请检查后重试' };

// One in-flight sender per origin/session, shared by remounts. HTTP timeouts and
// lost responses keep the same saved UUID; SSE acceptance can settle them too.
export class DurableOutbox {
  entries: OutboxEntry[] = [];
  error = '';
  private listeners = new Set<() => void>();
  private loaded?: Promise<void>;
  private busy = false;
  private pausedAuth = false;
  private timer?: ReturnType<typeof setTimeout>;
  private confirmed = new Set<string>();
  private cleaned = new Set<string>();
  private upgraded = new Set<string>();
  private delivered = new Set<string>();
  private mutations: Promise<unknown> = Promise.resolve();
  private interruptions = new Map<string, Promise<InterruptionResult>>();
  private online() { return typeof navigator === 'undefined' || navigator.onLine; }
  // Serialize commits, not HTTP: Esc must overtake a still-running old send.
  private mutate<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.mutations.then(operation);
    this.mutations = result.catch(() => {}); return result;
  }
  private async remove(entry: OutboxEntry) {
    await outboxStore.remove(entry.key); this.cleaned.add(entry.client_message_id);
    this.entries = this.entries.filter(item => item !== entry); this.emit();
  }
  private async failed(entry: OutboxEntry, reason: unknown, escalation = false) {
    const auth = reason instanceof ApiError && [401, 403].includes(reason.status);
    const canRetry = retryable(reason);
    entry.failure = auth ? 'auth' : reason instanceof ApiError && [400, 413, 422].includes(reason.status) ? 'validation' : canRetry ? 'network' : 'server';
    entry.status = canRetry && (escalation ? entry.escalation_attempts ?? 0 : entry.attempts) < 5 ? 'pending' : 'failed';
    if (auth) this.pausedAuth = true;
    this.error = reason instanceof Error ? reason.message : '请求未确认，请重试。';
    await outboxStore.put(entry); this.emit();
    if (entry.status === 'pending') this.schedule(entry);
  }
  private schedule(entry: OutboxEntry) {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => void this.flush(), Math.min(15_000, 1000 * 2 ** Math.max(0, (entry.escalation_pending ? entry.escalation_attempts ?? 1 : entry.attempts) - 1)));
  }
  constructor(readonly origin: string, readonly sessionId: string) {}
  private emit() { for (const listener of this.listeners) listener(); }
  subscribe(listener: () => void) { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; }
  async load() {
    this.loaded ??= outboxStore.list(this.origin, this.sessionId).then(entries => {
      this.entries = entries.map(entry => ({ ...entry, status: entry.status === 'sending' ? 'pending' as const : entry.status }));
      this.pausedAuth = this.entries.some(entry => entry.failure === 'auth'); this.error = ''; this.emit();
    }).catch(reason => { this.loaded = undefined; this.error = reason instanceof Error ? reason.message : '本地消息读取失败'; this.emit(); throw reason; });
    await this.loaded;
  }
  async enqueue(payload: MessageInput) {
    await this.load(); validateMessage(payload);
    const entry = safeEntry({ ...payload, images: payload.images ?? [], delivery_mode: payload.delivery_mode ?? 'after_call', origin: this.origin, session_id: this.sessionId, created_at: Date.now(), status: 'pending', attempts: 0 });
    if (!entry) throw new Error('消息格式无效，未保存或发送。');
    await this.mutate(async () => {
      if (this.confirmed.has(entry.client_message_id) || this.entries.some(item => item.client_message_id === entry.client_message_id)) return;
      await outboxStore.put(entry); // The composer must not clear before this commits.
      this.entries = [...this.entries, entry]; this.error = ''; this.emit();
    });
    void this.flush();
  }
  async confirm(ids: string[], upgradedIds: string[] = [], deliveredIds: string[] = []) {
    deliveredIds.forEach(id => this.delivered.add(id));
    ids.forEach(id => this.confirmed.add(id));
    upgradedIds.forEach(id => { this.confirmed.add(id); this.upgraded.add(id); });
    await this.load();
    await this.mutate(async () => {
      for (const id of new Set([...ids, ...upgradedIds])) {
        if (this.cleaned.has(id)) continue;
        const entry = this.entries.find(item => item.client_message_id === id);
        try {
          if (entry?.escalation_pending && !this.upgraded.has(id)) {
            if (!entry.accepted || entry.status === 'sending') {
              entry.accepted = true;
              if (entry.status === 'sending') entry.status = 'pending';
              await outboxStore.put(entry); this.schedule(entry);
            }
          } else if (entry) await this.remove(entry);
          else { await outboxStore.remove(outboxKey(this.origin, this.sessionId, id)); this.cleaned.add(id); }
        } catch { this.error = '已确认收件，但本地记录清理失败；重试仍将复用请求 ID。'; }
      }
      this.emit();
    });
    // Only new acceptance schedules a recheck. Replayed user events on each
    // streaming delta must not continually postpone the interrupt retry timer.
  }
  async escalate(id: string): Promise<InterruptionResult> {
    if (!uuid.test(id)) throw new Error('消息请求 ID 无效。');
    await this.load();
    await this.mutate(async () => {
      const entry = this.entries.find(item => item.client_message_id === id);
      if (!entry) {
        const intent = safeEntry({ origin: this.origin, session_id: this.sessionId, client_message_id: id, text: '', images: [], delivery_mode: 'immediate', created_at: Date.now(), status: 'pending', attempts: 0, accepted: true, escalation_pending: true, interruption_only: true });
        if (!intent) throw new Error('强制介入请求格式无效。');
        await outboxStore.put(intent);
        this.cleaned.delete(id); this.confirmed.add(id); this.entries = [...this.entries, intent];
        this.pausedAuth = false; this.error = ''; this.emit(); return;
      }
      const updated: OutboxEntry = { ...entry, delivery_mode: 'immediate', escalation_pending: true, escalation_attempts: 0, status: entry.status === 'sending' ? 'sending' : 'pending' };
      delete updated.failure;
      // Must commit the new mode AND intent before publishing or requesting it.
      await outboxStore.put(updated);
      Object.assign(entry, updated); delete entry.failure;
      this.pausedAuth = false; this.error = ''; this.emit();
    });
    if (!this.online()) return { client_message_id: id, promoted: false, delivered: false, pending_acceptance: true };
    const result = await this.interrupt(id);
    void this.flush(); return result;
  }
  private interrupt(id: string): Promise<InterruptionResult> {
    const active = this.interruptions.get(id); if (active) return active;
    const request = (async () => {
      await this.mutate(async () => {
        const entry = this.entries.find(item => item.client_message_id === id);
        if (entry) { entry.escalation_attempts = (entry.escalation_attempts ?? 0) + 1; await outboxStore.put(entry); }
      });
      const controller = new AbortController(); const timeout = setTimeout(() => controller.abort(), 30_000);
      try {
        const result = await api.interruptMessage(this.sessionId, id, controller.signal);
        // Malformed success/lost acknowledgement must not discard durable intent.
        if (result.client_message_id !== id || ![result.promoted, result.delivered, result.pending_acceptance].every(value => typeof value === 'boolean') || !(result.promoted || result.delivered || result.pending_acceptance)) throw new ApiError('服务器尚未确认强制介入，请重试。', 202);
        if (result.promoted || result.delivered) await this.confirm([id], [id], result.delivered ? [id] : []);
        else await this.mutate(async () => {
          const entry = this.entries.find(item => item.client_message_id === id);
          if (!entry) return;
          entry.status = (entry.escalation_attempts ?? 0) < 5 ? 'pending' : 'failed';
          if (entry.status === 'failed') entry.failure = 'server';
          await outboxStore.put(entry); this.emit(); this.schedule(entry);
        });
        return this.upgraded.has(id) ? { client_message_id: id, promoted: !this.delivered.has(id), delivered: this.delivered.has(id), pending_acceptance: false } : result;
      } catch (reason) {
        if (this.upgraded.has(id)) return { client_message_id: id, promoted: !this.delivered.has(id), delivered: this.delivered.has(id), pending_acceptance: false };
        await this.mutate(async () => {
          const entry = this.entries.find(item => item.client_message_id === id);
          if (entry) await this.failed(entry, reason, true);
          else if (!this.upgraded.has(id)) { this.error = reason instanceof Error ? reason.message : '强制介入未确认'; this.emit(); }
        });
        throw reason;
      } finally { clearTimeout(timeout); }
    })();
    this.interruptions.set(id, request);
    void request.finally(() => this.interruptions.delete(id)).catch(() => {});
    return request;
  }
  async discard(id: string) {
    await this.load();
    await this.mutate(async () => {
      const entry = this.entries.find(entry => entry.client_message_id === id); if (!entry || entry.status === 'sending' || this.interruptions.has(id)) return;
      await this.remove(entry);
    });
    void this.flush();
  }
  async retry(id: string) {
    await this.load();
    await this.mutate(async () => {
      const entry = this.entries.find(entry => entry.client_message_id === id); if (!entry || entry.status === 'sending') return;
      entry.status = 'pending'; entry.attempts = 0; entry.escalation_attempts = 0; delete entry.failure; this.pausedAuth = false;
      await outboxStore.put(entry); this.error = ''; this.emit();
    });
    void this.flush();
  }
  wake() { clearTimeout(this.timer); void this.load().then(() => this.flush()).catch(() => {}); }
  async flush() {
    if (this.busy || this.pausedAuth || !this.online()) return;
    this.busy = true; clearTimeout(this.timer);
    try {
      await this.load();
      const checked = new Set<string>();
      while (!this.pausedAuth && this.online()) {
        await this.mutations;
        const entry = this.entries.find(item => item.status === 'pending' && !checked.has(item.client_message_id)); if (!entry) break;
        if (entry.escalation_pending) {
          checked.add(entry.client_message_id);
          try { await this.interrupt(entry.client_message_id); } catch (reason) {
            this.error = reason instanceof Error ? reason.message : '强制介入未确认'; this.emit();
            if (!this.pausedAuth && this.entries.some(item => item === entry && item.status === 'failed')) continue;
            break;
          }
          if (!this.entries.includes(entry)) continue;
          if (entry.accepted || entry.status === 'failed') continue;
          // pending_acceptance means the server saved the early marker. Still
          // submit the original message once, now with the saved immediate mode.
        } else if (this.confirmed.has(entry.client_message_id) || entry.accepted) { await this.confirm([entry.client_message_id]); continue; }
        entry.status = 'sending'; entry.attempts++; this.emit();
        const controller = new AbortController(); const timeout = setTimeout(() => controller.abort(), 30_000);
        try {
          await api.message(this.sessionId, outboxPayload(entry), controller.signal);
          await this.confirm([entry.client_message_id]);
          // Never treat an old after_call 202 as an upgrade acknowledgement.
          if (this.entries.some(item => item === entry && item.escalation_pending && item.status !== 'failed')) {
            checked.add(entry.client_message_id); this.schedule(entry);
          }
        } catch (reason) {
          await this.mutate(async () => {
            if (!this.entries.includes(entry) || this.confirmed.has(entry.client_message_id) || (entry.escalation_pending && entry.status === 'failed')) return;
            await this.failed(entry, reason);
          });
          if (this.entries.some(item => item === entry && item.status === 'pending')) break;
        } finally { clearTimeout(timeout); }
      }
    } catch (reason) {
      this.error = reason instanceof Error ? reason.message : '本地消息保存失败';
      // A storage error must never cause an unsaved network attempt.
      for (const entry of this.entries) if (entry.status === 'sending') entry.status = 'pending';
      this.emit();
    } finally { this.busy = false; }
  }
}
const boxes = new Map<string, DurableOutbox>();
export function sessionOutbox(origin: string, sessionId: string) {
  const key = `${origin}\0${sessionId}`;
  let box = boxes.get(key); if (!box) { box = new DurableOutbox(origin, sessionId); boxes.set(key, box); }
  return box;
}
