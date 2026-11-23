import { useEffect, useState } from 'react';
import { Activity, ChevronDown, ChevronRight, Cpu, GitBranch, Layers, Play, RotateCcw, Server, Square, Terminal } from 'lucide-react';
import { api } from './api';
import type { RuntimeItem, RuntimeOverview } from './types';

function StatePill({ running }: { running: boolean }) {
  return (
    <span className={`runtime-state ${running ? 'is-running' : 'is-ended'}`}>
      <i className="runtime-dot" />
      {running ? '运行' : '结束'}
    </span>
  );
}

function GroupSection({
  title,
  icon,
  items,
  emptyText,
  defaultOpen = false,
  wide = false,
  headerActions,
  onOpenSession,
  onResumeSubagent,
  onCancelSubagent,
  busyId,
}: {
  title: string;
  icon: React.ReactNode;
  items: RuntimeItem[];
  emptyText: string;
  defaultOpen?: boolean;
  wide?: boolean;
  headerActions?: React.ReactNode;
  onOpenSession?: (id: string) => void;
  onResumeSubagent?: (id: string) => void;
  onCancelSubagent?: (id: string) => void;
  busyId?: string | null;
}) {
  const runningCount = items.filter(item => item.running).length;
  return (
    <details className={`runtime-group ${wide ? 'runtime-group-wide' : ''}`} open={defaultOpen || runningCount > 0}>
      <summary>
        <span className="runtime-group-title">{icon}{title}</span>
        <span className="runtime-group-counts">
          <i className={`runtime-dot ${runningCount > 0 ? 'is-running' : 'is-ended'}`} />
          {runningCount} 运行 / {items.length} 总计
        </span>
      </summary>
      {headerActions && <div className="runtime-group-actions">{headerActions}</div>}
      {items.length === 0 ? (
        <p className="runtime-empty">{emptyText}</p>
      ) : (
        <ul className="runtime-items">
          {items.map(item => {
            const isSubagent = Boolean(item.parent_id);
            const canResume = isSubagent && !item.running && ['cancelled', 'interrupted', 'paused', 'failed'].includes(item.status);
            return (
              <li key={item.id} className="runtime-item">
                <div className="runtime-item-main">
                  <StatePill running={item.running} />
                  {onOpenSession && isSubagent ? (
                    <button type="button" className="runtime-link" title={`打开子代理 ${item.name}`} onClick={() => onOpenSession(item.id)}>
                      {item.name}
                    </button>
                  ) : (
                    <strong className="runtime-item-name">{item.name}</strong>
                  )}
                </div>
                <div className="runtime-item-actions">
                  {item.detail && <small className="runtime-item-detail">{item.detail}</small>}
                  {isSubagent && item.running && onCancelSubagent && (
                    <button type="button" className="runtime-mini-btn danger" disabled={busyId === item.id} onClick={() => onCancelSubagent(item.id)} title="停止此子代理">
                      <Square size={10} fill="currentColor" />停止
                    </button>
                  )}
                  {canResume && onResumeSubagent && (
                    <button type="button" className="runtime-mini-btn ok" disabled={busyId === item.id} onClick={() => onResumeSubagent(item.id)} title="继续此子代理">
                      <Play size={10} fill="currentColor" />继续
                    </button>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </details>
  );
}

export function RuntimeMonitor({
  sessionId,
  sessionStatus,
  onOpenSession,
}: {
  sessionId: string | null;
  sessionStatus?: string;
  onOpenSession: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [data, setData] = useState<RuntimeOverview | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [refreshTick, setRefreshTick] = useState(0);

  useEffect(() => {
    let alive = true;
    const controller = new AbortController();
    const fetchOverview = async () => {
      try {
        const res = await api.runtimeOverview(sessionId, controller.signal);
        if (alive) setData(res);
      } catch {
        // Ignore transient poll errors; next tick retries.
      }
    };
    void fetchOverview();
    const timer = setInterval(() => void fetchOverview(), 3000);
    return () => {
      alive = false;
      controller.abort();
      clearInterval(timer);
    };
  }, [sessionId, sessionStatus, refreshTick]);

  if (!data) return null;

  const jobsAsItems: RuntimeItem[] = (data.jobs ?? []).map(job => ({
    id: `job:${job.id}`,
    name: `后台进程 ${job.id.slice(0, 8)} (${job.privilege})`,
    running: job.running,
    status: job.status,
    detail: `${job.bytes.toLocaleString()} bytes${job.exit_code != null ? ` · exit ${job.exit_code}` : ''}`,
  }));

  const subagents = data.subagents ?? [];
  const runningSubagents = subagents.filter(s => s.running).length;
  const lastCancelledCount = data.last_cancelled_subagents?.length ?? 0;

  const categories = [
    { key: 'services', label: '服务', items: data.services ?? [] },
    { key: 'processes', label: '进程', items: data.processes ?? [] },
    { key: 'jobs', label: '后台任务', items: jobsAsItems },
    { key: 'tasks', label: '会话任务', items: data.session_tasks ?? [] },
    { key: 'subagents', label: '子代理', items: subagents },
  ];

  const totalRunning = categories.reduce((acc, c) => acc + c.items.filter(i => i.running).length, 0);

  async function handleResumeSubagent(id: string) {
    setBusyId(id);
    try { await api.resumeSubagent(id); setRefreshTick(v => v + 1); } finally { setBusyId(null); }
  }
  async function handleCancelSubagent(id: string) {
    setBusyId(id);
    try { await api.cancelSubagent(id); setRefreshTick(v => v + 1); } finally { setBusyId(null); }
  }
  async function handleCancelAllSubagents(e: React.MouseEvent) {
    e.stopPropagation();
    setBusyId('__all__');
    try { await api.cancelAllSubagents(); setRefreshTick(v => v + 1); } finally { setBusyId(null); }
  }
  async function handleResumeLastSubagents(e: React.MouseEvent) {
    e.stopPropagation();
    setBusyId('__resume_last__');
    try { await api.resumeLastSubagents(sessionId); setRefreshTick(v => v + 1); } finally { setBusyId(null); }
  }

  return (
    <div className="runtime-bar-wrap">
      <div className="runtime-bar-row">
        <button
          type="button"
          className="runtime-bar-toggle"
          aria-expanded={open}
          onClick={() => setOpen(v => !v)}
        >
          <span className="runtime-bar-lead">
            <Activity size={13} />
            <span>运行状态</span>
            <span className={`runtime-badge ${totalRunning > 0 ? 'is-running' : 'is-ended'}`}>
              <i className="runtime-dot" />
              {totalRunning} 运行中
            </span>
          </span>
          <span className="runtime-bar-pills">
            {categories.map(cat => {
              const active = cat.items.filter(i => i.running).length;
              return (
                <span key={cat.key} className={`runtime-cat-pill ${active > 0 ? 'is-running' : 'is-ended'}`}>
                  <i className="runtime-dot" />
                  {cat.label} {active}/{cat.items.length}
                </span>
              );
            })}
          </span>
          <span className="runtime-chevron">{open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
        </button>

        {(runningSubagents > 0 || lastCancelledCount > 0) && (
          <div className="runtime-bar-quick-actions">
            {runningSubagents > 0 && (
              <button type="button" className="runtime-mini-btn danger" disabled={busyId !== null} onClick={handleCancelAllSubagents} title="停止全部运行中的子代理">
                <Square size={10} fill="currentColor" />停止子代理 ({runningSubagents})
              </button>
            )}
            {lastCancelledCount > 0 && (
              <button type="button" className="runtime-mini-btn ok" disabled={busyId !== null} onClick={handleResumeLastSubagents} title="仅继续上一次被取消的子代理">
                <RotateCcw size={10} />继续上次子代理 ({lastCancelledCount})
              </button>
            )}
          </div>
        )}
      </div>

      {open && (
        <div className="runtime-drawer">
          <GroupSection
            title="服务"
            icon={<Server size={13} />}
            items={data.services ?? []}
            emptyText="暂无服务记录。"
          />
          <GroupSection
            title="所有进程"
            icon={<Cpu size={13} />}
            items={data.processes ?? []}
            emptyText="当前无活跃会话进程。"
          />
          <GroupSection
            title="后台任务"
            icon={<Terminal size={13} />}
            items={jobsAsItems}
            emptyText="暂无后台 Bash 任务。"
          />
          <GroupSection
            title="当前会话任务"
            icon={<Layers size={13} />}
            items={data.session_tasks ?? []}
            emptyText={sessionId ? '当前会话暂无 Goal 或后台任务。' : '未选择会话。'}
          />
          <GroupSection
            title="子代理"
            icon={<GitBranch size={13} />}
            items={subagents}
            emptyText="暂无子代理。"
            defaultOpen
            wide
            busyId={busyId}
            onOpenSession={onOpenSession}
            onResumeSubagent={id => void handleResumeSubagent(id)}
            onCancelSubagent={id => void handleCancelSubagent(id)}
            headerActions={
              <>
                <button type="button" className="runtime-mini-btn danger" disabled={runningSubagents === 0 || busyId !== null} onClick={handleCancelAllSubagents}>
                  <Square size={10} fill="currentColor" />停止全部子代理
                </button>
                <button type="button" className="runtime-mini-btn ok" disabled={lastCancelledCount === 0 || busyId !== null} onClick={handleResumeLastSubagents}>
                  <RotateCcw size={10} />继续上次取消 ({lastCancelledCount})
                </button>
              </>
            }
          />
        </div>
      )}
    </div>
  );
}
