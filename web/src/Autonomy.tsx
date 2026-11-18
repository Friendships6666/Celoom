import { useRef, useState, type FormEvent } from 'react';
import { Check, CheckCheck, ChevronDown, CircleHelp, History, LoaderCircle, Pause, Play, Plus, Send, Target, X } from 'lucide-react';
import { api, errorMessage } from './api';
import { Alert } from './ui';
import { useAutonomy } from './useAutonomy';
import type { AgentEvent, Goal, GoalAction, Question } from './types';

function QuestionCard({ question, onSaved }: { question: Question; onSaved: (question: Question) => void }) {
  const [choice, setChoice] = useState(''); const [answer, setAnswer] = useState('');
  const [busy, setBusy] = useState(false); const [error, setError] = useState(''); const lock = useRef(false);
  const pending = question.status === 'pending';
  const value = choice === 'custom' || !question.options.length ? answer.trim() : choice.startsWith('option:') ? question.options[Number(choice.slice(7))] : '';
  async function submit(event: FormEvent) {
    event.preventDefault(); if (!pending || !value || lock.current) return;
    lock.current = true; setBusy(true); setError('');
    try { onSaved(await api.answerQuestion(question.session_id, question.id, value)); }
    catch (reason) { setError(errorMessage(reason)); }
    finally { lock.current = false; setBusy(false); }
  }
  return <article className={`question-card ${question.blocking ? 'blocking' : ''}`} aria-label={question.question}>
    <div className="question-heading"><span className={`autonomy-chip ${question.blocking ? 'blocking' : ''}`}>{question.blocking ? '等待回答 · 任务已停候' : '可选回答 · 任务继续'}</span>{!pending && <span className="muted">{question.status === 'answered' ? '已回答' : 'AI 已撤回'}</span>}</div>
    <p className="question-text">{question.question}</p>
    {pending ? <form onSubmit={event => void submit(event)} aria-label={`回答：${question.question}`}>
      {question.options.length > 0 && <fieldset className="question-options" disabled={busy}><legend className="sr-only">建议答案或自由输入</legend>{question.options.map((option, index) => <label key={index}><input type="radio" name={`question-${question.id}`} value={`option:${index}`} checked={choice === `option:${index}`} onChange={event => setChoice(event.target.value)} /><span>{option}</span>{index === 0 && <small>建议</small>}</label>)}<label><input type="radio" name={`question-${question.id}`} value="custom" checked={choice === 'custom'} onChange={() => setChoice('custom')} /><span>自由输入</span></label></fieldset>}
      {(!question.options.length || choice === 'custom') && <textarea aria-label={`自定义回答：${question.question}`} rows={2} maxLength={20000} placeholder="输入你的回答…" value={answer} disabled={busy} onChange={event => setAnswer(event.target.value)} />}
      {error && <Alert>{error}</Alert>}<div className="question-submit"><small>回答将交给 Agent 继续处理</small><button type="submit" className="button primary" disabled={busy || !value}>{busy ? <LoaderCircle size={13} className="spin" /> : <Send size={13} />}提交回答</button></div>
    </form> : question.answer !== null && <div className="question-answer"><CheckCheck size={14} /><span>{question.answer}</span></div>}
  </article>;
}
const goalLabels: Record<Goal['status'], string> = { pending: '待开始', active: '进行中', paused: '已暂停', completed: '已完成', cancelled: '已取消' };
function GoalCard({ goal, hasActive, onChange }: { goal: Goal; hasActive: boolean; onChange: (goal: Goal, action: GoalAction) => Promise<void> }) {
  const [busy, setBusy] = useState(false); const [error, setError] = useState(''); const lock = useRef(false);
  async function act(action: GoalAction) {
    if (lock.current) return; lock.current = true; setBusy(true); setError('');
    try { await onChange(goal, action); }
    catch (reason) { setError(errorMessage(reason)); }
    finally { lock.current = false; setBusy(false); }
  }
  const terminal = goal.status === 'completed' || goal.status === 'cancelled';
  return <li className={`goal-card goal-${goal.status}`}><div className="goal-title"><span className="goal-check" aria-label={goal.status === 'completed' ? 'AI 已确认完成' : goalLabels[goal.status]}>{goal.status === 'completed' ? <Check size={13} /> : goal.status === 'active' ? <span className="connection-dot" /> : null}</span><strong>{goal.title}</strong><span className="autonomy-chip">{goalLabels[goal.status]}</span></div><p className="goal-objective">{goal.objective}</p>{goal.progress && <p className="goal-progress">{goal.progress}</p>}<div className="goal-footer"><small>已执行 {goal.rounds} 轮</small>{busy && <LoaderCircle size={13} className="spin" />}<div className="goal-actions">{!terminal && <>{goal.status === 'active' ? <button className="text-button" disabled={busy} onClick={() => void act('pause')}><Pause size={12} />暂停</button> : <button className="text-button" disabled={busy || hasActive} title={hasActive ? '请先暂停当前进行中的目标' : '明确开始后才会运行任务'} onClick={() => void act(goal.status === 'pending' ? 'start' : 'resume')}><Play size={12} />{goal.status === 'pending' ? '开始' : '恢复'}</button>}<button className="text-button danger-text" disabled={busy} onClick={() => void act('cancel')}><X size={12} />取消</button></>}</div></div>{error && <Alert>{error}</Alert>}</li>;
}
export function AutonomyPanels({ sessionId, events }: { sessionId: string; events: AgentEvent[] }) {
  const state = useAutonomy(sessionId, events);
  const [adding, setAdding] = useState(false); const [title, setTitle] = useState(''); const [objective, setObjective] = useState('');
  const [goalsOpen, setGoalsOpen] = useState(false); const [questionsOpen, setQuestionsOpen] = useState(true);
  const [creating, setCreating] = useState(false); const [error, setError] = useState(''); const lock = useRef(false);
  const pending = state.questions.filter(question => question.status === 'pending');
  const history = state.questions.filter(question => question.status !== 'pending');
  const openGoals = state.goals.filter(goal => !['completed', 'cancelled'].includes(goal.status));
  async function create(event: FormEvent) {
    event.preventDefault(); if (lock.current || !title.trim() || !objective.trim()) return;
    lock.current = true; setCreating(true); setError('');
    try { state.saveGoal(await api.createGoal(sessionId, { title: title.trim(), objective: objective.trim() })); setTitle(''); setObjective(''); setAdding(false); }
    catch (reason) { setError(errorMessage(reason)); }
    finally { lock.current = false; setCreating(false); }
  }
  return <div className="autonomy-panels">
    <details className="goals-panel" open={goalsOpen}><summary onClick={event => { event.preventDefault(); setGoalsOpen(!goalsOpen); if (!goalsOpen) setQuestionsOpen(false); }}><Target size={15} /><strong>长期目标</strong><span className="autonomy-count">{openGoals.length}</span><small>{state.goals.find(goal => goal.status === 'active')?.title || (state.loading ? '读取中…' : '待办与执行进度')}</small><ChevronDown size={14} /></summary><div className="goals-content">
      {state.errors.goals && <Alert>目标加载失败：{state.errors.goals}<button className="text-button" onClick={state.reload}>重试</button></Alert>}
      <div className="goals-toolbar"><small>添加不启动任务 · 仅 AI 可确认完成</small><button className="text-button" onClick={() => setAdding(value => !value)} aria-expanded={adding}><Plus size={14} />添加目标</button></div>
      {adding && <form className="goal-create" onSubmit={event => void create(event)}><label>标题<input aria-label="目标标题" maxLength={200} value={title} disabled={creating} onChange={event => setTitle(event.target.value)} placeholder="为长期任务起个名字" required /></label><label>目标<textarea aria-label="目标说明" maxLength={20000} rows={2} value={objective} disabled={creating} onChange={event => setObjective(event.target.value)} placeholder="描述要达成的结果与验收要求" required /></label>{error && <Alert>{error}</Alert>}<button className="button primary" disabled={creating || !title.trim() || !objective.trim()}>{creating ? <LoaderCircle size={13} className="spin" /> : <Plus size={13} />}添加到待办</button></form>}
      {state.goals.length ? <ul className="goal-list">{state.goals.map(goal => <GoalCard key={goal.id} goal={goal} hasActive={state.goals.some(item => item.status === 'active' && item.id !== goal.id)} onChange={state.changeGoal} />)}</ul> : !state.loading && <p className="autonomy-empty">还没有长期目标</p>}
    </div></details>
    {state.errors.questions && <Alert>问题加载失败：{state.errors.questions}<button className="text-button" onClick={state.reload}>重试</button></Alert>}
    {pending.length > 0 && <details className="questions-panel" open={questionsOpen}><summary onClick={event => { event.preventDefault(); setQuestionsOpen(!questionsOpen); if (!questionsOpen) setGoalsOpen(false); }}><CircleHelp size={15} /><strong>Agent 请求你回答</strong><span className="autonomy-count">{pending.length}</span><small>{pending.some(question => question.blocking) ? '有任务等待你的回答' : '可稍后回答'}</small><ChevronDown size={14} /></summary><div className="question-stack">{pending.map(question => <QuestionCard key={question.id} question={question} onSaved={state.saveQuestion} />)}</div></details>}
    {history.length > 0 && <details className="question-history"><summary><History size={12} />回答与 AI 撤回记录 <span>{history.length}</span><ChevronDown size={12} /></summary><div className="question-stack">{history.map(question => <QuestionCard key={question.id} question={question} onSaved={state.saveQuestion} />)}</div></details>}
  </div>;
}
