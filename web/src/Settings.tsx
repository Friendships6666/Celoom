import { QualityChecks } from './QualityChecks';
import { useEffect, useState, type FormEvent } from 'react';
import { ChevronRight, GitBranch, Globe2, KeyRound, LockKeyhole, Network, Plus, Save, Search, ShieldCheck, Sparkles, Trash2, Zap } from 'lucide-react';
import { AutomatonPreview } from './AutomatonBg';
import { ANIMATION_STYLES, computeWorkloadDynamics, type AnimationStyleId, type LiveWorkload } from './automatonStyles';
import { api, ApiError, errorMessage, sanitizeConfig } from './api';
import { Alert, Field, Modal, ModelSelect, Spinner, Toggle } from './ui';
import { defaultSearchConfig, modelOptions, sameSelection, type ApiKey, type AppConfig, type Diagnostic, type ModelConfig, type NetworkConfig, type SearchConfig, type Selection, type Site, type SubagentModel } from './types';
import { poolLimit, selectSubagentModel, validateSubagentModels } from './subagentModels';

const efforts = ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'];
const newModel = (): ModelConfig => ({ id: crypto.randomUUID(), name: '', model: '', supports_images: false, max_output_tokens: 16384, context_window: 128000, auto_compact_percent: 95, reasoning_efforts: [], reasoning_effort: '', strict_tools: true, omit_tool_strict: true, reasoning_replay: false, send_store: true, cache_routing: true });
const newKey = (): ApiKey => ({ id: crypto.randomUUID(), name: '新 Key', models: [] });
const clone = (config: AppConfig) => structuredClone(sanitizeConfig(config));

function validate(config: AppConfig): string {
  for (const site of config.sites) {
    if (!site.name.trim()) return '请为每个站点填写名称。';
    try { const url = new URL(site.base_url); if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) return '站点地址必须是无账号密码、查询参数和片段的 HTTP(S) URL。'; } catch { return `请填写「${site.name}」的有效 API 地址。`; }
    for (const key of site.keys) {
      if (!key.name.trim()) return `请为「${site.name}」中的 Key 填写名称。`;
      if (key.secret && /^[*•●·…]+$/.test(key.secret.trim())) return '不能将掩码保存为 API Key。请留空以保留原值。';
      for (const model of key.models) {
        if (!model.name.trim() || !model.model.trim()) return `请填写「${key.name}」中模型的显示名称和 API 模型标识。`;
        if (![model.max_output_tokens, model.context_window].every(n => Number.isInteger(n) && n > 0 && n <= 4294967295)) return 'Token 数量必须是有效的正整数。';
        const compactPct = model.auto_compact_percent ?? 95;
        if (!Number.isInteger(compactPct) || compactPct < 20 || compactPct > 100) return '自动压缩阈值必须是 20–100 之间的整数百分比。';
        if (model.reasoning_effort && !model.reasoning_efforts.includes(model.reasoning_effort)) return '默认思考等级必须在已选等级中。';
      }
    }
  }
  for (const [name, value] of Object.entries(config.network)) {
    if (typeof value === 'number' && (!Number.isInteger(value) || value < 1)) return `网络参数 ${name} 必须是正整数。`;
  }
  if (config.network.proxy_mode === 'manual') {
    try { const url = new URL(config.network.proxy_url); if (!['http:', 'https:', 'socks5:', 'socks5h:'].includes(url.protocol)) return '代理仅支持 HTTP(S) 或 SOCKS5 地址。'; } catch { return '请填写有效的手动代理地址。'; }
  }
  const search = config.search ?? defaultSearchConfig();
  if (search.base_url.trim()) {
    try { const url = new URL(search.base_url); if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) return '搜索地址须为无账号密码、查询参数和片段的 HTTP(S) URL。'; } catch { return '请填写有效的搜索 API 地址。'; }
  } else if (search.provider === 'searxng') return '请填写 SearXNG 地址。';
  if (search.api_key && /^[*•●·…]+$/.test(search.api_key.trim())) return '请填写真实的搜索密钥，或留空保留原值。';
  return '';
}

