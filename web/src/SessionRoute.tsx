import { useEffect, useState, type FormEvent } from 'react';
import { Check, ChevronDown, LoaderCircle, SlidersHorizontal } from 'lucide-react';
import { api, errorMessage } from './api';
import { Alert } from './ui';
import { activeStatus, sameSelection, type AppConfig, type Selection, type Session } from './types';

interface Props { session: Session; config: AppConfig | null; onSaved: (session: Session) => void; onOpenSettings: () => void }
export function SessionRoute({ session, config, onSaved, onOpenSettings }: Props) {
  const [selection, setSelection] = useState<Selection>(session.selection);
  const [effort, setEffort] = useState<string | null>(session.reasoning_effort ?? null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const selectionKey = JSON.stringify(session.selection);
  useEffect(() => { setSelection(session.selection); setEffort(session.reasoning_effort ?? null); }, [selectionKey, session.reasoning_effort]);
  useEffect(() => { if (!notice) return; const timer = setTimeout(() => setNotice(''), 6000); return () => clearTimeout(timer); }, [notice]);
  const sites = config?.sites ?? [];
  const site = sites.find(site => site.id === selection.site_id);
  const key = site?.keys.find(key => key.id === selection.key_id);
  const model = key?.models.find(model => model.id === selection.model_id);
  const currentSite = sites.find(site => site.id === session.selection.site_id);
  const currentKey = currentSite?.keys.find(key => key.id === session.selection.key_id);
  const currentModel = currentKey?.models.find(model => model.id === session.selection.model_id);
  const dirty = !sameSelection(selection, session.selection) || effort !== (session.reasoning_effort ?? null);
  const validEffort = effort === null || effort === '' || model?.reasoning_efforts.includes(effort);
  function selectSite(id: string) {
    const nextSite = sites.find(site => site.id === id);
    const nextKey = nextSite?.keys.find(key => key.models.length);
    const nextModel = nextKey?.models[0];
    if (nextSite && nextKey && nextModel) { setSelection({ site_id: id, key_id: nextKey.id, model_id: nextModel.id }); setEffort(null); setNotice(''); }
  }
  function selectKey(id: string) {
    const nextModel = site?.keys.find(key => key.id === id)?.models[0];
    if (nextModel) { setSelection({ ...selection, key_id: id, model_id: nextModel.id }); setEffort(null); setNotice(''); }
  }
  async function save(event: FormEvent) {
    event.preventDefault(); if (saving || !dirty || !model || !validEffort) return;
    setSaving(true); setError(''); setNotice('');
    try { const saved = await api.sessionSettings(session.id, { selection, reasoning_effort: effort }); onSaved(saved); setNotice(activeStatus(session.status) ? '已保存，将在下次模型请求时生效。' : '会话设置已更新。'); }
    catch (reason) { setError(errorMessage(reason)); }
    finally { setSaving(false); }
  }
  return <div className="session-route-wrap"><details className="session-route"><summary aria-label="调整会话模型"><SlidersHorizontal size={14} /><span>{currentSite?.name || '站点不可用'}<span className="route-separator"> / </span>{currentModel?.name || currentModel?.model || '模型不可用'}</span><small>{session.reasoning_effort === '' ? '不发送思考等级' : session.reasoning_effort ?? currentModel?.reasoning_effort ?? ''}</small><ChevronDown size={13} /></summary><form aria-label="会话模型设置" className="route-form" onSubmit={event => void save(event)}>{error && <Alert onClose={() => setError('')}>{error}</Alert>}<div className="route-fields"><label>站点<select aria-label="会话站点" value={selection.site_id} disabled={saving} onChange={event => selectSite(event.target.value)}>{!site && <option value={selection.site_id}>当前站点不可用</option>}{sites.map(site => <option key={site.id} value={site.id} disabled={!site.keys.some(key => key.models.length)}>{site.name}</option>)}</select></label><label>密钥<select aria-label="会话密钥" value={selection.key_id} disabled={saving || !site} onChange={event => selectKey(event.target.value)}>{!key && <option value={selection.key_id}>当前密钥不可用</option>}{site?.keys.map(key => <option key={key.id} value={key.id} disabled={!key.models.length}>{key.name}</option>)}</select></label><label>模型<select aria-label="会话模型" value={selection.model_id} disabled={saving || !key} onChange={event => { setSelection({ ...selection, model_id: event.target.value }); setEffort(null); setNotice(''); }}>{!model && <option value={selection.model_id}>当前模型不可用</option>}{key?.models.map(model => <option key={model.id} value={model.id}>{model.name || model.model}</option>)}</select></label><label>思考等级<select aria-label="会话思考等级" value={effort === null ? 'inherit' : effort === '' ? 'omit' : `effort:${effort}`} disabled={saving || !model} onChange={event => setEffort(event.target.value === 'inherit' ? null : event.target.value === 'omit' ? '' : event.target.value.slice(7))}><option value="inherit">继承默认{model?.reasoning_effort ? `（${model.reasoning_effort}）` : ''}</option><option value="omit">不发送</option>{effort && !model?.reasoning_efforts.includes(effort) && <option value={`effort:${effort}`} disabled>{effort}（不可用）</option>}{model?.reasoning_efforts.filter(Boolean).map(value => <option key={value} value={`effort:${value}`}>{value}</option>)}</select></label></div><div className="route-actions"><button type="button" className="text-button" onClick={onOpenSettings}>管理连接</button>{dirty && activeStatus(session.status) && <span>下次模型请求生效</span>}<button className="button secondary" disabled={saving || !dirty || !model || !validEffort}>{saving ? <LoaderCircle size={13} className="spin" /> : <Check size={13} />}应用</button></div></form></details>{notice && <span className="route-notice" role="status">{notice}</span>}</div>;
}
