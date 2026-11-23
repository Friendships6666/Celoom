import { useEffect, useMemo, useReducer, useRef } from 'react';
import { sessionOutbox } from './outbox';
import type { AgentEvent } from './types';

export function useOutbox(sessionId: string | undefined, events: AgentEvent[]) {
  const box = useMemo(() => sessionId ? sessionOutbox(window.location.origin, sessionId) : null, [sessionId]);
  const [, refresh] = useReducer(value => value + 1, 0);
  const started = useRef<typeof box>(null);
  useEffect(() => {
    if (!box) return;
    const unsubscribe = box.subscribe(refresh);
    const online = () => box.wake();
    window.addEventListener('online', online);
    window.addEventListener('celoom:connected', online);
    // Keep the durable payload when navigating away, including in-flight sends.
    return () => { unsubscribe(); window.removeEventListener('online', online); window.removeEventListener('celoom:connected', online); };
  }, [box]);
  useEffect(() => {
    if (!box) return;
    const relevant = events.filter(event => event.session_id === box.sessionId && typeof event.data.client_message_id === 'string');
    const accepted = relevant.filter(event => event.type === 'user_message').map(event => event.data.client_message_id as string);
    const delivered = relevant.filter(event => event.type === 'message_delivered' || (event.type === 'user_message' && event.data.queued !== true)).map(event => event.data.client_message_id as string);
    const upgraded = [...delivered, ...relevant.filter(event => event.type === 'message_interrupted').map(event => event.data.client_message_id as string)];
    const first = started.current !== box; started.current = box;
    void box.confirm(accepted, upgraded, delivered).then(() => { if (first) box.wake(); }).catch(() => {});
  }, [box, events]);
  const accepted = new Set(events.filter(event => event.session_id === sessionId && event.type === 'user_message').map(event => event.data.client_message_id));
  return { box, entries: (box?.entries ?? []).filter(entry => entry.escalation_pending || !accepted.has(entry.client_message_id)), error: box?.error ?? '' };
}