export function Settings({ config, onSaved, onClose, onLoggedOut }: { config: AppConfig; onSaved: (config: AppConfig) => void; onClose: () => void; onLoggedOut: () => void }) {
  const [qualitySelection, setQualitySelection] = useState<{ selection: Selection; name: string } | null>(null);
  const [draft, setDraft] = useState<AppConfig>(() => clone(config));
  const [baseline, setBaseline] = useState(() => JSON.stringify(clone(config)));
  const [tab, setTab] = useState<'models' | 'subagents' | 'network' | 'animation' | 'security'>('models');
  const [siteId, setSiteId] = useState(config.sites[0]?.id ?? '');
  const [keyId, setKeyId] = useState('');
  const [modelId, setModelId] = useState('');
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState(false);
  const [conflict, setConflict] = useState(false);
  const dirty = JSON.stringify(draft) !== baseline;
  const site = draft.sites.find(s => s.id === siteId);
  const key = site?.keys.find(k => k.id === keyId) ?? site?.keys[0];
  const model = key?.models.find(m => m.id === modelId) ?? key?.models[0];
  useEffect(() => {
    if (!dirty) return;
    const handler = (event: BeforeUnloadEvent) => { event.preventDefault(); };
    window.addEventListener('beforeunload', handler); return () => window.removeEventListener('beforeunload', handler);
  }, [dirty]);
  function changeSite(patch: Partial<Site>) { setDraft(d => ({ ...d, sites: d.sites.map(s => s.id === siteId ? { ...s, ...patch } : s) })); setNotice(''); }
  function changeKey(patch: Partial<ApiKey>) { if (site && key) changeSite({ keys: site.keys.map(k => k.id === key.id ? { ...k, ...patch } : k) }); }
  function changeModel(patch: Partial<ModelConfig>) { if (key && model) changeKey({ models: key.models.map(m => m.id === model.id ? { ...m, ...patch } : m) }); }
  function changeSearch(patch: Partial<SearchConfig>) {
    setDraft(d => {
      const current = d.search ?? defaultSearchConfig();
      const search = { ...current, ...patch };
      if (search.provider !== current.provider || search.base_url !== current.base_url) {
        delete search.api_key;
        search.has_api_key = false;
      }
      return { ...d, search };
    });
    setNotice('');
  }
  function close() { if (!busy && (!dirty || confirm('设置尚未保存。确认放弃这些更改？'))) onClose(); }
  async function save() {
    const issue = validateSubagentModels(draft) || validate(draft); if (issue) { setError(issue); return; }
    setBusy(true); setError(''); setNotice('');
    try {
      const result = sanitizeConfig(await api.saveConfig(draft));
      setDraft(clone(result)); setBaseline(JSON.stringify(clone(result))); onSaved(result); setConflict(false); setNotice(`配置已保存 · revision ${result.revision}`);
    } catch (reason) { setError(errorMessage(reason)); setConflict(reason instanceof ApiError && reason.status === 409); }
    finally { setBusy(false); }
  }
  async function reload() {
    if (dirty && !confirm('重新读取将放弃未保存设置与临时输入的 Key，继续？')) return;
    setBusy(true); setError('');
    try { const next = sanitizeConfig(await api.config()); setDraft(clone(next)); setBaseline(JSON.stringify(clone(next))); onSaved(next); setSiteId(next.sites[0]?.id ?? ''); setConflict(false); setNotice('已加载服务器最新配置。'); }
    catch (reason) { setError(errorMessage(reason)); } finally { setBusy(false); }
  }
  function openQuality() {
    if (dirty) { setError('请先保存模型配置，再打开质量检查。'); return; }
    if (site && key && model) setQualitySelection({ selection: { site_id: site.id, key_id: key.id, model_id: model.id }, name: model.name || model.model });
  }
  if (qualitySelection) return <QualityChecks key={JSON.stringify(qualitySelection.selection)} selection={qualitySelection.selection} modelName={qualitySelection.name} onClose={() => setQualitySelection(null)} />;
  return <Modal title="工作台设置" subtitle="连接你的模型，让每一步都尽在掌握。" onClose={close} className="settings-modal">
    <nav className="settings-tabs" aria-label="设置分类">
      <button className={tab === 'models' ? 'active' : ''} onClick={() => setTab('models')}><Globe2 size={17} />模型连接</button>
      <button className={tab === 'subagents' ? 'active' : ''} onClick={() => setTab('subagents')}><GitBranch size={17} />子代理</button>
      <button className={tab === 'network' ? 'active' : ''} onClick={() => setTab('network')}><Network size={17} />网络与诊断</button>
      <button className={tab === 'animation' ? 'active' : ''} onClick={() => setTab('animation')}><Sparkles size={17} />背景动画</button>
      <button className={tab === 'security' ? 'active' : ''} onClick={() => setTab('security')}><ShieldCheck size={17} />安全</button>
    </nav>
    <div className="settings-scroll">
      {error && <Alert onClose={() => setError('')}>{error}{/子代理|subagent/i.test(error) && <button className="text-button" onClick={() => setTab('subagents')}>前往子代理</button>}{conflict && <button className="text-button" disabled={busy} onClick={() => void reload()}>读取最新配置</button>}</Alert>}
      {notice && <Alert tone="success" onClose={() => setNotice('')}>{notice}</Alert>}
      <fieldset disabled={busy} className="bare-fieldset">
      {tab === 'models' && <div className="model-editor">
        <aside className={`site-nav ${site ? 'has-selection' : ''}`}><div className="section-eyebrow">站点 <span>{draft.sites.length}</span></div>
          {draft.sites.map(s => <button key={s.id} className={`site-button ${siteId === s.id ? 'active' : ''}`} onClick={() => { setSiteId(s.id); setKeyId(''); setModelId(''); }}><span className="site-icon"><Globe2 size={17} /></span><span><strong>{s.name || '未命名站点'}</strong><small>{s.keys.length} 个 Key · {s.keys.reduce((n, k) => n + k.models.length, 0)} 个模型</small></span><ChevronRight size={14} /></button>)}
          <button className="button secondary add-site" onClick={() => { const s: Site = { id: crypto.randomUUID(), name: '新站点', base_url: '', keys: [] }; setDraft(d => ({ ...d, sites: [...d.sites, s] })); setSiteId(s.id); setKeyId(''); setModelId(''); }}><Plus size={16} />添加站点</button>
        </aside>
        <div className="site-editor">{site ? <>
          <div className="section-heading"><div><p className="section-eyebrow">CONNECTION / 站点</p><h3>{site.name || '未命名站点'}</h3></div><button className="icon-button danger-text" title="删除站点" aria-label="删除站点" onClick={() => { if (confirm(`删除「${site.name}」及其所有 Key 和模型？保存后生效。`)) { setDraft(d => ({ ...d, sites: d.sites.filter(s => s.id !== site.id) })); setSiteId(''); } }}><Trash2 size={17} /></button></div>
          <div className="form-grid"><Field label="站点名称"><input value={site.name} onChange={e => changeSite({ name: e.target.value })} placeholder="例如：我的模型服务" /></Field><Field label="API 基础地址" hint="填写服务商要求的基础地址，通常以 /v1 结尾。"><input type="url" value={site.base_url} onChange={e => changeSite({ base_url: e.target.value })} placeholder="https://api.example.com/v1" autoCapitalize="none" spellCheck={false} /></Field></div>
          <div className="subsection-heading"><h4><KeyRound size={15} />API Keys</h4><button className="text-button" onClick={() => { const k = newKey(); changeSite({ keys: [...site.keys, k] }); setKeyId(k.id); setModelId(''); }}><Plus size={14} />添加 Key</button></div>
          <div className="pill-list" aria-label="API Key 列表">{site.keys.map(k => <button className={`pill ${key?.id === k.id ? 'selected' : ''}`} key={k.id} onClick={() => { setKeyId(k.id); setModelId(''); }}><KeyRound size={13} />{k.name || '未命名 Key'}</button>)}</div>
          {key ? <div className="key-editor"><div className="form-grid"><Field label="Key 名称"><input value={key.name} onChange={e => changeKey({ name: e.target.value })} /></Field><Field label={key.has_secret ? '替换密钥（已有密钥）' : 'API 密钥'} hint={key.has_secret ? '留空保留原值；站点地址变化时需重新输入。' : '仅提交到你的后端，不会保存到浏览器存储。'}><input type="password" autoComplete="new-password" spellCheck={false} value={key.secret ?? ''} onChange={e => changeKey({ secret: e.target.value })} placeholder={key.has_secret ? '留空以保留已有密钥' : '输入 API Key'} /></Field></div>
            <div className="subsection-heading"><h4>此 Key 的模型 <span className="count">{key.models.length}</span></h4><button className="text-button" onClick={() => { const m = newModel(); changeKey({ models: [...key.models, m] }); setModelId(m.id); }}><Plus size={14} />添加模型</button></div>
            <div className="pill-list model-pills" aria-label="模型列表">{key.models.map(m => <button className={`pill ${model?.id === m.id ? 'selected' : ''}`} key={m.id} onClick={() => setModelId(m.id)}><Zap size={13} />{m.name || m.model || '新模型'}</button>)}</div>
            {model ? <div className="model-fields"><div className="form-grid"><Field label="显示名称"><input value={model.name} onChange={e => changeModel({ name: e.target.value })} placeholder="例如：主力推理模型" /></Field><Field label="API 模型标识"><input value={model.model} onChange={e => changeModel({ model: e.target.value })} placeholder="服务商提供的 model ID" autoCapitalize="none" spellCheck={false} /></Field><Field label="最大输出 token"><input type="number" min="1" max="4294967295" value={model.max_output_tokens || ''} onChange={e => changeModel({ max_output_tokens: Number(e.target.value) })} /></Field><Field label="上下文窗口 token"><input type="number" min="1" max="4294967295" value={model.context_window || ''} onChange={e => changeModel({ context_window: Number(e.target.value) })} /></Field><Field label="自动压缩阈值 (%)" hint="默认 95%，达到该上下文占用比例后自动整理上下文。"><input type="number" min="20" max="100" value={model.auto_compact_percent ?? 95} onChange={e => changeModel({ auto_compact_percent: Number(e.target.value) })} /></Field></div>
              <Toggle label="图片输入" hint="启用后可以在此模型的会话中附加图片。" checked={model.supports_images} onChange={supports_images => changeModel({ supports_images })} />
              <div className="reasoning-section"><span className="field-label">支持的思考等级</span><p className="field-hint">只勾选服务商支持的等级；不选则不声明等级。</p><div className="effort-list">{efforts.map(e => <label key={e} className={`effort ${model.reasoning_efforts.includes(e) ? 'checked' : ''}`}><input type="checkbox" checked={model.reasoning_efforts.includes(e)} onChange={event => { const next = event.target.checked ? efforts.filter(v => v === e || model.reasoning_efforts.includes(v)) : model.reasoning_efforts.filter(v => v !== e); changeModel({ reasoning_efforts: next, reasoning_effort: next.includes(model.reasoning_effort) ? model.reasoning_effort : '' }); }} />{e}</label>)}</div><Field label="默认思考等级"><select value={model.reasoning_effort} onChange={e => changeModel({ reasoning_effort: e.target.value })}><option value="">不发送（服务商默认）</option>{model.reasoning_efforts.map(e => <option key={e} value={e}>{e}</option>)}</select></Field></div>
              <button type="button" className="button secondary quality-open" onClick={openQuality}><ShieldCheck size={15} />模型质量检查</button>
              <details className="advanced"><summary>协议高级选项<span>Responses API</span></summary><Toggle label="固定缓存路由标识" hint="同一主会话与子 Agent 复用稳定的 prompt_cache_key。" checked={model.cache_routing !== false} onChange={cache_routing => changeModel({ cache_routing })} /><Toggle label="严格工具 Schema" hint="strict_tools：为此模型启用严格工具结构。" checked={model.strict_tools} onChange={strict_tools => changeModel({ strict_tools })} /><Toggle label="省略 strict 标记" hint="默认开启以改善兼容端点的缓存复用，仅省略 strict 字段，完整参数 Schema 与本地校验保留。端点需要 strict 标记时可关闭。" checked={model.omit_tool_strict !== false} onChange={omit_tool_strict => changeModel({ omit_tool_strict })} /><Toggle label="加密推理回放" hint="reasoning_replay：请求并在相同路由上回放加密推理。" checked={model.reasoning_replay} onChange={reasoning_replay => changeModel({ reasoning_replay })} /><Toggle label="显式发送 store: false" hint="send_store：关闭上游响应存储；不支持此字段时可禁用。" checked={model.send_store} onChange={send_store => changeModel({ send_store })} /></details>
              <button className="text-button danger-text delete-model" onClick={() => { if (confirm('从此 Key 删除这个模型？已有会话绑定可能失效。')) { changeKey({ models: key.models.filter(m => m.id !== model.id) }); setModelId(''); } }}><Trash2 size={14} />删除此模型</button>
            </div> : <div className="inline-empty"><Zap size={23} /><p>为这个 Key 添加可用模型。</p></div>}
            <button className="text-button danger-text delete-key" onClick={() => { if (confirm('删除此 Key 及其所有模型？保存后生效。')) { changeSite({ keys: site.keys.filter(k => k.id !== key.id) }); setKeyId(''); } }}><Trash2 size={14} />删除此 Key</button>
          </div> : <div className="inline-empty"><KeyRound size={25} /><p>添加 API Key，再配置它支持的模型。</p></div>}
        </> : <div className="settings-empty"><Globe2 size={38} /><h3>从一个连接开始</h3><p>添加模型站点，按 Key 独立管理模型。<br />所有连接仅使用 Responses 协议。</p></div>}</div>
      </div>}
      {tab === 'subagents' && <SubagentSettings config={draft} onChange={subagent_models => { setDraft(d => ({ ...d, subagent_models })); setNotice(''); }} />}
      {tab === 'network' && <NetworkSettings network={draft.network} onChange={network => setDraft(d => ({ ...d, network }))} search={draft.search ?? defaultSearchConfig()} onSearchChange={changeSearch} saved={config} dirty={dirty} />}
      {tab === 'animation' && <AnimationSettingsPanel />}
      {tab === 'security' && <SecuritySettings onLoggedOut={onLoggedOut} />}
      </fieldset>
    </div>
    <footer className="settings-footer"><div><span className={`save-dot ${dirty ? 'dirty' : ''}`} />{dirty ? '有未保存的更改' : `已同步 · revision ${draft.revision}`}</div><div className="button-row"><button className="button ghost" onClick={close} disabled={busy}>关闭</button><button className="button primary" onClick={() => void save()} disabled={!dirty || busy}>{busy ? <Spinner label="保存中" /> : <><Save size={16} />保存配置</>}</button></div></footer>
  </Modal>;
}

