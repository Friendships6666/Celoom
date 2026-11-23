import { useEffect, useRef, useState } from 'react';
import { ExternalLink, Globe2, LoaderCircle, Monitor, RefreshCw, ShieldCheck, Square, TriangleAlert } from 'lucide-react';
import { api, computerImageUrl, errorMessage } from './api';
import { computerCapability, latestComputerActivity, parseComputerStatus } from './computer';
import type { AgentEvent, ComputerActivity, ComputerStatus } from './types';
import { Alert, Modal, Spinner } from './ui';

export function ComputerCapture({ activity, compact = false }: { activity: ComputerActivity; compact?: boolean }) {
  const [failed, setFailed] = useState(false);
  const url = activity.capture_id ? computerImageUrl(activity.session_id, activity.capture_id) : '';
  useEffect(() => { setFailed(false); }, [url]);
  if (!url) return null;
  return <figure className={`computer-capture ${compact ? 'compact' : ''}`}>
    {failed ? <div className="capture-error" role="alert">截图无法读取，可能已过期或需要重新登录。<button className="text-button" onClick={() => setFailed(false)}>重试截图</button></div> : <a href={url} target="_blank" rel="noopener noreferrer" aria-label="查看计算机截图原图"><img src={url} alt={`${activity.target === 'browser' ? '隔离浏览器' : '计算机'}截图`} loading="lazy" onError={() => setFailed(true)} /></a>}
    <figcaption><span>会话截图{activity.width && activity.height ? ` · ${activity.width} × ${activity.height}` : ''}</span><a href={url} target="_blank" rel="noopener noreferrer">查看原图<ExternalLink size={12} /></a></figcaption>
  </figure>;
}
function Capability({ title, hint, value, browser = false }: { title: string; hint: string; value: unknown; browser?: boolean }) {
  const capability = computerCapability(value); const Icon = browser ? Globe2 : Monitor;
  return <article className="computer-layer"><span className="computer-layer-icon"><Icon size={19} /></span><div><div className="computer-layer-title"><strong>{title}</strong><span className={`computer-chip ${capability.tone}`}>{capability.label}</span></div><p>{hint}</p><small>{capability.detail}</small></div></article>;
}
export function ComputerPanel({ sessionId, events, onClose, authorizedBefore, onAuthorized }: { sessionId: string; events: AgentEvent[]; onClose: () => void; authorizedBefore: boolean; onAuthorized: () => void }) {
  const [status, setStatus] = useState<ComputerStatus | null>(null);
  const [loading, setLoading] = useState(true); const [statusError, setStatusError] = useState('');
  const [actionError, setActionError] = useState(''); const [notice, setNotice] = useState('');
  const [stopping, setStopping] = useState(false); const [hostBusy, setHostBusy] = useState(false);
  const [authorizing, setAuthorizing] = useState(false); const [authorized, setAuthorized] = useState(false);
  const [cookieSummary, setCookieSummary] = useState<{ count: number; domains: string[] } | null>(null);
  const [cookieInput, setCookieInput] = useState('');
  const [cookieUrl, setCookieUrl] = useState('https://www.jd.com');
  const [cookieBusy, setCookieBusy] = useState(false);
  const [refresh, setRefresh] = useState(0); const alive = useRef(true); const stopLock = useRef(false); const hostLock = useRef(false); const hostAuthorized = useRef(authorizedBefore);
  const activity = latestComputerActivity(events, sessionId); const capture = latestComputerActivity(events, sessionId, true);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  useEffect(() => {
    const controller = new AbortController();
    void api.computerCookies(controller.signal).then(res => { if (alive.current) setCookieSummary(res); }).catch(() => {});
    return () => controller.abort();
  }, [refresh]);
  async function handleImportCookies() {
    if (!cookieInput.trim()) return;
    setCookieBusy(true); setActionError(''); setNotice('');
    try {
      const trimmed = cookieInput.trim();
      const payload = trimmed.startsWith('[')
        ? { cookies_json: trimmed, url: cookieUrl.trim() || undefined }
        : { cookie_header: trimmed, url: cookieUrl.trim() };
      const res = await api.importComputerCookies(payload);
      if (alive.current) {
        setCookieSummary({ count: res.count, domains: res.domains });
        setCookieInput('');
        setNotice(`已导入 ${res.count} 条 Cookie（${res.domains.join('、')}），AI 浏览器将自动携带该登录态。`);
      }
    } catch (reason) { if (alive.current) setActionError(errorMessage(reason)); }
    finally { if (alive.current) setCookieBusy(false); }
  }
  async function handleClearCookies() {
    setCookieBusy(true); setActionError(''); setNotice('');
    try {
      await api.clearComputerCookies();
      if (alive.current) { setCookieSummary({ count: 0, domains: [] }); setNotice('已清除导入的 AI 浏览器 Cookie。'); }
    } catch (reason) { if (alive.current) setActionError(errorMessage(reason)); }
    finally { if (alive.current) setCookieBusy(false); }
  }
  useEffect(() => {
    const controller = new AbortController(); let disposed = false; let inFlight = false;
    async function load() {
      if (inFlight) return; inFlight = true;
      try { const next = parseComputerStatus(await api.computer(controller.signal)); if (!disposed) { setStatus(next); setStatusError(''); } }
      catch (reason) { if (!disposed) { setStatus(null); setStatusError(errorMessage(reason)); } }
      finally { inFlight = false; if (!disposed) setLoading(false); }
    }
    setLoading(true); void load(); const interval = window.setInterval(() => { if (!document.hidden) void load(); }, 5000);
    return () => { disposed = true; controller.abort(); window.clearInterval(interval); };
  }, [refresh]);
  async function stop() {
    if (stopLock.current) return; stopLock.current = true; setStopping(true); setActionError(''); setNotice('');
    try { await api.stopComputer(); if (alive.current) { setNotice('已发送全局紧急停止，正在刷新实际状态。'); setStatus(null); setRefresh(value => value + 1); } }
    catch (reason) { if (alive.current) setActionError(`紧急停止失败：${errorMessage(reason)}`); }
    finally { stopLock.current = false; if (alive.current) setStopping(false); }
  }
  async function setHost(enabled: boolean, explicitAuthorization = false) {
    if (hostLock.current || stopLock.current || !status || loading || status.busy) return;
    if (enabled && !hostAuthorized.current && !explicitAuthorization) { setAuthorizing(true); return; }
    if (explicitAuthorization) { if (!authorized) return; hostAuthorized.current = true; onAuthorized(); }
    hostLock.current = true; setHostBusy(true); setActionError(''); setNotice('');
    try { await api.computerHost(enabled); if (alive.current) { setAuthorizing(false); setStatus(null); setRefresh(value => value + 1); } }
    catch (reason) { if (alive.current) setActionError(`宿主接管设置失败：${errorMessage(reason)}`); }
    finally { hostLock.current = false; if (alive.current) setHostBusy(false); }
  }
  const hostDisabled = !status || loading || hostBusy || stopping || status.busy;
  return <Modal title="计算机" subtitle="隔离优先 · 按需启动 · 人的输入始终优先" onClose={onClose} className="computer-modal">
    <div className="computer-panel">
      <div className="computer-safety-bar"><span><ShieldCheck size={17} />查看状态不会启动浏览器或桌面</span><button className="icon-button" aria-label="刷新计算机状态" disabled={loading} onClick={() => setRefresh(value => value + 1)}><RefreshCw size={15} className={loading ? 'spin' : ''} /></button></div>
      <button className="computer-emergency" onClick={() => void stop()} disabled={stopping}>{stopping ? <LoaderCircle size={18} className="spin" /> : <Square size={15} fill="currentColor" />}<span><strong>紧急停止所有计算机操作</strong><small>全局停止 · 包括其他会话 · 无需再次确认</small></span></button>
      {actionError && <Alert>{actionError}</Alert>}{notice && <Alert tone="info">{notice}</Alert>}
      {statusError && <Alert>状态读取失败：{statusError} 不将未知状态视为可用。<button className="text-button" onClick={() => setRefresh(value => value + 1)}>重试状态</button></Alert>}
      {loading && <Spinner label="读取计算机状态" />}
      <div className="computer-runtime" aria-live="polite"><span className={`computer-chip ${status?.busy ? 'unavailable' : ''}`}>{!status ? '状态未确认' : status.busy ? '计算机忙碌' : '当前空闲'}</span>{status?.owner && <small>{status.owner === sessionId ? '本会话持有' : '其他会话持有'}</small>}</div>
      <div className="computer-layers">
        <Capability title="01 · DOM 浏览器" hint="持久化 AI 浏览器 Profile，支持登录态保存与 Cookie 导入、eval 提取、hover、wait、back。" value={status?.browser} browser />
        <Capability title="02 · 独立桌面" hint="原生应用在独立 KWin 桌面运行；语义控件优先，截图坐标备用。" value={status?.desktop} />
        <article className="computer-layer host-layer"><span className="computer-layer-icon"><Monitor size={19} /></span><div><div className="computer-layer-title"><strong>03 · 宿主备用</strong><span className="computer-chip">{!status ? '状态未确认' : status.host.enabled ? '已授权' : '默认关闭'}</span></div><p>仅在你授权后操作主桌面；检测到人的输入时自动让行。</p><div className="computer-host-row"><label><span>允许宿主接管</span><input type="checkbox" role="switch" aria-label="允许宿主接管" checked={status?.host.enabled === true} disabled={hostDisabled} onChange={event => void setHost(event.target.checked)} /></label>{hostBusy && <LoaderCircle size={14} className="spin" />}</div>{status?.busy && <small>操作忙碌期间不可切换；需要立即中断时请使用上方紧急停止。</small>}
          {authorizing && !status?.host.enabled && <div className="host-authorization"><TriangleAlert size={17} /><div><strong>授权操作你的主桌面</strong><p>Agent 将可能点击、输入并操作当前主桌面的应用，必要时会抢占现有接管。人的键鼠输入优先，AI 会自动让行。仅开启备用权限，不会立即启动操作。</p><label><input type="checkbox" checked={authorized} onChange={event => setAuthorized(event.target.checked)} />我已了解并允许宿主接管</label><div className="button-row"><button className="button secondary" disabled={hostBusy} onClick={() => setAuthorizing(false)}>暂不开启</button><button className="button primary" disabled={!authorized || hostDisabled} onClick={() => void setHost(true, true)}>授权并开启</button></div></div></div>}
          {status && <small className="computer-host-meta">{status.host.physical_monitor ? '有人输入监测可用' : '人输入监测未就绪'} · {status.host.human_idle_ms === null ? '空闲时间未知' : `已空闲 ${Math.floor(status.host.human_idle_ms / 1000)} 秒`}{status.host.lease_owner ? ` · ${status.host.lease_owner === sessionId ? '本会话租约' : '其他会话租约'}剩余 ${Math.ceil(status.host.lease_remaining_secs)} 秒` : ' · 无活跃租约'}</small>}
        </div></article>
      </div>
      <details className="computer-cookies" style={{ margin: '12px 0', padding: '10px 12px', border: '1px solid var(--line)', borderRadius: 10 }}>
        <summary style={{ cursor: 'pointer', fontWeight: 600, fontSize: 12 }}>AI 浏览器登录态 / Cookie 导入（已存 {cookieSummary?.count ?? 0} 条{cookieSummary?.domains?.length ? ` · ${cookieSummary.domains.join('、')}` : ''}）</summary>
        <p className="field-hint" style={{ margin: '8px 0' }}>支持粘贴 Cookie-Editor 导出的 JSON 数组，或直接粘贴请求头里的 Cookie 字符串（如京东登录后 F12 复制 Cookie）。Cookie 仅保存于本地私有目录（0600 权限）。</p>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
          <input value={cookieUrl} onChange={e => setCookieUrl(e.target.value)} placeholder="目标网址（仅粘贴 Cookie 字符串时必填，如 https://www.jd.com）" />
          <textarea rows={3} value={cookieInput} onChange={e => setCookieInput(e.target.value)} placeholder="粘贴 Cookie JSON 数组 [...] 或 Cookie 字符串 k1=v1; k2=v2" />
          <div className="button-row">
            <button type="button" className="button primary" disabled={cookieBusy || !cookieInput.trim()} onClick={() => void handleImportCookies()}>导入并应用 Cookie</button>
            <button type="button" className="button secondary" disabled={cookieBusy || !cookieSummary?.count} onClick={() => void handleClearCookies()}>清空已导入 Cookie</button>
          </div>
        </div>
      </details>
      <div className="computer-privilege"><ShieldCheck size={15} /><span>{status ? `实际权限：${status.privilege.mode === 'root' && status.privilege.uid === 0 ? 'root 进程' : status.privilege.mode === 'sudo' ? '普通进程 · sudo 模式' : '普通用户'} · UID ${status.privilege.uid} · ${status.privilege.root_available ? 'root 能力可用' : 'root 不可用'}` : '实际权限：未确认'}<small>独立桌面隔离输入，不是文件或网络安全沙箱；锁屏及安全界面不保证可控。</small></span></div>
      <section className="computer-latest" aria-label="本会话计算机活动"><div className="computer-section-title"><strong>本会话最近活动</strong><small>不显示其他会话</small></div>{activity ? <p className={activity.is_error ? 'danger-text' : ''}><code>{activity.action} · {activity.target}</code>{activity.summary}</p> : <p className="muted">还没有计算机事件。将任务发给 Agent 后，工具会按需启动。</p>}{capture ? <ComputerCapture activity={capture} /> : <div className="computer-no-capture"><Monitor size={24} /><span>暂无本会话截图</span></div>}</section>
    </div>
  </Modal>;
}
