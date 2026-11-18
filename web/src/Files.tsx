import { useEffect, useMemo, useRef, useState } from 'react';
import { ChevronRight, ExternalLink, File, FileCode2, FileImage, Folder, FolderOpen, LoaderCircle, RefreshCw, X } from 'lucide-react';
import { api, errorMessage, filePreviewUrl } from './api';
import { fileMarkers, isRelativeFilePath, turnFiles, type FileMarker } from './events';
import type { AgentEvent, FileEntry, TurnFile } from './types';

const kindLabels = { new: '新建', modified: '修改', deleted: '已删除' };
function FileIcon({ path }: { path: string }) { return /\.(png|jpe?g|gif|webp|svg)$/i.test(path) ? <FileImage size={15} /> : /\.(html?|css|jsx?|tsx?|json|rs|py|md)$/i.test(path) ? <FileCode2 size={15} /> : <File size={15} />; }
export function TurnFilesCard({ files, sessionId }: { files: TurnFile[]; sessionId: string }) {
  return <section className={`turn-files ${files.length ? '' : 'empty'}`} aria-label="本轮文件产物"><div className="turn-files-heading"><FolderOpen size={14} /><strong>本轮文件</strong><span>{files.length ? `${files.length} 项变更` : '未更改文件'}</span></div>{files.length > 0 && <div className="turn-file-list">{files.map(file => file.kind === 'deleted' ? <span className="turn-file deleted" key={file.path} title={file.path}><FileIcon path={file.path} /><span><strong>{file.name}</strong><small>{file.path}</small></span><em>{kindLabels[file.kind]}</em></span> : <a className="turn-file" key={file.path} href={filePreviewUrl(sessionId, file.path)} target="_blank" rel="noopener noreferrer" title={`预览 ${file.path}`}><FileIcon path={file.path} /><span><strong>{file.name}</strong><small>{file.path}</small></span><em className={file.kind}>{kindLabels[file.kind]}</em><ExternalLink size={12} /></a>)}</div>}</section>;
}
// Invalidate the changed directory and its ancestors, never fetch collapsed children.
export function directoryRevisions(events: AgentEvent[]): Record<string, number> {
  const revisions: Record<string, number> = {};
  function invalidate(path: string, seq: number) {
    if (!isRelativeFilePath(path)) return;
    const parts = path.split('/'); parts.pop();
    revisions[''] = Math.max(revisions[''] ?? 0, seq);
    while (parts.length) { const dir = parts.join('/'); revisions[dir] = Math.max(revisions[dir] ?? 0, seq); parts.pop(); }
  }
  for (const event of events) {
    if (event.type === 'file_changed' || (event.type === 'file_activity' && event.data.status === 'done')) {
      if (typeof event.data.path === 'string') {
        invalidate(event.data.path, event.seq);
        if (event.data.is_dir === true && isRelativeFilePath(event.data.path)) revisions[event.data.path] = Math.max(revisions[event.data.path] ?? 0, event.seq);
      }
    }
    if (event.type === 'turn_files') for (const file of turnFiles(event.data.files)) invalidate(file.path, event.seq);
  }
  return revisions;
}
interface TreeProps { sessionId: string; path: string; revisions: Record<string, number>; markers: Record<string, FileMarker>; refresh: number }
function Directory({ sessionId, path, revisions, markers, refresh }: TreeProps) {
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [retry, setRetry] = useState(0);
  const revision = revisions[path] ?? 0;
  useEffect(() => {
    const controller = new AbortController(); let alive = true;
    setLoading(true); setError('');
    void api.files(sessionId, path, controller.signal).then(list => {
      if (alive) setEntries(list.entries.filter(entry => isRelativeFilePath(entry.path) && entry.path.split('/').slice(0, -1).join('/') === path).sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name)));
    }).catch(reason => { if (alive) setError(errorMessage(reason)); }).finally(() => { if (alive) setLoading(false); });
    return () => { alive = false; controller.abort(); };
  }, [sessionId, path, revision, refresh, retry]);
  return <ul className="file-directory" aria-label={path || '工作目录文件'} aria-busy={loading}>{entries.map(entry => <TreeEntry key={entry.path} entry={entry} sessionId={sessionId} revisions={revisions} markers={markers} refresh={refresh} path={entry.path} />)}{loading && <li className="file-tree-message" role="status"><LoaderCircle size={13} className="spin" />{entries.length ? '正在刷新' : '读取目录'}</li>}{error && <li className="file-tree-error" role="alert"><span>{error}</span><button className="text-button" onClick={() => setRetry(n => n + 1)}>重试</button></li>}{!loading && !error && !entries.length && <li className="file-tree-message">{path ? '空目录' : '目录内暂无文件'}</li>}</ul>;
}
function TreeEntry({ entry, ...props }: TreeProps & { entry: FileEntry }) {
  const [open, setOpen] = useState(false);
  const marker = props.markers[entry.path];
  const status = marker?.status ?? entry.status;
  const editingInside = entry.is_dir && Object.entries(props.markers).some(([path, marker]) => path.startsWith(`${entry.path}/`) && marker.status === 'editing');
  const badge = status === 'editing' || editingInside ? '编辑中' : marker?.isNew ? '本轮新建' : status === 'new' ? '新建' : status === 'modified' ? '修改' : '';
  const contents = <><span className={`file-node-chevron ${open ? 'open' : ''}`}>{entry.is_dir && <ChevronRight size={12} />}</span>{entry.is_dir ? open ? <FolderOpen size={15} /> : <Folder size={15} /> : <FileIcon path={entry.path} />}<span className="file-node-name">{entry.name}</span>{badge && <span className={`file-status ${status === 'editing' || editingInside ? 'editing' : marker?.isNew ? 'new' : ''}`}>{badge}</span>}</>;
  return <li>{entry.is_dir ? <button className="file-node directory" aria-label={`目录 ${entry.path}`} aria-expanded={open} title={entry.path} onClick={() => setOpen(value => !value)}>{contents}</button> : <a className="file-node" href={filePreviewUrl(props.sessionId, entry.path)} target="_blank" rel="noopener noreferrer" title={`预览 ${entry.path}`}>{contents}</a>}{entry.is_dir && open && <Directory {...props} path={entry.path} />}</li>;
}
export function FileSidebar({ sessionId, events, open, onClose, toggleRef }: { sessionId: string; events: AgentEvent[]; open: boolean; onClose: () => void; toggleRef: React.RefObject<HTMLButtonElement | null> }) {
  const [refresh, setRefresh] = useState(0);
  const [drawer, setDrawer] = useState(() => window.matchMedia('(max-width: 1180px)').matches);
  const closeRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLElement>(null);
  const revisions = useMemo(() => directoryRevisions(events), [events]);
  const markers = useMemo(() => fileMarkers(events), [events]);
  const editing = Object.entries(markers).filter(([, marker]) => marker.status === 'editing');
  useEffect(() => { const media = window.matchMedia('(max-width: 1180px)'); const listener = () => setDrawer(media.matches); media.addEventListener('change', listener); return () => media.removeEventListener('change', listener); }, []);
  useEffect(() => {
    if (!open || !drawer) return;
    closeRef.current?.focus();
    function keyboard(event: KeyboardEvent) {
      if (event.key === 'Escape') { event.preventDefault(); onClose(); }
      if (event.key === 'Tab') {
        const nodes = panelRef.current?.querySelectorAll<HTMLElement>('a[href],button:not(:disabled)');
        if (!nodes?.length) return;
        const first = nodes[0]; const last = nodes[nodes.length - 1];
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
      }
    }
    window.addEventListener('keydown', keyboard);
    return () => { window.removeEventListener('keydown', keyboard); toggleRef.current?.focus(); };
  }, [open, drawer, onClose, toggleRef]);
  return <>{drawer && open && <div className="files-scrim" onClick={onClose} aria-hidden="true" />}<aside id="session-files" ref={panelRef} className={`file-sidebar ${open ? 'is-open' : ''}`} role={drawer ? 'dialog' : undefined} aria-modal={drawer && open ? true : undefined} aria-label="工作目录文件" inert={drawer && !open}><header><span><FolderOpen size={16} />文件</span><button className="icon-button" onClick={() => setRefresh(n => n + 1)} aria-label="刷新文件树" title="刷新文件树"><RefreshCw size={14} /></button>{drawer && <button ref={closeRef} className="icon-button" aria-label="关闭文件面板" onClick={onClose}><X size={17} /></button>}</header>{editing.length > 0 && <div className="file-current-activity" role="status"><span className="editing-dot" />正在编辑<span title={editing.map(([path]) => path).join('\n')}>{editing.map(([path]) => path).join('、')}</span></div>}<div className="file-tree-scroll">{(!drawer || open) && <Directory sessionId={sessionId} path="" revisions={revisions} markers={markers} refresh={refresh} />}</div></aside></>;
}