function SubagentSettings({ config, onChange }: { config: AppConfig; onChange: (pool: SubagentModel[]) => void }) {
  const pool = config.subagent_models ?? [];
  const options = modelOptions(config);
  const issue = validateSubagentModels(config);
  const change = (index: number, entry: SubagentModel) => onChange(pool.map((current, i) => i === index ? entry : current));
  function add() {
    const option = options[0];
    if (option && pool.length < poolLimit) onChange([...pool, { id: crypto.randomUUID(), name: `预设 ${pool.length + 1}`, selection: option.selection, reasoning_effort: null, note: '' }]);
  }
  return <section className="subagent-settings" aria-label="子代理模型池">
    <div className="section-heading"><div><p className="section-eyebrow">SUBAGENTS / 子代理</p><h3>为任务准备合适的模型</h3><p className="muted">添加模型与思考等级预设，供子代理按任务选择。未指定预设时，沿用父会话。</p></div><span className="pool-count">{pool.length}<small> / {poolLimit}</small></span></div>
    {issue && <Alert>{issue}</Alert>}
    <div className="pool-toolbar"><span>模型池 <b>{pool.length}</b></span><button className="button secondary" disabled={!options.length || pool.length >= poolLimit} onClick={add}><Plus size={15} />添加预设</button></div>
    {pool.length ? <div className="pool-list">{pool.map((entry, index) => {
      const selected = options.find(option => sameSelection(option.selection, entry.selection));
      const effortValue = entry.reasoning_effort === null ? 'default' : entry.reasoning_effort === '' ? 'omit' : `effort:${entry.reasoning_effort}`;
      const stale = !!entry.reasoning_effort && !selected?.model.reasoning_efforts.includes(entry.reasoning_effort);
      return <article className={`pool-entry ${!selected ? 'invalid' : ''}`} key={entry.id} aria-label={`子代理预设 ${index + 1}`}>
        <span className="pool-entry-index"><GitBranch size={16} /><small>{String(index + 1).padStart(2, '0')}</small></span>
        <div className="pool-entry-fields"><Field label="预设名称"><input value={entry.name} onChange={event => change(index, { ...entry, name: event.target.value })} placeholder="例如：快速检查 / 深度分析" /></Field>
          <Field label="备注（AI 可见）"><input value={entry.note ?? ''} onChange={event => change(index, { ...entry, note: event.target.value })} placeholder="例如：高智商模型 / 低智商模型 / 高价格模型" /></Field>
          <Field label="模型连接"><ModelSelect config={config} value={entry.selection} label={`预设 ${index + 1} 模型连接`} onChange={selection => change(index, selectSubagentModel(entry, selection))} /></Field>
          <Field label="思考等级"><select value={effortValue} aria-label={`预设 ${index + 1} 思考等级`} onChange={event => change(index, { ...entry, reasoning_effort: event.target.value === 'default' ? null : event.target.value === 'omit' ? '' : event.target.value.slice(7) })}>
            <option value="default">模型默认{selected?.model.reasoning_effort ? ` · ${selected.model.reasoning_effort}` : ''}</option><option value="omit">不发送思考等级</option>
            {stale && <option value={effortValue}>已失效 · {entry.reasoning_effort}</option>}{selected?.model.reasoning_efforts.map(effort => <option key={effort} value={`effort:${effort}`}>{effort}</option>)}
          </select></Field>
          <div className="pool-entry-caption">{selected ? <><span>{selected.site.name} · {selected.key.name}</span><span>{selected.model.supports_images ? '支持图片' : '仅文本'} · {selected.model.context_window.toLocaleString()} 上下文</span></> : <span className="danger-text">连接已移除，请重新选择模型或删除此预设。</span>}</div>
        </div><button className="icon-button danger-text" aria-label={`删除预设 ${index + 1}`} title="删除预设" onClick={() => onChange(pool.filter((_, i) => i !== index))}><Trash2 size={16} /></button>
      </article>;
    })}</div> : <div className="pool-empty"><span className="security-emblem"><GitBranch size={25} /></span><h4>按任务搭配模型</h4><p>同一模型可添加多个预设，分别使用不同思考等级。</p>{!options.length && <small>先在「模型连接」中添加站点、Key 和模型。</small>}</div>}
    <p className="pool-note">更改仅影响之后启动的子代理；已有子代理保留启动时的模型与思考等级。</p>
  </section>;
}

