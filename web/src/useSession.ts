import { useCallback, useEffect, useRef, useState } from 'react';
import { api, errorMessage } from './api';
import { latestSequence, mergeEvents, sessionFromEvent } from './events';
import type { AgentEvent, Session } from './types';

export type Connection = 'loading' | 'connected' | 'reconnecting' | 'error';
export function useSession(id: string | null, onSession: (session: Session) => void) {
  const [session, setSession] = useState<Session | null>(null);
  const [events, setEvents] = useState<AgentEvent[]>([]);
  const [connection, setConnection] = useState<Connection>('loading');
  const [error, setError] = useState('');
  const [generation, setGeneration] = useState(0);
  const sessionRef = useRef<Session | null>(null);
  const callback = useRef(onSession);
  callback.current = onSession;
  const updateSession = useCallback((next: Session) => {
    sessionRef.current = next; setSession(next); callback.current(next);
  }, []);
  useEffect(() => {
    setSession(null); sessionRef.current = null; setEvents([]); setError(''); setConnection('loading');
    if (!id) return;
    const controller = new AbortController();
    let disposed = false;
    let source: EventSource | undefined;
    let reconnect: ReturnType<typeof setTimeout> | undefined;
    let after = 0;
    const connect = () => {
      if (disposed) return;
      clearTimeout(reconnect); source?.close();
      source = new EventSource(`/api/sessions/${encodeURIComponent(id)}/events?after=${after}`, { withCredentials: true });
      source.onopen = () => { if (!disposed) { setConnection('connected'); setError(''); window.dispatchEvent(new Event('justai:connected')); } };
      source.addEventListener('update', (raw) => {
        if (disposed) return;
        try {
          const event = JSON.parse((raw as MessageEvent<string>).data) as AgentEvent;
          if (event.session_id !== id || !Number.isSafeInteger(event.seq) || typeof event.type !== 'string' || !event.data || typeof event.data !== 'object') throw new Error('invalid event');
          if (event.seq <= after) return;
          after = event.seq;
          setEvents(current => mergeEvents(current, [event]));
          const current = sessionRef.current;
          if (current) {
            const next = sessionFromEvent(current, event);
            if (next !== current) updateSession(next);
          }
        } catch { setError('收到无法识别的事件；已保留当前记录。请重新加载会话。'); }
      });
      source.onerror = () => {
        source?.close();
        if (disposed) return;
        setConnection('reconnecting');
        reconnect = setTimeout(connect, 2000);
        void api.bootstrap().then(state => {
          if (!state.authenticated && !disposed) window.dispatchEvent(new Event('justai:unauthorized'));
        }).catch(() => { /* Reconnect remains available during a temporary outage. */ });
      };
    };
    // Exactly one detail fetch per load. SSE begins only after the committed snapshot.
    void api.detail(id, controller.signal).then(detail => {
      if (disposed) return;
      updateSession(detail.session); setEvents(mergeEvents([], detail.events));
      after = latestSequence(detail.events); connect();
    }).catch(reason => {
      if (!disposed) { setConnection('error'); setError(errorMessage(reason)); }
    });
    const online = () => { if (disposed) return; if (sessionRef.current) { setConnection('reconnecting'); connect(); } else setGeneration(value => value + 1); };
    window.addEventListener('online', online);
    return () => { disposed = true; controller.abort(); source?.close(); clearTimeout(reconnect); window.removeEventListener('online', online); };
  }, [id, generation, updateSession]);
  const updateSettings = useCallback((saved: Session) => {
    const current = sessionRef.current;
    if (current?.id === saved.id) updateSession({ ...current, selection: saved.selection, reasoning_effort: saved.reasoning_effort ?? null, updated_at: saved.updated_at });
  }, [updateSession]);
  return { session, events, connection, error, updateSession, updateSettings, reload: () => setGeneration(n => n + 1) };
}
