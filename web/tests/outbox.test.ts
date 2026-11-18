import { IDBFactory } from 'fake-indexeddb';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api, ApiError } from '../src/api';
import { DurableOutbox, outboxPayload, outboxStore, retryable, safeEntry } from '../src/outbox';
import { validateMessage, pastedFiles } from '../src/messageInput';
import { timeline } from '../src/events';
import { activeStatus, statusLabels, type AgentEvent, type InterruptionResult, type MessageInput } from '../src/types';

const origin = 'https://app.test';
const id = '17ccabf2-d256-4813-a229-f8115f570c2a';
const payload: MessageInput = { text: 'TEST FIXTURE', images: [{ mime: 'image/png', data: 'YWJj' }, { mime: 'image/jpeg', data: 'ZGVm' }], client_message_id: id, delivery_mode: 'after_call' };
const stored = () => ({ ...payload, origin, session_id: 's', created_at: 1, status: 'pending', attempts: 0 });
beforeEach(() => { vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] }); vi.stubGlobal('indexedDB', new IDBFactory()); vi.stubGlobal('navigator', { onLine: false }); });
afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); vi.useRealTimers(); });

describe('durable minimal outbox', () => {
  it('saves offline including both images, reconstructs after memory loss and binds origin/session', async () => {
    const send = vi.spyOn(api, 'message');
    const first = new DurableOutbox(origin, 's'); await first.enqueue(payload);
    expect(send).not.toHaveBeenCalled();
    const reload = new DurableOutbox(origin, 's'); await reload.load();
    expect(outboxPayload(reload.entries[0])).toEqual(payload);
    expect(await outboxStore.list('https://other.test', 's')).toEqual([]);
    expect(await outboxStore.list(origin, 'other-session')).toEqual([]);
    await reload.discard(id); expect(await outboxStore.list(origin, 's')).toEqual([]);
  });
  it('commits storage before attempting HTTP and never saves arbitrary auth/config fields', async () => {
    const safe = safeEntry({ ...stored(), api_key: 'never-store', Cookie: 'never-store', auth: { secret: 'never-store' } })!;
    expect(JSON.stringify(safe)).not.toContain('never-store');
    await outboxStore.put(safe);
    expect(Object.keys((await outboxStore.list(origin, 's'))[0]).sort()).toEqual(Object.keys(safe).sort());
    vi.stubGlobal('navigator', { onLine: true });
    const send = vi.spyOn(api, 'message').mockImplementation(async (_session, request) => {
      expect((await outboxStore.list(origin, 's'))[0]).toMatchObject({ client_message_id: request.client_message_id, status: 'pending' });
      return { run_id: 'fixture-run' };
    });
    const box = new DurableOutbox(origin, 's'); await box.load(); await box.flush();
    expect(send).toHaveBeenCalledTimes(1); expect(await outboxStore.list(origin, 's')).toEqual([]);
  });
  it('does not attempt a network request when IndexedDB fails', async () => {
    vi.stubGlobal('indexedDB', undefined); vi.stubGlobal('navigator', { onLine: true });
    const send = vi.spyOn(api, 'message');
    await expect(new DurableOutbox(origin, 's').enqueue(payload)).rejects.toThrow('不支持');
    expect(send).not.toHaveBeenCalled();
  });
  it('lost 202 retries the same immutable UUID and full images after reload', async () => {
    const first = new DurableOutbox(origin, 's'); await first.enqueue(payload);
    vi.stubGlobal('navigator', { onLine: true });
    const send = vi.spyOn(api, 'message').mockRejectedValueOnce(new TypeError('lost response')).mockResolvedValue({ run_id: 'same-run' });
    await first.flush();
    expect(first.entries[0]).toMatchObject({ status: 'pending', client_message_id: id });
    // Offline stops the old instance's timer while simulating a destroyed page.
    vi.stubGlobal('navigator', { onLine: false });
    const reload = new DurableOutbox(origin, 's'); await reload.load();
    vi.stubGlobal('navigator', { onLine: true }); await reload.flush();
    expect(send.mock.calls.map(call => call[1])).toEqual([payload, payload]);
    expect(await outboxStore.list(origin, 's')).toEqual([]);
    // Clear the obsolete test instance's retry timer without another request.
    vi.stubGlobal('navigator', { onLine: false }); first.wake();
  });
  it('SSE client-id acceptance settles a lost response without resending or duplicate timeline rows', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    const send = vi.spyOn(api, 'message'); const remove = vi.spyOn(outboxStore, 'remove'); await box.confirm([id]); await box.confirm([id]);
    expect(remove).toHaveBeenCalledTimes(1); // Streaming deltas must not repeat historical IDB deletes.
    vi.stubGlobal('navigator', { onLine: true }); await box.flush();
    expect(send).not.toHaveBeenCalled(); expect(await outboxStore.list(origin, 's')).toEqual([]);
    const event = (seq: number, type: string, data: AgentEvent['data']): AgentEvent => ({ seq, type, data, session_id: 's', created_at: '' });
    const accepted = event(1, 'user_message', { ...payload, queued: true });
    expect(timeline([accepted])[0]).toMatchObject({ queued: true });
    expect(timeline([accepted, event(2, 'user_message', { ...payload, queued: true }), event(3, 'message_delivered', { client_message_id: id })])).toHaveLength(1);
    expect(timeline([accepted, event(2, 'message_delivered', { client_message_id: id })])[0]).toMatchObject({ queued: false });
  });
  it.each([400, 401, 403, 404, 413, 500])('HTTP %s fails without automatic retry or silent deletion', async status => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    vi.stubGlobal('navigator', { onLine: true });
    const send = vi.spyOn(api, 'message').mockRejectedValue(new ApiError('fixture', status));
    await box.flush(); await box.flush();
    expect(send).toHaveBeenCalledTimes(1); expect(box.entries[0].status).toBe('failed');
    expect((await outboxStore.list(origin, 's'))[0].client_message_id).toBe(id);
    const reload = new DurableOutbox(origin, 's'); await reload.load(); await reload.flush(); expect(send).toHaveBeenCalledTimes(1);
  });
  it('auth failure pauses other pending entries, manual retry restores the unchanged UUID', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    await box.enqueue({ ...payload, client_message_id: '27ccabf2-d256-4813-a229-f8115f570c2a' });
    vi.stubGlobal('navigator', { onLine: true });
    const send = vi.spyOn(api, 'message').mockRejectedValueOnce(new ApiError('fixture', 401)).mockResolvedValue({ run_id: 'fixture' });
    await box.flush(); await box.flush(); expect(send).toHaveBeenCalledTimes(1);
    vi.stubGlobal('navigator', { onLine: false }); await box.retry(id);
    vi.stubGlobal('navigator', { onLine: true }); await box.flush();
    expect(send.mock.calls[1][1].client_message_id).toBe(id); expect(box.entries).toEqual([]);
  });
  it('serializes concurrent flushes and retries', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    let resolve!: (value: { run_id: string }) => void;
    const send = vi.spyOn(api, 'message').mockImplementation(() => new Promise(done => { resolve = done; }));
    vi.stubGlobal('navigator', { onLine: true }); const first = box.flush();
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1)); await box.flush();
    resolve({ run_id: 'fixture' }); await first;
    expect(send).toHaveBeenCalledTimes(1);
  });
  it('bounds automatic retries to five attempts and preserves a failed manual-retry record', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    vi.stubGlobal('navigator', { onLine: true });
    const send = vi.spyOn(api, 'message').mockRejectedValue(new ApiError('fixture', 503));
    for (let n = 0; n < 6; n++) await box.flush();
    expect(send).toHaveBeenCalledTimes(5); expect(box.entries[0]).toMatchObject({ status: 'failed', attempts: 5, client_message_id: id });
    expect((await outboxStore.list(origin, 's'))[0].status).toBe('failed');
  });
  it('only transient network and explicit retryable HTTP statuses are retried', () => {
    for (const status of [408, 429, 502, 503, 504]) expect(retryable(new ApiError('', status))).toBe(true);
    expect(retryable(new TypeError('fixture'))).toBe(true);
    for (const status of [400, 401, 403, 404, 409, 413, 500]) expect(retryable(new ApiError('', status))).toBe(false);
  });
});