function NetworkSettings({ network, onChange, search, onSearchChange, saved, dirty }: { network: NetworkConfig; onChange: (network: NetworkConfig) => void; search: SearchConfig; onSearchChange: (patch: Partial<SearchConfig>) => void; saved: AppConfig; dirty: boolean }) {
  const [selection, setSelection] = useState<Selection | null>(modelOptions(saved)[0]?.selection ?? null);
  const [result, setResult] = useState<Diagnostic | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  async function test() { if (!selection) return; setBusy(true); setError(''); setResult(null); try { setResult(await api.diagnose(selection)); } catch (reason) { setError(errorMessage(reason)); } finally { setBusy(false); } }
  return <div className="network-settings"><div className="section-heading"><div><p className="section-eyebrow">NETWORK / 网络</p><h3>清晰的连接路径</h3><p className="muted">每次新请求使用最新配置，进行中的请求保留启动时快照。</p></div><Network size={28} className="accent" /></div>
    <div className="mode-options" role="radiogroup" aria-label="代理模式">{([{ id: 'environment', title: '环境代理', subtitle: '读取服务进程环境' }, { id: 'manual', title: '手动代理', subtitle: '明确指定代理地址' }, { id: 'direct', title: '直接连接', subtitle: '不使用任何代理' }] as const).map(mode => <label key={mode.id} className={`mode-option ${network.proxy_mode === mode.id ? 'selected' : ''}`}><input type="radio" name="proxy-mode" checked={network.proxy_mode === mode.id} onChange={() => onChange({ ...network, proxy_mode: mode.id })} /><strong>{mode.title}</strong><small>{mode.subtitle}</small></label>)}</div>
    {network.proxy_mode === 'manual' && <Field label="代理 URL" hint="例如 http://127.0.0.1:7890；失败时不会静默回退直连。"><input type="password" value={network.proxy_url} onChange={e => onChange({ ...network, proxy_url: e.target.value })} placeholder="http://127.0.0.1:7890" autoComplete="off" spellCheck={false} /></Field>}
    {network.proxy_mode !== 'direct' && <Field label="NO_PROXY" hint="绕过代理的主机列表，以逗号分隔。"><input value={network.no_proxy} onChange={e => onChange({ ...network, no_proxy: e.target.value })} placeholder="localhost,127.0.0.1" /></Field>}
    <details className="advanced network-advanced"><summary>超时与重试<span>单位：秒</span></summary><div className="form-grid">{([{ key: 'connect_timeout_secs', label: '连接超时', hint: '建议 10 秒' }, { key: 'headers_timeout_secs', label: '响应头超时', hint: '建议 120 秒' }, { key: 'idle_timeout_secs', label: '流空闲超时', hint: '建议 300 秒' }, { key: 'request_timeout_secs', label: '单次请求期限', hint: '建议 1800 秒' }, { key: 'max_attempts', label: '最大生成尝试次数', hint: '建议 3 次；不会重跑已执行工具' }] as const).map(item => <Field key={item.key} label={item.label} hint={item.hint}><input type="number" min="1" value={network[item.key] || ''} onChange={e => onChange({ ...network, [item.key]: Number(e.target.value) })} /></Field>)}</div></details>
    <section className="search-settings" aria-label="网络搜索"><div className="section-heading"><h3><Search size={17} />网络搜索</h3></div>
      <div className="form-grid"><Field label="搜索供应商"><select value={search.provider} onChange={e => onSearchChange({ provider: e.target.value as SearchConfig['provider'], base_url: '' })}><option value="auto">全网聚合（免费多引擎）</option><option value="duckduckgo">DuckDuckGo</option><option value="bing">Bing（必应公开搜索）</option><option value="wikipedia">Wikipedia（维基百科）</option><option value="github">GitHub 仓库搜索</option><option value="arxiv">arXiv 学术论文</option><option value="hackernews">HackerNews（技术资讯）</option><option value="stackexchange">StackOverflow（编程问答）</option><option value="brave">Brave Search</option><option value="searxng">SearXNG</option></select></Field>
        <Field label="搜索 API URL" hint={search.provider === 'searxng' ? '填写你的 SearXNG 服务地址。' : search.provider === 'auto' ? '聚合模式自动并行调用免费公开引擎。' : '留空使用供应商默认地址。'}><input type="url" value={search.base_url} disabled={search.provider === 'auto'} onChange={e => onSearchChange({ base_url: e.target.value })} placeholder={search.provider === 'brave' ? 'https://api.search.brave.com/res/v1/web/search' : search.provider === 'searxng' ? 'https://search.example.com' : search.provider === 'bing' ? 'https://www.bing.com/search' : search.provider === 'wikipedia' ? 'https://en.wikipedia.org/w/api.php' : search.provider === 'github' ? 'https://api.github.com/search/repositories' : search.provider === 'arxiv' ? 'https://export.arxiv.org/api/query' : search.provider === 'hackernews' ? 'https://hn.algolia.com/api/v1/search' : search.provider === 'stackexchange' ? 'https://api.stackexchange.com/2.3/search/advanced' : search.provider === 'auto' ? '自动聚合多引擎' : 'https://html.duckduckgo.com/html/'} autoCapitalize="none" autoComplete="off" spellCheck={false} /></Field>
      </div>
      <div className="search-provider-grid" role="group" aria-label="可用搜索引擎">
        {([
          { id: 'auto', name: '全网聚合', desc: '并行聚合以下 7 个免费引擎', free: true },
          { id: 'duckduckgo', name: 'DuckDuckGo', desc: '公开网页搜索', free: true },
          { id: 'bing', name: 'Bing 必应', desc: '中英文网页搜索', free: true },
          { id: 'wikipedia', name: 'Wikipedia', desc: '中/英文百科', free: true },
          { id: 'github', name: 'GitHub', desc: '开源仓库与代码', free: true },
          { id: 'arxiv', name: 'arXiv', desc: '学术论文检索', free: true },
          { id: 'hackernews', name: 'HackerNews', desc: '技术社区资讯', free: true },
          { id: 'stackexchange', name: 'StackOverflow', desc: '编程技术问答', free: true },
          { id: 'brave', name: 'Brave Search', desc: '需专用 API Key', free: false },
          { id: 'searxng', name: 'SearXNG', desc: '自建搜索实例', free: false },
        ] as const).map(item => {
          const active = search.provider === item.id;
          const includedInAuto = search.provider === 'auto' && item.free && item.id !== 'auto';
          return <button type="button" key={item.id} className={`search-provider-card ${active ? 'active' : includedInAuto ? 'included' : ''}`} onClick={() => onSearchChange({ provider: item.id, base_url: '' })}>
            <strong>{item.name}{active && <span className="search-badge">当前</span>}{includedInAuto && <span className="search-badge">已聚合</span>}</strong>
            <small>{item.desc}</small>
          </button>;
        })}
      </div>
      {(search.provider === 'brave' || search.provider === 'searxng') && <Field label="搜索 API Key" hint={search.has_api_key ? '已保存；留空保留。更换供应商或地址后需重填。' : '搜索专用密钥。'}><input type="password" value={search.api_key ?? ''} onChange={e => onSearchChange({ api_key: e.target.value })} autoComplete="new-password" spellCheck={false} placeholder={search.has_api_key ? '留空保留已有搜索密钥' : '输入搜索密钥'} /></Field>}
      <p className="field-hint search-note">{search.provider === 'brave' ? '需要 Brave Search 专用密钥。' : search.provider === 'searxng' ? '需在服务端启用 JSON API；密钥可选。' : search.provider === 'auto' ? '免费免密钥：并行聚合 DuckDuckGo、Bing、Wikipedia、HackerNews 等公开引擎，深度模式自动抓取正文摘要。' : '免费公开搜索接口，无需 API Key；工具调用时仍可通过 depth="deep" 开启多引擎深度搜索。'}</p>
    </section>
    <section className="diagnostic-panel"><div className="section-heading"><div><h3><Zap size={17} />连接诊断</h3><p className="muted">仅在你点击时发起一次短模型请求，可能产生费用。</p></div></div><div className="diagnostic-actions"><ModelSelect config={saved} value={selection} onChange={setSelection} disabled={busy} /><button className="button secondary" disabled={busy || !selection || dirty} onClick={() => void test()}>{busy ? <Spinner label="诊断中" /> : <><Zap size={15} />测试连接</>}</button></div>{dirty && <p className="field-hint">请先保存配置，再测试最新连接。</p>}{error && <Alert>{error}</Alert>}{result && <div className={`diagnostic-result ${result.ok ? 'success' : 'error'}`}><strong>{result.ok ? '连接成功' : '连接未完成'}</strong><p>{result.message}</p><div className="diagnostic-route"><span>网络路径</span><code>{typeof result.route === 'string' ? result.route : JSON.stringify(result.route) || '未提供'}</code></div><div className="diagnostic-facts"><span>阶段 <b>{result.stage}</b></span><span>模型 <b>{result.model}</b></span><span>耗时 <b>{result.latency_ms} ms</b></span><span>配置 <b>r{result.revision}</b></span></div></div>}</section>
  </div>;
}
function SecuritySettings({ onLoggedOut }: { onLoggedOut: () => void }) {
  const [current, setCurrent] = useState(''); const [next, setNext] = useState(''); const [confirmPassword, setConfirmPassword] = useState('');
  const [busy, setBusy] = useState(false); const [error, setError] = useState('');
  async function submit(event: FormEvent) { event.preventDefault(); setError(''); if (new TextEncoder().encode(next).length < 12) { setError('新密码至少需要 12 字节。'); return; } if (next !== confirmPassword) { setError('两次输入的新密码不一致。'); return; } setBusy(true); try { await api.password(current, next); setCurrent(''); setNext(''); setConfirmPassword(''); onLoggedOut(); } catch (reason) { setError(errorMessage(reason)); } finally { setBusy(false); } }
  return <div className="security-settings"><div className="security-emblem"><LockKeyhole size={28} /></div><h3>保护你的工作台</h3><p className="muted">修改密码后，所有设备上的登录会话将立即失效。</p><form onSubmit={event => void submit(event)} className="password-form">{error && <Alert>{error}</Alert>}<Field label="当前密码"><input required type="password" autoComplete="current-password" value={current} onChange={e => setCurrent(e.target.value)} /></Field><Field label="新密码"><input required type="password" autoComplete="new-password" value={next} onChange={e => setNext(e.target.value)} /></Field><Field label="再次输入新密码"><input required type="password" autoComplete="new-password" value={confirmPassword} onChange={e => setConfirmPassword(e.target.value)} /></Field><button className="button primary" disabled={busy}>{busy ? <Spinner label="更新中" /> : <><ShieldCheck size={16} />更新密码并退出</>}</button></form><p className="security-note"><ShieldCheck size={15} />密码与 API Key 从不写入浏览器本地存储。</p></div>;
}

