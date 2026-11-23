import { useCallback, useEffect, useRef, useState } from 'react';
import { ApiError, api, errorMessage } from './api';
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
    sessionRef.current = next;
    setSession(next);
    callback.current(next);
  }, []);

  useEffect(() => {
    setSession(null);
    sessionRef.current = null;
    setEvents([]);
    setError('');
    setConnection('loading');
    if (!id) return;

    const controller = new AbortController();
    let disposed = false;
    let source: EventSource | undefined;
    let reconnectTimer: ReturnType<typeof setTimeout> | undefined;
    let after = 0;
    let backoffMs = 1000;
    let loaded = false;

    const nextDelay = () => {
      const delay = backoffMs;
      backoffMs = Math.min(backoffMs * 2, 10000);
      return delay;
    };

    const connectSse = () => {
      if (disposed) return;
      clearTimeout(reconnectTimer);
      source?.close();
      source = new EventSource(`/api/sessions/${encodeURIComponent(id)}/events?after=${after}`, {
        withCredentials: true,
      });
      source.onopen = () => {
        if (disposed) return;
        backoffMs = 1000;
        setConnection('connected');
        setError('');
        window.dispatchEvent(new Event('celoom:connected'));
      };
      source.addEventListener('update', raw => {
        if (disposed) return;
        try {
          const event = JSON.parse((raw as MessageEvent<string>).data) as AgentEvent;
          if (
            event.session_id !== id ||
            !Number.isSafeInteger(event.seq) ||
            typeof event.type !== 'string' ||
            !event.data ||
            typeof event.data !== 'object'
          ) {
            throw new Error('invalid event');
          }
          if (event.seq <= after) return;
          after = event.seq;
          setEvents(current => mergeEvents(current, [event]));
          const current = sessionRef.current;
          if (current) {
            const next = sessionFromEvent(current, event);
            if (next !== current) updateSession(next);
          }
        } catch {
          setError('收到无法识别的事件；已保留当前记录。请重新加载会话。');
        }
      });
      source.onerror = () => {
        source?.close();
        if (disposed) return;
        setConnection('reconnecting');
        reconnectTimer = setTimeout(connectSse, nextDelay());
        void api
          .bootstrap()
          .then(state => {
            if (!state.authenticated && !disposed) {
              window.dispatchEvent(new Event('celoom:unauthorized'));
            }
          })
          .catch(() => {
            /* Keep exponential backoff reconnect active during temporary outages. */
          });
      };
    };

    const loadDetail = () => {
      if (disposed) return;
      clearTimeout(reconnectTimer);
      void api
        .detail(id, controller.signal)
        .then(detail => {
          if (disposed) return;
          loaded = true;
          backoffMs = 1000;
          updateSession(detail.session);
          setEvents(mergeEvents([], detail.events));
          after = latestSequence(detail.events);
          connectSse();
        })
        .catch(reason => {
          if (disposed || controller.signal.aborted) return;
          const fatal = reason instanceof ApiError && [401, 403, 404].includes(reason.status);
          if (fatal) {
            setConnection('error');
            setError(errorMessage(reason));
          } else {
            setConnection('reconnecting');
            setError(errorMessage(reason));
            reconnectTimer = setTimeout(loadDetail, nextDelay());
          }
        });
    };

    loadDetail();

    const triggerReconnect = () => {
      if (disposed) return;
      backoffMs = 1000;
      clearTimeout(reconnectTimer);
      if (loaded && sessionRef.current) {
        setConnection('reconnecting');
        connectSse();
      } else {
        setConnection('reconnecting');
        loadDetail();
      }
    };

    const onOffline = () => {
      if (disposed) return;
      clearTimeout(reconnectTimer);
      source?.close();
      setConnection('reconnecting');
    };

    const onVisibility = () => {
      if (disposed || document.visibilityState !== 'visible') return;
      if (!source || source.readyState === EventSource.CLOSED) {
        triggerReconnect();
      }
    };

    window.addEventListener('online', triggerReconnect);
    window.addEventListener('offline', onOffline);
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      disposed = true;
      controller.abort();
      source?.close();
      clearTimeout(reconnectTimer);
      window.removeEventListener('online', triggerReconnect);
      window.removeEventListener('offline', onOffline);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  }, [id, generation, updateSession]);

  const updateSettings = useCallback(
    (saved: Session) => {
      const current = sessionRef.current;
      if (current?.id === saved.id) {
        updateSession({
          ...current,
          selection: saved.selection,
          reasoning_effort: saved.reasoning_effort ?? null,
          updated_at: saved.updated_at,
        });
      }
    },
    [updateSession],
  );

  return {
    session,
    events,
    connection,
    error,
    updateSession,
    updateSettings,
    reload: () => setGeneration(n => n + 1),
  };
}