const interrupted = (changes: Partial<InterruptionResult> = {}): InterruptionResult => ({ client_message_id: id, promoted: false, delivered: false, pending_acceptance: false, ...changes });
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (reason: unknown) => void; const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }

describe('Esc upgrades a sent UUID, never a draft or a duplicate acceptance', () => {
  it('commits mode and intent before interrupt, then submits a not-yet-sent message with its original payload', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    const interrupt = vi.spyOn(api, 'interruptMessage').mockImplementation(async (_session, requestId) => {
      expect(requestId).toBe(id);
      expect((await outboxStore.list(origin, 's'))[0]).toMatchObject({ delivery_mode: 'immediate', escalation_pending: true });
      return interrupted({ pending_acceptance: true });
    });
    const send = vi.spyOn(api, 'message').mockResolvedValue({ run_id: 'fixture' });
    vi.stubGlobal('navigator', { onLine: true });
    expect(await box.escalate(id)).toMatchObject({ pending_acceptance: true, promoted: false });
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(box.entries[0]).toMatchObject({ accepted: true }));
    expect(send.mock.calls[0][1]).toEqual({ ...payload, delivery_mode: 'immediate' });
    expect(payload.delivery_mode).toBe('after_call');
    interrupt.mockResolvedValue(interrupted({ promoted: true })); await box.flush();
    expect(box.entries).toEqual([]); expect(send).toHaveBeenCalledTimes(1);
  });
  it('keeps an old in-flight after_call 202 until interruption is actually confirmed', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    const sending = deferred<{ run_id: string }>(); const upgrading = deferred<InterruptionResult>();
    const send = vi.spyOn(api, 'message').mockReturnValue(sending.promise);
    vi.spyOn(api, 'interruptMessage').mockReturnValue(upgrading.promise);
    vi.stubGlobal('navigator', { onLine: true }); const flush = box.flush();
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    const escalation = box.escalate(id);
    await vi.waitFor(() => expect(box.entries[0].escalation_pending).toBe(true));
    sending.resolve({ run_id: 'fixture' }); await flush;
    expect((await outboxStore.list(origin, 's'))[0]).toMatchObject({ accepted: true, escalation_pending: true, delivery_mode: 'immediate' });
    upgrading.resolve(interrupted({ promoted: true })); await escalation;
    expect(await outboxStore.list(origin, 's')).toEqual([]);
    expect(send.mock.calls[0][1]).toEqual(payload); expect(send).toHaveBeenCalledTimes(1);
  });
  it('SSE acceptance before interrupt reply preserves the upgrade; interrupted SSE settles it idempotently', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    const upgrading = deferred<InterruptionResult>();
    const interrupt = vi.spyOn(api, 'interruptMessage').mockReturnValue(upgrading.promise);
    const send = vi.spyOn(api, 'message');
    vi.stubGlobal('navigator', { onLine: true }); const escalation = box.escalate(id);
    await vi.waitFor(() => expect(interrupt).toHaveBeenCalledTimes(1));
    await box.confirm([id]);
    expect((await outboxStore.list(origin, 's'))[0]).toMatchObject({ accepted: true, escalation_pending: true });
    await box.confirm([], [id]); await box.confirm([id], [id]);
    upgrading.resolve(interrupted({ pending_acceptance: true })); await escalation;
    expect(box.entries).toEqual([]); expect(await outboxStore.list(origin, 's')).toEqual([]); expect(send).not.toHaveBeenCalled();
  });
  it('offline escalation survives reload without claiming interruption and retries the same ID online', async () => {
    const first = new DurableOutbox(origin, 's'); await first.enqueue(payload);
    const interrupt = vi.spyOn(api, 'interruptMessage').mockResolvedValue(interrupted({ delivered: true }));
    const send = vi.spyOn(api, 'message');
    expect(await first.escalate(id)).toEqual(interrupted({ pending_acceptance: true }));
    expect(interrupt).not.toHaveBeenCalled();
    const reload = new DurableOutbox(origin, 's'); await reload.load();
    expect(reload.entries[0]).toMatchObject({ escalation_pending: true, delivery_mode: 'immediate' });
    expect(outboxPayload(reload.entries[0])).toEqual({ ...payload, delivery_mode: 'immediate' });
    vi.stubGlobal('navigator', { onLine: true }); await reload.flush();
    expect(interrupt.mock.calls[0].slice(0, 2)).toEqual(['s', id]); expect(send).not.toHaveBeenCalled();
    expect(await outboxStore.list(origin, 's')).toEqual([]);
  });
  it('already delivered server-side is a no-op without enqueueing or repeating acceptance', async () => {
    const box = new DurableOutbox(origin, 's');
    const interrupt = vi.spyOn(api, 'interruptMessage').mockResolvedValue(interrupted({ delivered: true }));
    const send = vi.spyOn(api, 'message');
    vi.stubGlobal('navigator', { onLine: true });
    expect(await box.escalate(id)).toEqual(interrupted({ delivered: true }));
    expect(interrupt).toHaveBeenCalledTimes(1); expect(send).not.toHaveBeenCalled(); expect(box.entries).toEqual([]);
  });
  it.each([400, 401, 403, 404, 422])('interrupt HTTP %s is visible, durable, and has no automatic retry', async status => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    const interrupt = vi.spyOn(api, 'interruptMessage').mockRejectedValue(new ApiError('fixture interrupt failure', status));
    const send = vi.spyOn(api, 'message'); vi.stubGlobal('navigator', { onLine: true });
    await expect(box.escalate(id)).rejects.toThrow('fixture interrupt failure');
    await box.flush(); await vi.advanceTimersByTimeAsync(60_000);
    expect(interrupt).toHaveBeenCalledTimes(1); expect(send).not.toHaveBeenCalled(); expect(box.error).toContain('fixture');
    expect((await outboxStore.list(origin, 's'))[0]).toMatchObject({ status: 'failed', escalation_pending: true, delivery_mode: 'immediate' });
    const reload = new DurableOutbox(origin, 's'); await reload.load(); await reload.flush(); expect(interrupt).toHaveBeenCalledTimes(1);
    interrupt.mockResolvedValue(interrupted({ promoted: true }));
    vi.stubGlobal('navigator', { onLine: false }); await reload.retry(id);
    vi.stubGlobal('navigator', { onLine: true }); await reload.flush();
    expect(interrupt.mock.calls[1][1]).toBe(id); expect(reload.entries).toEqual([]);
  });
  it('lost interrupt acknowledgement retries same UUID after refresh, never the accepted text', async () => {
    const first = new DurableOutbox(origin, 's'); await first.enqueue(payload); await first.escalate(id); await first.confirm([id]);
    const interrupt = vi.spyOn(api, 'interruptMessage').mockRejectedValueOnce(new TypeError('lost acknowledgement')).mockResolvedValue(interrupted({ promoted: true }));
    const send = vi.spyOn(api, 'message');
    vi.stubGlobal('navigator', { onLine: true }); await first.flush();
    vi.stubGlobal('navigator', { onLine: false });
    const reload = new DurableOutbox(origin, 's'); await reload.load();
    expect(reload.entries[0]).toMatchObject({ accepted: true, escalation_pending: true });
    vi.stubGlobal('navigator', { onLine: true }); await reload.flush();
    expect(interrupt.mock.calls.map(call => call[1])).toEqual([id, id]); expect(send).not.toHaveBeenCalled();
    expect(await outboxStore.list(origin, 's')).toEqual([]);
    vi.stubGlobal('navigator', { onLine: false }); first.wake();
  });
  it('pending_acceptance is retained and rechecked after acceptance without resubmitting', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload); await box.escalate(id); await box.confirm([id]);
    const interrupt = vi.spyOn(api, 'interruptMessage').mockResolvedValueOnce(interrupted({ pending_acceptance: true })).mockResolvedValue(interrupted({ promoted: true }));
    const send = vi.spyOn(api, 'message');
    vi.stubGlobal('navigator', { onLine: true }); await box.flush();
    expect(box.entries[0]).toMatchObject({ escalation_pending: true, accepted: true });
    await box.flush(); expect(box.entries).toEqual([]); expect(interrupt).toHaveBeenCalledTimes(2); expect(send).not.toHaveBeenCalled();
  });
  it('replayed acceptance events do not postpone the pending upgrade retry', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload); await box.escalate(id);
    const interrupt = vi.spyOn(api, 'interruptMessage').mockResolvedValue(interrupted({ promoted: true }));
    await box.confirm([id]);
    vi.stubGlobal('navigator', { onLine: true });
    await vi.advanceTimersByTimeAsync(500); await box.confirm([id]);
    await vi.advanceTimersByTimeAsync(500);
    await vi.waitFor(() => expect(interrupt).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(box.entries).toEqual([]));
  });
  it('persists offline no-entry server-queued escalation across refresh and never posts a user body', async () => {
    const box = new DurableOutbox(origin, 's');
    const interrupt = vi.spyOn(api, 'interruptMessage').mockResolvedValue(interrupted({ promoted: true })); const send = vi.spyOn(api, 'message');
    expect(await box.escalate(id)).toEqual(interrupted({ pending_acceptance: true })); expect(interrupt).not.toHaveBeenCalled();
    const [intent] = await outboxStore.list(origin, 's');
    expect(intent).toMatchObject({ client_message_id: id, interruption_only: true, accepted: true, escalation_pending: true, text: '强制介入请求待联网', images: [] });
    expect(() => outboxPayload(intent)).toThrow('不能作为用户消息');
    const reload = new DurableOutbox(origin, 's'); await reload.load();
    vi.stubGlobal('navigator', { onLine: true }); await reload.flush();
    expect(interrupt).toHaveBeenCalledTimes(1); expect(interrupt.mock.calls[0][1]).toBe(id);
    expect(send).not.toHaveBeenCalled(); expect(await outboxStore.list(origin, 's')).toEqual([]);
  });
  it('reopens a cleaned 202 UUID as an interruption-only intent and allows SSE to settle it', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload); await box.confirm([id]);
    expect(box.entries).toEqual([]); await box.escalate(id);
    expect(box.entries[0]).toMatchObject({ interruption_only: true, accepted: true });
    await box.confirm([id]); expect(box.entries).toHaveLength(1);
    await box.confirm([], [id]); expect(box.entries).toEqual([]); expect(await outboxStore.list(origin, 's')).toEqual([]);
  });
  it('commits online no-entry intent before HTTP and recovers its lost acknowledgement after refresh', async () => {
    const box = new DurableOutbox(origin, 's');
    const interrupt = vi.spyOn(api, 'interruptMessage').mockImplementationOnce(async () => {
      expect((await outboxStore.list(origin, 's'))[0]).toMatchObject({ interruption_only: true, escalation_pending: true });
      throw new TypeError('lost interrupt ACK');
    }).mockResolvedValue(interrupted({ promoted: true }));
    const send = vi.spyOn(api, 'message'); vi.stubGlobal('navigator', { onLine: true });
    await expect(box.escalate(id)).rejects.toThrow('lost interrupt ACK');
    vi.stubGlobal('navigator', { onLine: false }); const reload = new DurableOutbox(origin, 's'); await reload.load();
    vi.stubGlobal('navigator', { onLine: true }); await reload.flush();
    expect(interrupt.mock.calls.map(call => call[1])).toEqual([id, id]); expect(send).not.toHaveBeenCalled(); expect(await outboxStore.list(origin, 's')).toEqual([]);
    vi.stubGlobal('navigator', { onLine: false }); box.wake();
  });
  it('persists interruption-only auth failure and manually retries only interrupt with the same UUID', async () => {
    const box = new DurableOutbox(origin, 's');
    const interrupt = vi.spyOn(api, 'interruptMessage').mockRejectedValueOnce(new ApiError('login required', 401)).mockResolvedValue(interrupted({ delivered: true }));
    const send = vi.spyOn(api, 'message'); vi.stubGlobal('navigator', { onLine: true });
    await expect(box.escalate(id)).rejects.toThrow('login required');
    const reload = new DurableOutbox(origin, 's'); await reload.load(); await reload.flush();
    expect(interrupt).toHaveBeenCalledTimes(1); expect(reload.entries[0]).toMatchObject({ interruption_only: true, accepted: true, failure: 'auth', status: 'failed' });
    vi.stubGlobal('navigator', { onLine: false }); await reload.retry(id);
    vi.stubGlobal('navigator', { onLine: true }); await reload.flush();
    expect(interrupt.mock.calls.map(call => call[1])).toEqual([id, id]); expect(send).not.toHaveBeenCalled(); expect(reload.entries).toEqual([]);
  });
  it('strictly validates interruption-only records and strips arbitrary descriptions and secrets', () => {
    const intent = { ...stored(), text: '', images: [], delivery_mode: 'immediate', accepted: true, escalation_pending: true, interruption_only: true };
    expect(safeEntry(intent)).toMatchObject({ interruption_only: true, text: '强制介入请求待联网', images: [] });
    for (const changes of [{ accepted: false }, { escalation_pending: false }, { delivery_mode: 'after_call' }, { images: payload.images }]) expect(safeEntry({ ...intent, ...changes })).toBeNull();
    expect(safeEntry({ ...stored(), text: '', images: [] })).toBeNull();
    expect(JSON.stringify(safeEntry({ ...intent, text: 'never-persist-body', api_key: 'never-persist-secret' }))).not.toContain('never-persist');
  });
  it('SSE upgrade or delivery wins over a lost interrupt response without a false error', async () => {
    for (const delivered of [false, true]) {
      const box = new DurableOutbox(origin, delivered ? 'delivered' : 's'); await box.enqueue(payload);
      const response = deferred<InterruptionResult>();
      const interrupt = vi.spyOn(api, 'interruptMessage').mockReturnValue(response.promise);
      vi.stubGlobal('navigator', { onLine: true }); const escalation = box.escalate(id);
      await vi.waitFor(() => expect(interrupt).toHaveBeenCalled());
      await box.confirm([id], [id], delivered ? [id] : []);
      response.reject(new TypeError('lost acknowledgement'));
      expect(await escalation).toEqual(interrupted({ promoted: !delivered, delivered }));
      expect(box.error).toBe(''); expect(box.entries).toEqual([]);
      interrupt.mockRestore(); vi.stubGlobal('navigator', { onLine: false });
    }
  });
  it('does not promote over the network until the escalation commit succeeds', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload);
    vi.spyOn(outboxStore, 'put').mockRejectedValue(new Error('storage full'));
    const interrupt = vi.spyOn(api, 'interruptMessage'); const send = vi.spyOn(api, 'message');
    vi.stubGlobal('navigator', { onLine: true }); await expect(box.escalate(id)).rejects.toThrow('storage full');
    expect(interrupt).not.toHaveBeenCalled(); expect(send).not.toHaveBeenCalled();
    expect((await outboxStore.list(origin, 's'))[0].delivery_mode).toBe('after_call');
    expect(box.entries[0]).toMatchObject({ delivery_mode: 'after_call' }); expect(box.entries[0].escalation_pending).toBeUndefined();
  });
  it('bounds pending-acceptance rechecks and does not block other ordinary messages', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload); await box.escalate(id); await box.confirm([id]);
    const other = { ...payload, client_message_id: '27ccabf2-d256-4813-a229-f8115f570c2a' }; await box.enqueue(other);
    const interrupt = vi.spyOn(api, 'interruptMessage').mockResolvedValue(interrupted({ pending_acceptance: true }));
    const send = vi.spyOn(api, 'message').mockResolvedValue({ run_id: 'other' });
    vi.stubGlobal('navigator', { onLine: true });
    for (let n = 0; n < 6; n++) await box.flush();
    expect(interrupt).toHaveBeenCalledTimes(5); expect(send).toHaveBeenCalledTimes(1); expect(send.mock.calls[0][1]).toEqual(other);
    expect(box.entries[0]).toMatchObject({ client_message_id: id, escalation_pending: true, status: 'failed' });
  });
  it('does not block a later ordinary message after a non-auth interruption rejection', async () => {
    const box = new DurableOutbox(origin, 's'); await box.enqueue(payload); await box.escalate(id); await box.confirm([id]);
    const other = { ...payload, client_message_id: '27ccabf2-d256-4813-a229-f8115f570c2a' }; await box.enqueue(other);
    const interrupt = vi.spyOn(api, 'interruptMessage').mockRejectedValue(new ApiError('invalid escalation', 400));
    const send = vi.spyOn(api, 'message').mockResolvedValue({ run_id: 'other' });
    vi.stubGlobal('navigator', { onLine: true }); await box.flush();
    expect(interrupt).toHaveBeenCalledTimes(1); expect(send).toHaveBeenCalledTimes(1); expect(send.mock.calls[0][1]).toEqual(other);
    expect(box.entries[0]).toMatchObject({ client_message_id: id, status: 'failed', escalation_pending: true });
  });
  it('keeps one entry and one send when enqueue is retried with the same UUID', async () => {
    const box = new DurableOutbox(origin, 's'); await Promise.all([box.enqueue(payload), box.enqueue(payload)]);
    expect(box.entries).toHaveLength(1);
    const send = vi.spyOn(api, 'message').mockResolvedValue({ run_id: 'same' });
    vi.stubGlobal('navigator', { onLine: true }); await box.flush(); await box.enqueue(payload); await box.flush();
    expect(send).toHaveBeenCalledTimes(1); expect(box.entries).toEqual([]);
  });
  it('interrupt API posts only the original UUID to the session-specific endpoint', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify(interrupted({ promoted: true })), { status: 200, headers: { 'Content-Type': 'application/json' } }));
    vi.stubGlobal('fetch', fetch);
    expect(await api.interruptMessage('session/one', id)).toEqual(interrupted({ promoted: true }));
    expect(fetch.mock.calls[0][0]).toBe('/api/sessions/session%2Fone/messages/interrupt');
    expect(fetch.mock.calls[0][1]).toMatchObject({ method: 'POST', credentials: 'same-origin', body: JSON.stringify({ client_message_id: id }) });
  });
  it('old saved formats remain readable and only explicitly allowed upgrade fields persist', () => {
    const legacy = safeEntry(stored())!; expect(legacy.escalation_pending).toBeUndefined();
    const saved = safeEntry({ ...stored(), delivery_mode: 'immediate', escalation_pending: true, accepted: true, escalation_attempts: 2, api_key: 'secret' })!;
    expect(saved).toMatchObject({ escalation_pending: true, accepted: true, escalation_attempts: 2 });
    expect(JSON.stringify(saved)).not.toContain('secret');
  });
});