const PREVIEW_STATES: ReadonlyArray<{
  id: string;
  label: string;
  desc: string;
  workload: LiveWorkload;
}> = [
  { id: 'idle', label: '空闲 (idle)', desc: '无任务运行，背景完全空白', workload: { mode: 'idle', activeSessions: 0, activeTools: 0, activeSubagents: 0, toolKinds: 0, energy: 0 } },
  { id: 'thinking', label: '单会话思考 (thinking)', desc: '1 个主会话推理中', workload: { mode: 'thinking', activeSessions: 1, activeTools: 0, activeSubagents: 0, toolKinds: 0, energy: 0.45 } },
  { id: 'tooling', label: '工具/DAG 执行 (tooling)', desc: '2 个工具并发调用中', workload: { mode: 'tooling', activeSessions: 1, activeTools: 2, activeSubagents: 0, toolKinds: 2, energy: 0.72 } },
  { id: 'swarm_light', label: '多子代理并发 (3 个)', desc: '3 个子代理 + 4 个后台任务', workload: { mode: 'swarm', activeSessions: 2, activeTools: 4, activeSubagents: 3, toolKinds: 3, energy: 0.9 } },
  { id: 'swarm_heavy', label: '极限高负载 (8 子代理)', desc: '8 个子代理 + 10 个并发工具', workload: { mode: 'swarm', activeSessions: 4, activeTools: 10, activeSubagents: 8, toolKinds: 5, energy: 1.0 } },
];

