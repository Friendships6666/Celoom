import { useCallback, useEffect, useRef, useState } from 'react';
import { api, errorMessage } from './api';
import { goalSnapshot, latestSequence, mergeSnapshots, questionSnapshot } from './events';
import type { AgentEvent, Goal, GoalAction, Question } from './types';

export function useAutonomy(sessionId: string, events: AgentEvent[]) {
  const [questions, setQuestions] = useState<Question[]>([]);
  const [goals, setGoals] = useState<Goal[]>([]);
  const [loading, setLoading] = useState(true);
  const [errors, setErrors] = useState({ questions: '', goals: '' });
  const [generation, setGeneration] = useState(0);
  const seen = useRef(0);
  const changedQuestions = useRef(new Set<string>());
  const changedGoals = useRef(new Set<string>());
  const goalEventRevision = useRef(new Map<string, number>());
  const initialEvents = useRef(events);
  useEffect(() => {
    const controller = new AbortController(); let disposed = false;
    changedQuestions.current.clear(); changedGoals.current.clear();
    setLoading(true); setErrors({ questions: '', goals: '' });
    void Promise.allSettled([
      api.questions(sessionId, controller.signal).then(items => {
        if (!disposed) setQuestions(current => mergeSnapshots(mergeSnapshots(current, items.filter(item => item.session_id === sessionId && !changedQuestions.current.has(item.id))), current.filter(item => changedQuestions.current.has(item.id))));
      }).catch(error => { if (!disposed) setErrors(current => ({ ...current, questions: errorMessage(error) })); }),
      api.goals(sessionId, controller.signal).then(items => {
        if (!disposed) setGoals(current => mergeSnapshots(mergeSnapshots(current, items.filter(item => item.session_id === sessionId && !changedGoals.current.has(item.id))), current.filter(item => changedGoals.current.has(item.id))));
      }).catch(error => { if (!disposed) setErrors(current => ({ ...current, goals: errorMessage(error) })); }),
    ]).then(() => { if (!disposed) setLoading(false); });
    return () => { disposed = true; controller.abort(); };
  }, [sessionId, generation]);
  useEffect(() => {
    // The detail history is older than initial GET; only later SSE snapshots win
    // over a still-in-flight GET (including equal database timestamps).
    const initialMax = latestSequence(initialEvents.current);
    for (const event of events) {
      if (event.seq <= seen.current || event.session_id !== sessionId) continue;
      const question = questionSnapshot(event); const goal = goalSnapshot(event);
      if (question) {
        if (event.seq > initialMax) changedQuestions.current.add(question.id);
        setQuestions(current => mergeSnapshots(current, [question]));
      }
      if (goal) {
        goalEventRevision.current.set(goal.id, event.seq);
        if (event.seq > initialMax) changedGoals.current.add(goal.id);
        setGoals(current => mergeSnapshots(current, [goal]));
      }
    }
    seen.current = latestSequence(events);
  }, [events, sessionId]);
  const saveQuestion = useCallback((question: Question) => { changedQuestions.current.add(question.id); setQuestions(current => mergeSnapshots(current, [question])); }, []);
  const saveGoal = useCallback((goal: Goal) => { changedGoals.current.add(goal.id); setGoals(current => mergeSnapshots(current, [goal])); }, []);
  const changeGoal = useCallback(async (goal: Goal, action: GoalAction) => {
    const revision = goalEventRevision.current.get(goal.id);
    const saved = await api.changeGoal(sessionId, goal.id, action);
    changedGoals.current.add(saved.id);
    setGoals(current => {
      const latest = current.find(item => item.id === saved.id);
      // A newer SSE state received while PATCH was in flight wins timestamp ties.
      if (latest && goalEventRevision.current.get(saved.id) !== revision && saved.updated_at <= latest.updated_at) return current;
      return mergeSnapshots(current, [saved]);
    });
  }, [sessionId]);
  return { questions, goals, loading, errors, saveQuestion, saveGoal, changeGoal, reload: () => setGeneration(value => value + 1) };
}