describe('paste payload bounds and network status', () => {
  it('validates 8 attachments, decoded per-image limit, encoded total and MIME allowlist', () => {
    expect(() => validateMessage({ ...payload, images: Array(8).fill(payload.images![0]) })).not.toThrow();
    expect(() => validateMessage({ ...payload, images: Array(9).fill(payload.images![0]) })).toThrow('8');
    expect(() => validateMessage({ ...payload, images: [{ mime: 'image/png', data: 'AAAA'.repeat(Math.ceil((10 * 1024 * 1024 + 1) / 3)) }] })).toThrow('10 MiB');
    expect(() => validateMessage({ ...payload, text: 'a'.repeat(24 * 1024 * 1024) })).toThrow('24 MiB');
    expect(() => validateMessage({ ...payload, images: [{ mime: 'text/html', data: 'YWJj' }] })).toThrow('格式');
    expect(() => validateMessage({ ...payload, images: [{ mime: 'image/png', data: '<script>' }] })).toThrow('格式');
  });
  it('snapshots both clipboard channels for byte-based dedup without losing metadata collisions', () => {
    const a = { name: 'a.png', type: 'image/png', size: 3, lastModified: 0 } as File;
    const b = { ...a } as File;
    const data = { items: [a, b].map(file => ({ kind: 'file', getAsFile: () => file })), files: [a, b] } as unknown as DataTransfer;
    expect(pastedFiles(data)).toEqual([a, b, a, b]);
    expect(pastedFiles({ items: [], files: [a] } as unknown as DataTransfer)).toEqual([a]);
  });
  it('waiting_for_network remains active/busy with an automatic recovery label', () => {
    expect(activeStatus('waiting_for_network')).toBe(true); expect(statusLabels.waiting_for_network).toBe('等待网络');
  });
});
