import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { ArrowRight, Check, ChevronDown, CircleHelp, FolderOpen, GitBranch, KeyRound, Lock, LogOut, Menu, MessageSquare, Moon, Plus, RefreshCw, Search, Settings2, Shield, ShieldCheck, Sun, Trash2, Unlock, X } from 'lucide-react';
import { api, errorMessage, sanitizeConfig } from './api';
import { LogoMark } from './Logo';
import { Chat, Welcome } from './Chat';
import { Settings } from './Settings';
import { Alert, Field, Modal, ModelSelect, Spinner } from './ui';
import { useSession } from './useSession';
import { activeStatus, modelOptions, sameSelection, statusLabels, type AppConfig, type Bootstrap, type Selection, type Session } from './types';

type Theme = 'light' | 'dark';
function initialTheme(): Theme { try { const saved = localStorage.getItem('justai-theme'); if (saved === 'light' || saved === 'dark') return saved; } catch { /* Storage may be disabled. */ } return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'; }
export default function App() {
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [bootstrap, setBootstrap] = useState<Bootstrap | null>(null);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(true);
  useEffect(() => { document.documentElement.dataset.theme = theme; try { localStorage.setItem('justai-theme', theme); } catch { /* Theme is optional. */ } const meta = document.querySelector('meta[name="theme-color"]'); meta?.setAttribute('content', theme === 'dark' ? '#0b1820' : '#f8f5f0'); document.querySelector('link[rel="icon"]')?.setAttribute('href', `/brand/justaiagent-${theme}.svg`); }, [theme]);
  const refresh = useCallback(async () => { setLoading(true); setError(''); try { setBootstrap(await api.bootstrap()); } catch (reason) { setError(errorMessage(reason)); } finally { setLoading(false); } }, []);
  useEffect(() => { void refresh(); const unauthorized = () => setBootstrap(current => current ? { ...current, authenticated: false } : current); window.addEventListener('justai:unauthorized', unauthorized); return () => window.removeEventListener('justai:unauthorized', unauthorized); }, [refresh]);
  const toggleTheme = () => setTheme(t => t === 'light' ? 'dark' : 'light');
  if (loading) return <div className="boot-screen"><Brand /><Spinner label="正在打开工作台" /></div>;
  if (!bootstrap) return <div className="boot-screen"><Brand /><Alert>{error || '无法连接服务器。'}</Alert><button className="button secondary" onClick={() => void refresh()}>重新连接</button></div>;
  if (typeof globalThis.crypto?.randomUUID !== 'function') return <main className="boot-screen"><Brand /><Alert>请使用 HTTPS 地址或本机 localhost 打开工作台。安全上下文与现代浏览器是保护登录信息及生成防重复请求 ID 的必要条件。</Alert><ThemeButton theme={theme} toggleTheme={toggleTheme} /></main>;
  return bootstrap.authenticated ? <Workspace bootstrap={bootstrap} theme={theme} toggleTheme={toggleTheme} onLoggedOut={() => { setBootstrap({ ...bootstrap, authenticated: false }); }} /> : <Auth bootstrap={bootstrap} theme={theme} toggleTheme={toggleTheme} onAuthenticated={refresh} />;
}
function Brand({ compact = false }: { compact?: boolean }) { return <div className={`brand ${compact ? 'compact' : ''}`}><span className="brand-mark"><LogoMark size={36} /></span><span>JustAI<span className="brand-light">Agent</span></span></div>; }
function ThemeButton({ theme, toggleTheme }: { theme: Theme; toggleTheme: () => void }) { return <button className="icon-button theme-button" onClick={toggleTheme} aria-label={theme === 'light' ? '切换到暗色主题' : '切换到明亮主题'} title={theme === 'light' ? '电光青 · 暗色' : '酒红 · 明亮'}>{theme === 'light' ? <Moon size={18} /> : <Sun size={18} />}</button>; }

function Auth({ bootstrap, theme, toggleTheme, onAuthenticated }: { bootstrap: Bootstrap; theme: Theme; toggleTheme: () => void; onAuthenticated: () => Promise<void> }) {
  const setup = !bootstrap.initialized;
  const [password, setPassword] = useState(''); const [confirmation, setConfirmation] = useState(''); const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  async function submit(event: FormEvent) {
    event.preventDefault(); if (busy) return; setError('');
    if (setup && new TextEncoder().encode(password).length < 12) { setError('访问密码至少需要 12 字节（建议使用较长的独立密码）。'); return; }
    if (setup && password !== confirmation) { setError('两次输入的密码不一致。'); return; }
    setBusy(true);
    try { await api.auth(setup ? 'setup' : 'login', password); setPassword(''); setConfirmation(''); await onAuthenticated(); }
    catch (reason) { setError(errorMessage(reason)); } finally { setBusy(false); }
  }
  return <main className="auth-page"><div className="auth-top"><Brand /><ThemeButton theme={theme} toggleTheme={toggleTheme} /></div><div className="auth-content"><section className="auth-intro"><div className="auth-decoration" aria-hidden="true"><LogoMark size={92} /></div><h1>把想法，<br />交给<em>行动。</em></h1><p>安静、直接、属于你的 AI 工作台。<br />用熟悉的模型，完成真正重要的事。</p></section><section className="auth-card"><span className="auth-card-icon"><ShieldCheck size={25} /></span><h2>{setup ? '建立你的专属空间' : '欢迎回来。'}</h2><p>{setup ? '首次使用，请设置工作台访问密码。' : '输入密码，回到你的工作现场。'}</p>{setup && !bootstrap.allow_setup ? <Alert>首次密码设置仅允许从本机访问。请在服务器本机完成初始化，再从其他设备登录。</Alert> : <form onSubmit={event => void submit(event)}>{error && <Alert>{error}</Alert>}<Field label={setup ? '设置访问密码' : '访问密码'}><input type="password" required autoFocus autoComplete={setup ? 'new-password' : 'current-password'} value={password} onChange={e => setPassword(e.target.value)} placeholder={setup ? '至少 12 字节，建议使用较长密码' : '输入你的密码'} disabled={busy} /></Field>{setup && <Field label="确认密码"><input type="password" required autoComplete="new-password" value={confirmation} onChange={e => setConfirmation(e.target.value)} placeholder="再次输入密码" disabled={busy} /></Field>}<button className="button primary auth-submit" disabled={busy}>{busy ? <Spinner label={setup ? '正在初始化' : '正在登录'} /> : <>{setup ? '创建工作台' : '进入工作台'}<ArrowRight size={17} /></>}</button></form>}<div className="auth-card-foot"><ShieldCheck size={13} />Cookie 安全认证 · 密码不保存在浏览器存储</div></section></div><footer className="auth-footer"><span>JustAIAgent</span><span>v{bootstrap.version}</span></footer></main>;
}

function Workspace({ bootstrap, theme, toggleTheme, onLoggedOut }: { bootstrap: Bootstrap; theme: Theme; toggleTheme: () => void; onLoggedOut: () => void }) {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [loadError, setLoadError] = useState('');
  const [query, setQuery] = useState('');
  const [settings, setSettings] = useState(false);
  const [creating, setCreating] = useState(false);
  const [sidebar, setSidebar] = useState(false);
  const [isMobile, setIsMobile] = useState(() => window.matchMedia('(max-width: 680px)').matches);
  useEffect(() => { const media = window.matchMedia('(max-width: 680px)'); const update = () => setIsMobile(media.matches); media.addEventListener('change', update); return () => media.removeEventListener('change', update); }, []);
  const [loggingOut, setLoggingOut] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ session: Session; x: number; y: number } | null>(null);
  const [pwModal, setPwModal] = useState<{ session: Session; mode: 'full' | 'unlock' | 'decrypt' | 'half' } | null>(null);
  const [menuBusy, setMenuBusy] = useState(false);
  const updateSession = useCallback((session: Session) => setSessions(current => { const existing = current.some(s => s.id === session.id); return existing ? current.map(s => s.id === session.id ? session : s) : [...current, session]; }), []);
  const detail = useSession(selectedId, updateSession);
  const load = useCallback(async () => {
    setRefreshing(true); setLoadError('');
    const [configuration, list] = await Promise.allSettled([api.config(), api.sessions()]);
    const errors: string[] = [];
    if (configuration.status === 'fulfilled') setConfig(sanitizeConfig(configuration.value)); else errors.push(`配置：${errorMessage(configuration.reason)}`);
    if (list.status === 'fulfilled') setSessions(list.value); else errors.push(`会话：${errorMessage(list.reason)}`);
    setLoadError(errors.join('；')); setLoading(false); setRefreshing(false);
  }, []);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => { if (!sidebar) return; const handler = (event: KeyboardEvent) => { if (event.key === 'Escape') setSidebar(false); }; window.addEventListener('keydown', handler); return () => window.removeEventListener('keydown', handler); }, [sidebar]);
  useEffect(() => {
    if (!contextMenu) return;
    const close = () => setContextMenu(null);
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') close(); };
    window.addEventListener('click', close);
    window.addEventListener('keydown', onKey);
    return () => { window.removeEventListener('click', close); window.removeEventListener('keydown', onKey); };
  }, [contextMenu]);
  const options = modelOptions(config);
  const selected = detail.session;
  const model = selected ? options.find(o => sameSelection(o.selection, selected.selection)) : undefined;
  const filtered = sessions.filter(s => `${s.title} ${s.cwd}`.toLocaleLowerCase().includes(query.toLocaleLowerCase())).sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at));
  function openSession(id: string) { setSelectedId(id); setSidebar(false); }
  async function handleEncryption(session: Session, mode: 'none' | 'half' | 'full', password?: string) {
    setMenuBusy(true); setLoadError('');
    try {
      const updated = await api.setSessionEncryption(session.id, mode, password);
      updateSession(updated);
      if (selectedId === session.id) detail.reload();
    } catch (reason) {
      setLoadError(errorMessage(reason));
    } finally { setMenuBusy(false); }
  }
  async function handleLock(session: Session) {
    setMenuBusy(true); setLoadError('');
    try {
      const updated = await api.lockSession(session.id);
      updateSession(updated);
      if (selectedId === session.id) detail.reload();
    } catch (reason) {
      setLoadError(errorMessage(reason));
    } finally { setMenuBusy(false); }
  }
  async function handleDelete(session: Session) {
    if (!confirm(`确定彻底删除会话“${session.title || '未命名会话'}”吗？\n数据库记录与磁盘会话文件将被永久清除，无法恢复。`)) return;
    setMenuBusy(true); setLoadError('');
    try {
      await api.deleteSession(session.id);
      setSessions(current => current.filter(s => s.id !== session.id));
      if (selectedId === session.id) setSelectedId(null);
    } catch (reason) {
      setLoadError(errorMessage(reason));
    } finally { setMenuBusy(false); }
  }
  function openSettings() { if (!config) { void load(); return; } setSettings(true); setSidebar(false); }
  function openCreate() { if (!options.length) { openSettings(); return; } setCreating(true); setSidebar(false); }
  async function logout() { setLoggingOut(true); setLoadError(''); try { await api.logout(); onLoggedOut(); } catch (reason) { setLoadError(errorMessage(reason)); } finally { setLoggingOut(false); } }
  return <div className="workspace">
    {sidebar && <div className="sidebar-scrim" onClick={() => setSidebar(false)} aria-hidden="true" />}
    <aside className={`sidebar ${sidebar ? 'is-open' : ''}`} aria-label="会话侧栏" inert={isMobile && !sidebar}><div className="sidebar-brand"><button className="brand-home" onClick={() => { setSelectedId(null); setSidebar(false); }} aria-label="返回首页"><Brand compact /></button><button className="icon-button mobile-only" onClick={() => setSidebar(false)} aria-label="关闭侧栏"><X size={19} /></button></div><button className="button primary new-session" onClick={openCreate} disabled={loading}><Plus size={18} />新建会话<span className="new-session-symbol">↗</span></button><div className="sidebar-search"><Search size={15} /><input value={query} onChange={e => setQuery(e.target.value)} placeholder="搜索会话" aria-label="搜索会话" /></div><div className="sidebar-list-heading"><span>你的会话 {sessions.length > 0 && <small>{sessions.length}</small>}</span><button className="icon-button" onClick={() => void load()} disabled={refreshing} title="刷新会话与配置" aria-label="刷新会话与配置"><RefreshCw size={13} className={refreshing ? 'spin' : ''} /></button></div><div className="session-list">{loading ? <div className="sidebar-empty"><Spinner label="读取会话" /></div> : !filtered.length ? <div className="sidebar-empty"><MessageSquare size={23} /><p>{query ? '没有匹配的会话' : loadError ? '暂时无法读取会话' : '还没有会话'}</p><small>{query ? '换个关键词试试' : '新的想法，值得一个开始。'}</small></div> : filtered.map(session => <button className={`session-item ${selectedId === session.id ? 'selected' : ''} ${session.parent_id ? 'child-session' : ''}`} key={session.id} onClick={() => openSession(session.id)} onContextMenu={e => { e.preventDefault(); e.stopPropagation(); setContextMenu({ session, x: Math.min(e.clientX, window.innerWidth - 190), y: Math.min(e.clientY, window.innerHeight - 220) }); }}>{session.parent_id ? <GitBranch size={15} /> : <MessageSquare size={15} />}<span><strong>{session.title || '未命名会话'}{session.encryption === 'full' && <Lock size={11} className="session-enc-icon" aria-label="全加密" />}{session.encryption === 'half' && <Shield size={11} className="session-enc-icon" aria-label="半加密" />}</strong><small>{session.parent_id ? '子 Agent · ' : ''}{dateLabel(session.updated_at)} · {session.locked ? '已锁定' : statusLabels[session.status] ?? session.status}</small></span>{activeStatus(session.status) && <span className="session-running-dot" />}</button>)}</div><div className="sidebar-footer"><button className="sidebar-action" onClick={openSettings} disabled={loading}><Settings2 size={18} /><span>工作台设置</span><ChevronDown size={13} className="settings-arrow" /></button><div className="sidebar-bottom"><span className="local-badge"><span />个人工作空间</span><ThemeButton theme={theme} toggleTheme={toggleTheme} /><button className="icon-button" title="退出登录" aria-label="退出登录" disabled={loggingOut} onClick={() => void logout()}><LogOut size={16} /></button></div><span className="version-text">JUSTAIAGENT <span>v{bootstrap.version}</span></span></div></aside>
    <main className="main-panel"><header className="topbar"><div className="topbar-left"><button className="icon-button mobile-only" onClick={() => setSidebar(true)} aria-label="打开会话侧栏"><Menu size={21} /></button><div className="topbar-title"><strong>{selected?.title || (selectedId ? '读取会话…' : '工作台')}</strong>{selected ? <span className="cwd-label" title={selected.cwd}><FolderOpen size={12} />{selected.cwd}</span> : null}</div></div><div className="topbar-right">{selected ? <span className={`session-status ${activeStatus(selected.status) ? 'active' : ''}`}><span />{selected.locked ? '已锁定' : statusLabels[selected.status] ?? selected.status}</span> : null}<span className="topbar-divider" /><button className="icon-button" title="工作台设置" aria-label="打开设置" onClick={openSettings}><Settings2 size={18} /></button><div className="mobile-only"><ThemeButton theme={theme} toggleTheme={toggleTheme} /></div></div></header>
      {loadError && <div className="workspace-error"><Alert onClose={() => setLoadError('')}>{loadError}<button className="text-button" disabled={refreshing} onClick={() => void load()}>重试加载</button></Alert></div>}
      {loading ? <div className="chat-loading"><Spinner label="准备你的工作台" /></div> : selectedId && selected?.locked ? <SessionLockedView session={selected} onUnlocked={updated => { updateSession(updated); detail.reload(); }} /> : selectedId ? <Chat key={selectedId} session={selected} events={detail.events} model={model} config={config} onSettingsSaved={detail.updateSettings} connection={detail.connection} error={detail.error} onReload={detail.reload} onOpenSession={openSession} onOpenSettings={openSettings} /> : <Welcome onCreate={openCreate} onSettings={openSettings} hasModels={options.length > 0} />}
    </main>
    {contextMenu && <div className="session-context-menu" style={{ left: contextMenu.x, top: contextMenu.y }} role="menu" onClick={e => e.stopPropagation()}>
      <button type="button" role="menuitem" onClick={() => { openSession(contextMenu.session.id); setContextMenu(null); }}><MessageSquare size={14} />打开会话</button>
      {contextMenu.session.encryption === 'full' ? <>
        {contextMenu.session.locked ? <button type="button" role="menuitem" onClick={() => { setPwModal({ session: contextMenu.session, mode: 'unlock' }); setContextMenu(null); }}><Unlock size={14} />输入密码解锁</button>
          : <button type="button" role="menuitem" disabled={menuBusy} onClick={() => { void handleLock(contextMenu.session); setContextMenu(null); }}><Lock size={14} />立即锁定</button>}
        <button type="button" role="menuitem" disabled={menuBusy} onClick={() => { if (contextMenu.session.locked) { setPwModal({ session: contextMenu.session, mode: 'decrypt' }); } else { void handleEncryption(contextMenu.session, 'none'); } setContextMenu(null); }}><Unlock size={14} />解除全加密</button>
        <button type="button" role="menuitem" disabled={menuBusy} onClick={() => { if (contextMenu.session.locked) { setPwModal({ session: contextMenu.session, mode: 'half' }); } else { void handleEncryption(contextMenu.session, 'half'); } setContextMenu(null); }}><Shield size={14} />转为半加密</button>
      </> : contextMenu.session.encryption === 'half' ? <>
        <button type="button" role="menuitem" disabled={menuBusy} onClick={() => { setPwModal({ session: contextMenu.session, mode: 'full' }); setContextMenu(null); }}><Lock size={14} />全加密（设密码）</button>
        <button type="button" role="menuitem" disabled={menuBusy} onClick={() => { void handleEncryption(contextMenu.session, 'none'); setContextMenu(null); }}><Unlock size={14} />解除半加密</button>
      </> : <>
        <button type="button" role="menuitem" disabled={menuBusy} onClick={() => { setPwModal({ session: contextMenu.session, mode: 'full' }); setContextMenu(null); }}><Lock size={14} />全加密（设密码）</button>
        <button type="button" role="menuitem" disabled={menuBusy} onClick={() => { void handleEncryption(contextMenu.session, 'half'); setContextMenu(null); }}><Shield size={14} />半加密（AES-256）</button>
      </>}
      <div className="context-menu-sep" />
      <button type="button" role="menuitem" className="danger-item" disabled={menuBusy} onClick={() => { const s = contextMenu.session; setContextMenu(null); void handleDelete(s); }}><Trash2 size={14} />彻底删除</button>
    </div>}
    {pwModal && <SessionPasswordModal session={pwModal.session} mode={pwModal.mode} onClose={() => setPwModal(null)} onDone={updated => { updateSession(updated); if (selectedId === updated.id) detail.reload(); setPwModal(null); }} />}
    {settings && config && <Settings config={config} onSaved={setConfig} onClose={() => setSettings(false)} onLoggedOut={onLoggedOut} />}
    {creating && config && <CreateSession config={config} workspace={bootstrap.workspace} onClose={() => setCreating(false)} onCreated={session => { updateSession(session); openSession(session.id); setCreating(false); }} />}
  </div>;
}
function dateLabel(date: string) { const parsed = new Date(date); if (Number.isNaN(parsed.getTime())) return ''; const today = new Date(); return parsed.toDateString() === today.toDateString() ? parsed.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : parsed.toLocaleDateString([], { month: 'short', day: 'numeric' }); }
function CreateSession({ config, workspace, onClose, onCreated }: { config: AppConfig; workspace?: string; onClose: () => void; onCreated: (session: Session) => void }) {
  const [title, setTitle] = useState(''); const [cwd, setCwd] = useState(workspace ?? ''); const [selection, setSelection] = useState<Selection | null>(modelOptions(config)[0]?.selection ?? null);
  const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  const selected = modelOptions(config).find(o => selection && sameSelection(o.selection, selection));
  async function submit(event: FormEvent) { event.preventDefault(); if (!selection || busy) return; setBusy(true); setError(''); try { onCreated(await api.createSession({ ...(title.trim() ? { title: title.trim() } : {}), ...(cwd.trim() ? { cwd: cwd.trim() } : {}), selection })); } catch (reason) { setError(errorMessage(reason)); } finally { setBusy(false); } }
  return <Modal title="开启新的会话" subtitle="选择模型与工作目录，开始专注的一件事。" onClose={() => { if (!busy) onClose(); }} className="create-modal"><form className="create-form" onSubmit={event => void submit(event)}>{error && <Alert>{error}</Alert>}<Field label="会话名称（可选）"><input value={title} onChange={e => setTitle(e.target.value)} placeholder="这次，想完成什么？" disabled={busy} /></Field><Field label="模型连接"><ModelSelect config={config} value={selection} onChange={setSelection} disabled={busy} /></Field>{selected && <div className="selected-model-info"><span><Check size={13} />Responses API</span><span>{selected.model.supports_images ? '支持图片' : '仅文本'}</span>{selected.model.reasoning_effort && <span>思考：{selected.model.reasoning_effort}</span>}</div>}<Field label="工作目录" hint="服务器上的目录；留空使用后端默认工作目录。"><input value={cwd} onChange={e => setCwd(e.target.value)} placeholder="例如 /home/you/project" disabled={busy} spellCheck={false} /></Field><p className="create-warning"><CircleHelp size={15} />Agent 拥有启动用户的文件与终端权限，请确认工作目录与任务范围。</p><div className="button-row form-actions"><button className="button ghost" type="button" disabled={busy} onClick={onClose}>取消</button><button className="button primary" disabled={busy || !selection}>{busy ? <Spinner label="创建中" /> : <><Plus size={16} />创建会话</>}</button></div></form></Modal>;
}
function SessionLockedView({ session, onUnlocked }: { session: Session; onUnlocked: (session: Session) => void }) {
  const [password, setPassword] = useState(''); const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  async function submit(event: FormEvent) {
    event.preventDefault(); if (!password || busy) return; setBusy(true); setError('');
    try { const updated = await api.unlockSession(session.id, password); setPassword(''); onUnlocked(updated); }
    catch (reason) { setError(errorMessage(reason)); } finally { setBusy(false); }
  }
  return <div className="session-lock-panel"><div className="session-lock-card"><span className="auth-card-icon"><Lock size={24} /></span><h3>{session.title || '全加密会话'}</h3><p>此会话已启用 AES-256-GCM 全加密，输入会话密码后可访问内容。</p><form onSubmit={event => void submit(event)}>{error && <Alert>{error}</Alert>}<Field label="会话密码"><input type="password" required autoFocus value={password} onChange={e => setPassword(e.target.value)} placeholder="输入会话密码" disabled={busy} /></Field><button className="button primary" disabled={busy || !password}>{busy ? <Spinner label="正在解锁" /> : <><KeyRound size={15} />解锁会话</>}</button></form></div></div>;
}
function SessionPasswordModal({ session, mode, onClose, onDone }: { session: Session; mode: 'full' | 'unlock' | 'decrypt' | 'half'; onClose: () => void; onDone: (session: Session) => void }) {
  const [password, setPassword] = useState(''); const [confirm, setConfirm] = useState(''); const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  const title = mode === 'full' ? '设置会话全加密' : mode === 'unlock' ? '解锁全加密会话' : mode === 'half' ? '转为半加密' : '解除全加密';
  const subtitle = mode === 'full' ? '启用后需输入该密码才能在工作台访问此会话。' : mode === 'unlock' ? '输入会话密码以解锁当前会话。' : '输入当前会话密码以验证并解密。';
  async function submit(event: FormEvent) {
    event.preventDefault(); if (busy) return; setError('');
    if (!password) { setError('请输入密码。'); return; }
    if (mode === 'full' && password !== confirm) { setError('两次输入的密码不一致。'); return; }
    setBusy(true);
    try {
      const updated = mode === 'unlock' ? await api.unlockSession(session.id, password) : await api.setSessionEncryption(session.id, mode === 'decrypt' ? 'none' : mode, password);
      onDone(updated);
    } catch (reason) { setError(errorMessage(reason)); } finally { setBusy(false); }
  }
  return <Modal title={title} subtitle={`${session.title || '未命名会话'} · ${subtitle}`} onClose={() => { if (!busy) onClose(); }} className="create-modal"><form className="create-form" onSubmit={event => void submit(event)}>{error && <Alert>{error}</Alert>}<Field label="会话密码"><input type="password" required autoFocus value={password} onChange={e => setPassword(e.target.value)} placeholder={mode === 'full' ? '设置独立密码' : '输入会话密码'} disabled={busy} /></Field>{mode === 'full' && <Field label="确认密码"><input type="password" required value={confirm} onChange={e => setConfirm(e.target.value)} placeholder="再次输入密码" disabled={busy} /></Field>}<div className="button-row form-actions"><button className="button ghost" type="button" disabled={busy} onClick={onClose}>取消</button><button className="button primary" disabled={busy || !password}>{busy ? <Spinner label="处理中" /> : '确认'}</button></div></form></Modal>;
}