function AnimationSettingsPanel() {
  const [selectedStyle, setSelectedStyle] = useState<AnimationStyleId | 'auto'>('auto');
  const [stateId, setStateId] = useState<string>('swarm_light');
  const theme: 'light' | 'dark' = document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light';
  const currentState = PREVIEW_STATES.find(s => s.id === stateId) ?? PREVIEW_STATES[3];
  const dynamics = computeWorkloadDynamics(currentState.workload);

  return <section className="anim-settings" aria-label="背景动画预览">
    <div className="section-heading">
      <div><p className="section-eyebrow">ANIMATION / 离散细胞动态预览（只读）</p><h3>全自动负载驱动细胞动画</h3><p className="muted">真实背景会随子代理数量、后台任务数自动切换预设与激烈程度（密度锁定不卡顿），任务结束自动清空；此处仅用于预览各状态与预设效果。</p></div>
    </div>
    <div className="anim-preview-box">
      <AutomatonPreview style={selectedStyle} workload={currentState.workload} theme={theme} />
      <span className="anim-preview-badge">
        {selectedStyle === 'auto' ? '自动轮换预设' : ANIMATION_STYLES.find(s => s.id === selectedStyle)?.name}
        {' · '}
        {currentState.label}
        {currentState.workload.mode !== 'idle' ? ` · 速度 ${dynamics.speed.toFixed(2)}x · 强度 ${Math.round(dynamics.intensity * 100)}%` : ''}
      </span>
    </div>
    <div className="anim-state-bar" role="radiogroup" aria-label="模拟运行状态预览">
      {PREVIEW_STATES.map(st => (
        <button type="button" key={st.id} className={`anim-state-pill ${stateId === st.id ? 'active' : ''}`} onClick={() => setStateId(st.id)}>
          <strong>{st.label}</strong>
          <small>{st.desc}</small>
        </button>
      ))}
    </div>
    <div className="anim-style-grid" role="radiogroup" aria-label="动画预设预览">
      <button type="button" className={`anim-style-card ${selectedStyle === 'auto' ? 'active' : ''}`} onClick={() => setSelectedStyle('auto')}>
        <div className="anim-style-head"><strong>自动动态切换</strong><span>60s锁定 · 10%混合</span></div>
        <small>每个状态至少稳定播放 1 分钟，高负载下有 10% 小概率触发多预设混合</small>
      </button>
      {ANIMATION_STYLES.map(item => (
        <button type="button" key={item.id} className={`anim-style-card ${selectedStyle === item.id ? 'active' : ''}`} onClick={() => setSelectedStyle(item.id)}>
          <div className="anim-style-head"><strong>{item.name}</strong><span>{item.tag}</span></div>
          <small>{item.desc}</small>
        </button>
      ))}
    </div>
  </section>;
}
