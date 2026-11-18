import { useEffect, useRef, type ReactNode } from 'react';
import { AlertCircle, CheckCircle2, LoaderCircle, X } from 'lucide-react';
import { modelOptions, selectionValue, type AppConfig, type Selection } from './types';

export function Spinner({ label = '加载中' }: { label?: string }) { return <span className="loading-inline" role="status"><LoaderCircle className="spin" size={17} /><span>{label}</span></span>; }
export function Alert({ children, tone = 'error', onClose }: { children: ReactNode; tone?: 'error' | 'success' | 'info'; onClose?: () => void }) {
  return <div className={`alert ${tone}`} role={tone === 'error' ? 'alert' : 'status'}>{tone === 'success' ? <CheckCircle2 size={17} /> : <AlertCircle size={17} />}<div>{children}</div>{onClose && <button className="icon-button" aria-label="关闭提示" onClick={onClose}><X size={16} /></button>}</div>;
}
export function Field({ label, hint, children, className = '' }: { label: string; hint?: string; children: ReactNode; className?: string }) {
  return <label className={`field ${className}`}><span className="field-label">{label}</span>{children}{hint && <span className="field-hint">{hint}</span>}</label>;
}
export function Toggle({ label, hint, checked, onChange }: { label: string; hint?: string; checked: boolean; onChange: (value: boolean) => void }) {
  return <label className="toggle-row"><span><strong>{label}</strong>{hint && <small>{hint}</small>}</span><input type="checkbox" role="switch" checked={checked} onChange={e => onChange(e.target.checked)} /><span className="toggle-track" aria-hidden="true" /></label>;
}
export function Modal({ title, subtitle, children, onClose, className = '' }: { title: string; subtitle?: string; children: ReactNode; onClose: () => void; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const close = useRef(onClose); close.current = onClose;
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const first = ref.current?.querySelector<HTMLElement>('input,button,select,textarea,[tabindex="0"]');
    first?.focus();
    const old = document.body.style.overflow; document.body.style.overflow = 'hidden';
    const key = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { event.preventDefault(); close.current(); }
      if (event.key !== 'Tab') return;
      const all = Array.from(ref.current?.querySelectorAll<HTMLElement>('button:not(:disabled),input:not(:disabled),textarea:not(:disabled),select:not(:disabled),a[href],[tabindex="0"]') ?? []).filter(el => el.getClientRects().length);
      const first = all[0]; const last = all[all.length - 1];
      if (event.shiftKey && (document.activeElement === first || !ref.current?.contains(document.activeElement))) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    };
    document.addEventListener('keydown', key);
    return () => { document.removeEventListener('keydown', key); document.body.style.overflow = old; previous?.focus(); };
  }, []);
  return <div className="modal-backdrop" onMouseDown={e => { if (e.target === e.currentTarget) { e.preventDefault(); onClose(); } }}><div className={`modal ${className}`} role="dialog" aria-modal="true" aria-label={title} ref={ref}><header className="modal-header"><div><h2>{title}</h2>{subtitle && <p>{subtitle}</p>}</div><button className="icon-button" onClick={onClose} aria-label="关闭弹窗"><X size={20} /></button></header>{children}</div></div>;
}
export function ModelSelect({ config, value, onChange, disabled, label = '选择模型' }: { config: AppConfig | null; value: Selection | null; onChange: (selection: Selection) => void; disabled?: boolean; label?: string }) {
  const options = modelOptions(config);
  const selected = value ? selectionValue(value) : '';
  return <select aria-label={label} value={selected} disabled={disabled || !options.length} onChange={event => { const option = options.find(o => selectionValue(o.selection) === event.target.value); if (option) onChange(option.selection); }}>
    {!value && <option value="">{options.length ? '选择一个模型' : '请先配置模型'}</option>}
    {value && !options.some(o => selectionValue(o.selection) === selected) && <option value={selected}>当前绑定模型已移除</option>}
    {config?.sites.map(site => <optgroup key={site.id} label={site.name || '未命名站点'}>{options.filter(o => o.site.id === site.id).map(o => <option key={selectionValue(o.selection)} value={selectionValue(o.selection)}>{o.model.name || o.model.model || '未命名模型'} · {o.key.name || '未命名 Key'}</option>)}</optgroup>)}
  </select>;
}
