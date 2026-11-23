import { memo, useCallback, useEffect, useMemo, useRef, useState, type ChangeEvent, type ClipboardEvent, type FormEvent, type ReactNode } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeKatex from 'rehype-katex';
import 'katex/dist/katex.min.css';
import { ArrowDown, ArrowUp, Brain, Check, ChevronRight, CircleAlert, CircleHelp, Code2, Copy, Eye, FolderOpen, GitBranch, Globe2, Image as ImageIcon, ImagePlus, LoaderCircle, Minimize2, Monitor, Search, Square, Target, Terminal, Workflow, X, Zap } from 'lucide-react';
import { api, errorMessage } from './api';
import { subagentLabel, timeline, toolSummary, type TimelineItem } from './events';
import { LogoMark } from './Logo';
import { brandAccent, detectModelBrand } from './ModelAvatar';
import { FileSidebar, TurnFilesCard } from './Files';
import { SessionRoute } from './SessionRoute';
import { AutonomyPanels } from './Autonomy';
import { ComputerCapture, ComputerPanel } from './Computer';
import { safeComputerText } from './computer';
import { Alert, Spinner } from './ui';
import { activeStatus, statusLabels, type AgentEvent, type AppConfig, type ImageInput, type MessageInput, type ModelOption, type Session } from './types';
import type { Connection } from './useSession';
import { imageMimes, MAX_IMAGES, MAX_IMAGE_BYTES, pastedFiles, pastedImages, readImage, validateMessage } from './messageInput';
import { attachmentUrl as renderImage, ImageLightbox } from './ImageLightbox';
import { useOutbox } from './useOutbox';
import { failureLabels } from './outbox';
async function copyText(value: string): Promise<boolean> {
  if (!value) return false;
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(value);
      return true;
    }
  } catch {
    // Fallback below when clipboard API is blocked or unavailable.
  }
  const textarea = document.createElement('textarea');
  textarea.value = value;
  textarea.setAttribute('readonly', '');
  textarea.style.position = 'fixed';
  textarea.style.opacity = '0';
  document.body.appendChild(textarea);
  textarea.select();
  const ok = document.execCommand('copy');
  document.body.removeChild(textarea);
  return ok;
}

function CopyButton({ text, label = '复制', className = 'copy-btn' }: { text: string; label?: string; className?: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1800);
    return () => window.clearTimeout(timer);
  }, [copied]);
  return (
    <button
      type="button"
      className={className}
      onClick={() => {
        void copyText(text).then((ok) => {
          if (ok) setCopied(true);
        });
      }}
      title={copied ? '已复制' : label}
      aria-label={copied ? '已复制' : label}
    >
      {copied ? <Check size={13} /> : <Copy size={13} />}
      <span>{copied ? '已复制' : label}</span>
    </button>
  );
}

function normalizeLatexDelimiters(input: string): string {
  // Convert \[...\] and \(...\) into $$...$$ and $...$ outside code fences and inline code.
  const segments = input.split(/(```[\s\S]*?```|`[^`\n]+`)/g);
  return segments
    .map((segment, idx) => {
      if (idx % 2 === 1) return segment;
      return segment
        .replace(/\\\[([\s\S]+?)\\\]/g, (_m, expr: string) => `\n$$\n${expr.trim()}\n$$\n`)
        .replace(/\\\((.+?)\\\)/g, (_m, expr: string) => `$${expr.trim()}$`);
    })
    .join('');
}

function extractCodeText(node: ReactNode): string {
  if (typeof node === 'string') return node;
  if (typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(extractCodeText).join('');
  if (node && typeof node === 'object' && 'props' in node) {
    const props = (node as { props?: { children?: ReactNode } }).props;
    return extractCodeText(props?.children);
  }
  return '';
}

function CodeBlock({ className, children }: { className?: string; children: ReactNode }) {
  const match = /language-([^\s]+)/.exec(className ?? '');
  const lang = (match?.[1] ?? '').toLowerCase();
  const code = extractCodeText(children).replace(/\n$/, '');
  const isHtml = lang === 'html' || lang === 'htm';
  const isSvg = lang === 'svg';
  const previewable = isHtml || isSvg;
  const [preview, setPreview] = useState(false);
  const srcDoc = isSvg
    ? `<!doctype html><html><head><meta charset="utf-8"><style>html,body{margin:0;height:100%;display:grid;place-items:center;background:#fff;}</style></head><body>${code}</body></html>`
    : code;
  return (
    <div className="code-block">
      <div className="code-block-bar">
        <span className="code-lang">{lang || 'code'}</span>
        <div className="code-block-actions">
          {previewable && (
            <button
              type="button"
              className={`code-action-btn ${preview ? 'active' : ''}`}
              onClick={() => setPreview((v) => !v)}
            >
              {preview ? <Code2 size={13} /> : <Eye size={13} />}
              <span>{preview ? '源码' : '预览'}</span>
            </button>
          )}
          <CopyButton text={code} className="code-action-btn" />
        </div>
      </div>
      {preview && previewable ? (
        <iframe
          className="html-preview-frame"
          sandbox="allow-scripts"
          srcDoc={srcDoc}
          title={`${lang.toUpperCase()} 预览`}
        />
      ) : (
        <pre><code className={className}>{children}</code></pre>
      )}
    </div>
  );
}

const Markdown = memo(function Markdown({ text }: { text: string }) {
  const normalized = useMemo(() => normalizeLatexDelimiters(text), [text]);
  return (
    <ReactMarkdown
      remarkPlugins={[remarkGfm, remarkMath]}
      rehypePlugins={[rehypeKatex]}
      skipHtml
      components={{
        a: ({ children, href }) => <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>,
        img: ({ alt }) => <span className="external-image">[远程图片：{alt || '已阻止自动加载'}]</span>,
        pre: ({ children }) => {
          const child = Array.isArray(children) ? children[0] : children;
          if (child && typeof child === 'object' && 'props' in child) {
            const props = (child as { props?: { className?: string; children?: ReactNode } }).props;
            return <CodeBlock className={props?.className}>{props?.children}</CodeBlock>;
          }
          return <pre>{children}</pre>;
        },
      }}
    >
      {normalized}
    </ReactMarkdown>
  );
});
function timeLabel(time: string) { const date = new Date(time); return Number.isNaN(date.getTime()) ? '' : date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }); }
function ToolCard({ item, running }: { item: Extract<TimelineItem, { kind: 'tool' }>; running: boolean }) {
  const operation = toolSummary(item.name, item.args);
  const tool = item.name.replace(/^functions\./, '');
  const Icon = tool === 'dag' ? Workflow : tool === 'subagent' ? GitBranch : tool === 'computer' ? Monitor : tool === 'ask_user' ? CircleHelp : tool === 'goal' ? Target : tool === 'web_search' ? Search : tool === 'web_fetch' ? Globe2 : tool === 'read_image' ? ImageIcon : Terminal;
  return <details className={`tool-card ${item.error ? 'tool-error' : ''}`}><summary><span className="tool-icon"><Icon size={16} /></span><span className="tool-summary"><strong>{item.name || '工具调用'}</strong>{operation && <code className="tool-operation" title={operation}>{operation}</code>}<small>{item.finished ? item.error ? '执行失败' : '执行完成' : running ? '正在执行' : '未确认完成'}</small></span><span className="tool-result-icon">{item.finished ? item.error ? <CircleAlert size={15} /> : <Check size={15} /> : running ? <LoaderCircle size={15} className="spin" /> : <CircleAlert size={15} />}</span><ChevronRight size={15} className="details-chevron" /></summary><div className="tool-details"><span className="section-eyebrow">输入参数</span><pre>{tool === 'computer' ? safeComputerText(typeof item.args === 'string' ? item.args : JSON.stringify(item.args, null, 2) ?? '') : typeof item.args === 'string' ? item.args : JSON.stringify(item.args, null, 2)}</pre>{item.output !== undefined && <><span className="section-eyebrow">输出</span><pre>{item.output || '（无输出）'}</pre></>}{item.finished && item.capture?.capture_id && <ComputerCapture activity={item.capture} compact />}</div></details>;
}
const TimelineEntry = memo(function TimelineEntry({ item, running, onOpenSession, onPreview, model, sessionId }: { item: TimelineItem; running: boolean; onOpenSession: (id: string) => void; onPreview: (images: ImageInput[], index: number) => void; model?: ModelOption; sessionId: string }) {
  if (item.kind === 'user') {
    const attachments = item.images.filter(image => !!renderImage(image));
    return <article className="message user-message"><div className="message-meta"><span>你</span><time>{timeLabel(item.time)}</time>{item.queued && <span className="queued-badge">{item.interrupted || item.deliveryMode === 'immediate' ? '已排队 · 强制介入' : '已排队 · 等待交付'}</span>}</div>{attachments.length > 0 && <div className="message-images">{attachments.map((image, i) => <button type="button" className="message-image-button" key={i} aria-label={`查看附件 ${i + 1}`} onClick={() => onPreview(attachments, i)}><img src={renderImage(image)} alt={`图片附件 ${i + 1}`} loading="lazy" /></button>)}</div>}{item.text && <div className="user-text">{item.text}</div>}</article>;
  }
  if (item.kind === 'assistant') {
    const hasReasoning = Boolean(item.reasoning && item.reasoning.trim());
    const hasText = Boolean(item.text && item.text.trim());
    if (!hasText && !hasReasoning && !item.failed) {
      if (item.provisional && running) {
        return (
          <div className="working-state" role="status">
            <span className="working-dots"><i /><i /><i /></span>
            <span>正在思考并生成回复…</span>
          </div>
        );
      }
      return null;
    }
    const thinking = item.provisional && running && !hasText;
    const accent = brandAccent(detectModelBrand(item.source?.model_name, item.source?.model, model?.model.name, model?.model.model));
    const style = thinking ? { '--think-primary': accent.primary, '--think-secondary': accent.secondary, '--think-tertiary': accent.tertiary } as React.CSSProperties : undefined;
    const time = timeLabel(item.time);
    return (
      <article className={`message assistant-message ${item.failed ? 'failed-attempt' : ''} ${thinking ? 'is-thinking' : ''}`} style={style}>
        {(time || item.provisional) && (
          <div className="message-meta assistant-meta-minimal">
            {time && <time>{time}</time>}
            {item.provisional && <span className="live-mark">{running ? (hasText ? '生成中' : '思考中') : '未完成'}</span>}
          </div>
        )}
        {hasReasoning && (
          <details className="thinking-panel" open={thinking}>
            <summary>
              <Brain size={14} />
              <span>{thinking ? '正在深度思考…' : '思考过程'}</span>
              <ChevronRight className="chevron" size={14} />
            </summary>
            <div className="thinking-body">
              <Markdown text={item.reasoning ?? ''} />
              {thinking && <span className="text-cursor" />}
            </div>
          </details>
        )}
        {hasText ? (
          <div className="markdown">
            <Markdown text={item.text} />
            {item.provisional && running && <span className="text-cursor" />}
          </div>
        ) : (
          item.provisional && running && (
            <div className="generating-placeholder">
              <LoaderCircle className="spin" size={14} />
              <span>正在组织回复内容…</span>
              <span className="generating-dots"><i /><i /><i /></span>
            </div>
          )
        )}
        {hasText && !item.provisional && (
          <div className="assistant-actions">
            <CopyButton text={item.text} label="复制回答" />
          </div>
        )}
        {item.failed && <div className="attempt-failure"><CircleAlert size={14} />此尝试未完成：{item.failed}</div>}
      </article>
    );
  }
  if (item.kind === 'tool') return <ToolCard item={item} running={running} />;
  if (item.kind === 'files') return <TurnFilesCard files={item.files} sessionId={sessionId} />;
  if (item.kind === 'subagent') return <button className="subagent-card" onClick={() => onOpenSession(item.id)} disabled={!item.id}><span className="subagent-icon"><GitBranch size={19} /></span><span><strong>子 Agent <span className={`mini-status ${activeStatus(item.status) ? 'active' : ''}`}>{statusLabels[item.status as Session['status']] ?? item.status}</span></strong><small>{item.task || '查看子 Agent 会话'}</small>{subagentLabel(item.source) && <em className="subagent-caption" title={[item.source?.model, item.source?.reasoning_effort === '' ? '不发送思考等级' : item.source?.reasoning_effort].filter(Boolean).join(' · ')}>{subagentLabel(item.source)}{item.source?.reasoning_effort && <span> · {item.source.reasoning_effort}</span>}</em>}</span><ChevronRight size={17} /></button>;
  return <div className={`event-notice ${item.tone}`}><CircleAlert size={15} /><details><summary>{item.text.split('\n')[0]}</summary>{item.text.includes('\n') && <pre>{item.text.split('\n').slice(1).join('\n')}</pre>}</details></div>;
});

interface ChatProps {
  session: Session | null; events: AgentEvent[]; model?: ModelOption; config: AppConfig | null; connection: Connection;
  error: string; onReload: () => void; onOpenSession: (id: string) => void; onOpenSettings: () => void; onSettingsSaved: (session: Session) => void;
}
const WINDOW_STEP = 60;

export function Chat({ session, events, model, config, connection, error, onReload, onOpenSession, onOpenSettings, onSettingsSaved }: ChatProps) {
  const [text, setText] = useState('');
  const [images, setImages] = useState<ImageInput[]>([]);
  const [preview, setPreview] = useState<{ images: ImageInput[]; index: number } | null>(null);
  const openPreview = useCallback((attachments: ImageInput[], index: number) => setPreview({ images: attachments, index }), []);
  const [visibleCount, setVisibleCount] = useState(WINDOW_STEP);
  useEffect(() => { setPreview(null); setVisibleCount(WINDOW_STEP); }, [session?.id]);
  const [sending, setSending] = useState(false);
  const [action, setAction] = useState<'cancel' | 'compact' | null>(null);
  const [actionError, setActionError] = useState('');
  const [readingImages, setReadingImages] = useState(false);
  const [scrolledAway, setScrolledAway] = useState(false);
  const [filesOpen, setFilesOpen] = useState(false);
  const [computerOpen, setComputerOpen] = useState(false);
  const [computerAuthorized, setComputerAuthorized] = useState(false);
  const fileToggle = useRef<HTMLButtonElement>(null);
  const closeFiles = useCallback(() => setFilesOpen(false), []);
  const bottom = useRef<HTMLDivElement>(null);
  const scroll = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLTextAreaElement>(null);
  const upload = useRef<HTMLInputElement>(null);
  const follow = useRef(true);
  const [escalating, setEscalating] = useState(false);
  const [escalationStatus, setEscalationStatus] = useState('');
  const escalationLock = useRef(false);
  useEffect(() => { setEscalationStatus(''); }, [session?.id]);
  const outbox = useOutbox(session?.id, events);
  const sendLock = useRef(false);
  const imageLock = useRef(false);
  const composing = useRef(false);
  const alive = useRef(true);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  const items = useMemo(() => timeline(events.filter(event => event.session_id === session?.id)), [events, session?.id]);
  const hiddenCount = Math.max(0, items.length - visibleCount);
  const visibleItems = useMemo(() => (hiddenCount > 0 ? items.slice(hiddenCount) : items), [items, hiddenCount]);
  const hasUserMessage = useMemo(() => events.some(e => e.type === 'user_message'), [events]);
  const loadMoreHistory = useCallback((all = false) => {
    const el = scroll.current;
    const prevHeight = el ? el.scrollHeight : 0;
    const prevTop = el ? el.scrollTop : 0;
    setVisibleCount(c => (all ? items.length : c + WINDOW_STEP));
    if (el) {
      requestAnimationFrame(() => {
        if (scroll.current === el) {
          el.scrollTop = prevTop + (el.scrollHeight - prevHeight);
        }
      });
    }
  }, [items.length]);
  const pendingMessage = useMemo(() => {
    const queued = items.flatMap(item => item.kind === 'user' && item.queued && item.clientId ? [{ id: item.clientId, time: Date.parse(item.time) || 0, immediate: item.interrupted || item.deliveryMode === 'immediate' }] : []);
    const local = outbox.entries.map(entry => ({ id: entry.client_message_id, time: entry.created_at, immediate: entry.delivery_mode === 'immediate' }));
    return [...queued, ...local].sort((a, b) => a.time - b.time).at(-1);
  }, [items, outbox.entries]);
  const running = activeStatus(session?.status);
  const usage = useMemo(() => {
    for (let i = events.length - 1; i >= 0; i--) {
      if (events[i].type === 'usage') return events[i].data;
    }
    return undefined;
  }, [events]);
  const usageInput = Number(usage?.input_tokens);
  const usageOutput = Number(usage?.output_tokens);
  const rawCached = usage?.cached_tokens ?? (usage?.upstream_usage as { input_tokens_details?: { cached_tokens?: unknown } } | undefined)?.input_tokens_details?.cached_tokens;
  const usageCached = rawCached !== null && rawCached !== undefined && Number.isFinite(Number(rawCached)) ? Number(rawCached) : null;
  const cacheHitRate = usageCached !== null && usageInput > 0 ? (usageCached / usageInput) * 100 : null;
  const contextWindow = Number(usage?.context_window) > 0 ? Number(usage?.context_window) : (model?.model.context_window ?? 0);
  const compactThresholdPct = Math.min(100, Math.max(20, model?.model.auto_compact_percent ?? 95));
  const compactThresholdRatio = compactThresholdPct / 100;
  const contextUsed = Number.isFinite(usageInput) && usageInput > 0 ? usageInput : 0;
  const contextRatio = contextWindow > 0 ? Math.min(1, contextUsed / contextWindow) : 0;
  const contextPct = Math.round(contextRatio * 100);
  const contextLevel = session?.status === 'context_limit' || session?.status === 'auto_compacting' || contextRatio >= compactThresholdRatio ? 'full' : contextRatio >= Math.max(0.5, compactThresholdRatio - 0.12) ? 'warn' : 'normal';
  useEffect(() => { if (follow.current) bottom.current?.scrollIntoView({ behavior: 'instant', block: 'end' }); }, [events]);
  useEffect(() => { if (input.current) { input.current.style.height = 'auto'; input.current.style.height = `${Math.min(input.current.scrollHeight, 190)}px`; } }, [text]);
  async function attachImages(files: File[], fromPaste = false) {
    if (!files.length) return;
    if (imageLock.current || sendLock.current) { setActionError('正在读取或保存消息，请稍后再添加图片。'); return; }
    setActionError('');
    if (!model?.model.supports_images) { setActionError('当前模型未启用图片输入，请在设置中选择支持图片的模型；粘贴的文字已保留。'); return; }
    if (files.some(f => !imageMimes.includes(f.type))) { setActionError('请选择 PNG、JPEG、WebP 或 GIF 图片。'); return; }
    if (files.some(f => f.size > MAX_IMAGE_BYTES) || (!fromPaste && files.length + images.length > MAX_IMAGES)) { setActionError('每张图片限 10 MiB，每条消息最多 8 张。'); return; }
    imageLock.current = true; setReadingImages(true);
    try {
      const next = fromPaste ? await pastedImages(files, MAX_IMAGES - images.length) : await Promise.all(files.map(readImage));
      validateMessage({ text, images: [...images, ...next], client_message_id: crypto.randomUUID(), delivery_mode: 'after_call' });
      if (alive.current) setImages(current => [...current, ...next]);
    } catch (reason) { if (alive.current) setActionError(errorMessage(reason)); }
    finally { imageLock.current = false; if (alive.current) setReadingImages(false); }
  }
  async function readImages(event: ChangeEvent<HTMLInputElement>) {
    const files = Array.from(event.target.files ?? []); event.target.value = ''; await attachImages(files);
  }
  function paste(event: ClipboardEvent<HTMLTextAreaElement>) {
    if (event.currentTarget.disabled || event.currentTarget.readOnly) return;
    const files = pastedFiles(event.clipboardData);
    if (!files.length) return; // Native textarea paste preserves cursor, undo and IME; HTML is never executed.
    event.preventDefault();
    const value = event.clipboardData.getData('text/plain');
    const target = event.currentTarget;
    const start = target.selectionStart; const end = target.selectionEnd;
    if (value) {
      setText(current => current.slice(0, start) + value + current.slice(end));
      requestAnimationFrame(() => { if (input.current === target) target.setSelectionRange(start + value.length, start + value.length); });
    }
    // Ctrl+Shift+V may contain only text: we use only data the browser exposes.
    void attachImages(files, true);
  }
  async function send(event?: FormEvent) {
    event?.preventDefault();
    if (!session || !outbox.box || sendLock.current || imageLock.current || composing.current || (!text.trim() && !images.length)) return;
    if (images.length && !model?.model.supports_images) { setActionError('当前模型未启用图片输入，请移除附件或在设置中启用。'); return; }
    const payload: MessageInput = { text, images, client_message_id: crypto.randomUUID(), delivery_mode: 'after_call' };
    sendLock.current = true; setSending(true); setActionError(''); follow.current = true; setScrolledAway(false);
    try { validateMessage(payload); await outbox.box.enqueue(payload); if (alive.current) { setText(''); setImages([]); setEscalationStatus(''); input.current?.focus(); } }
    catch (reason) { if (alive.current) setActionError(errorMessage(reason)); }
    finally { sendLock.current = false; if (alive.current) setSending(false); }
  }
  async function escalateMessage() {
    if (!pendingMessage || !outbox.box || escalationLock.current) return;
    escalationLock.current = true; setEscalating(true); setActionError('');
    try {
      const result = await outbox.box.escalate(pendingMessage.id);
      if (alive.current) setEscalationStatus(result.delivered ? '消息已交付，无需强制介入' : result.promoted ? '已请求强制介入 · 任务继续' : '已保存强制介入请求 · 等待联网或消息确认');
    } catch (reason) { if (alive.current) setActionError(errorMessage(reason)); }
    finally { escalationLock.current = false; if (alive.current) setEscalating(false); }
  }
  async function runAction(next: 'cancel' | 'compact') {
    if (!session || action) return;
    if (next === 'compact' && !confirm('手动压缩会以摘要替换当前模型上下文；历史消息仍保留。此操作可能产生模型费用。继续？')) return;
    setAction(next); setActionError('');
    try { if (next === 'cancel') await api.cancel(session.id); else await api.compact(session.id); }
    catch (reason) { if (alive.current) setActionError(errorMessage(reason)); }
    finally { if (alive.current) setAction(null); }
  }
  if (connection === 'loading') return <div className="chat-loading"><Spinner label="读取会话与消息记录" /></div>;
  if (!session) return <div className="chat-loading"><Alert>{error || '会话暂不可用。'}</Alert><button className="button secondary" onClick={onReload}>重新加载</button></div>;
  return <div className="chat-workspace"><div className="chat-layout">
    <div className="conversation-region"><div className="conversation-scroll" ref={scroll} onScroll={() => { const el = scroll.current; if (el) { follow.current = el.scrollHeight - el.scrollTop - el.clientHeight < 100; setScrolledAway(!follow.current); if (hiddenCount > 0 && el.scrollTop < 100) loadMoreHistory(false); } }}>
      <div className="conversation">
        {error && <Alert>{error}<button className="text-button" onClick={onReload}>重新加载</button></Alert>}
        {session.parent_id && <button className="parent-session text-button" onClick={() => onOpenSession(session.parent_id!)}><GitBranch size={14} />返回父会话</button>}
        {items.length ? <div className="timeline">{hiddenCount > 0 && <div className="history-loader"><button type="button" className="text-button" onClick={() => loadMoreHistory(false)}>加载更早 {Math.min(WINDOW_STEP, hiddenCount)} 条（还有 {hiddenCount} 条）</button><button type="button" className="text-button" onClick={() => loadMoreHistory(true)}>显示全部</button></div>}{visibleItems.map(item => <TimelineEntry key={`${item.kind}-${item.key}`} item={item} running={running} onOpenSession={onOpenSession} onPreview={openPreview} model={model} sessionId={session.id} />)}</div> : <div className="conversation-empty"><span className="empty-emblem"><LogoMark size={36} /></span><h2>开始这次会话</h2><p>在下方输入你的目标。</p></div>}
        {running && !items.some(item => item.kind === 'assistant' && item.provisional) && <div className="working-state" role="status"><span className="working-dots"><i /><i /><i /></span>{session.status === 'auto_compacting' ? '正在自动整理上下文，完成后继续任务' : session.status === 'compacting' ? '正在整理上下文' : session.status === 'waiting_for_answer' ? '任务等待你的回答，请查看下方问题' : session.status === 'waiting_for_network' ? '等待网络恢复 · 自动继续，可发送消息或停止' : 'Agent 正在工作'}</div>}
        <div ref={bottom} className="scroll-anchor" />
      </div>
    </div>
    {scrolledAway && <button className="jump-bottom" onClick={() => { follow.current = true; setScrolledAway(false); bottom.current?.scrollIntoView({ behavior: 'smooth' }); }}><ArrowDown size={16} />回到最新</button>}
    </div><div className="composer-area"><div className="composer-wrap">
      <div className="session-controls"><SessionRoute session={session} config={config} onSaved={onSettingsSaved} onOpenSettings={onOpenSettings} /><button className={`icon-button computer-toggle ${computerOpen ? 'selected' : ''}`} aria-label="打开计算机面板" aria-haspopup="dialog" aria-expanded={computerOpen} title="计算机 · 隔离优先" onClick={() => setComputerOpen(true)}><Monitor size={18} /></button><button ref={fileToggle} className="icon-button files-toggle" aria-label="打开文件面板" aria-controls="session-files" aria-expanded={filesOpen} title="工作目录文件" onClick={() => setFilesOpen(value => !value)}><FolderOpen size={18} /></button></div>
      {actionError && <Alert onClose={() => setActionError('')}>{actionError}</Alert>}
      {outbox.error && <Alert>{outbox.error}</Alert>}
      {outbox.entries.length > 0 && <section className="outbox" aria-label="本地待发送消息"><header><span className="connection-dot warning" />本地待发送 <b>{outbox.entries.length}</b><small>已可靠保存 · 恢复连接后重试</small></header>{outbox.entries.map(entry => <div className={`outbox-entry ${entry.status}`} key={entry.client_message_id}><div><p>{entry.text || '图片消息'}{entry.images.length > 0 && <small> · {entry.images.length} 张图片</small>}</p><small role="status">{entry.status === 'sending' ? '正在发送…' : entry.failure ? failureLabels[entry.failure] : '等待发送'} · {entry.delivery_mode === 'immediate' ? '尽快介入' : '工具边界后交付'}</small></div><div className="outbox-actions"><button className="text-button" type="button" disabled={entry.status === 'sending'} onClick={() => void outbox.box?.retry(entry.client_message_id).catch(reason => setActionError(errorMessage(reason)))}>重试</button><button className="text-button danger-text" type="button" disabled={entry.status === 'sending'} onClick={() => void outbox.box?.discard(entry.client_message_id).catch(reason => setActionError(errorMessage(reason)))}>丢弃</button></div></div>)}</section>}
      {session.status === 'waiting_for_answer' && <div className="delivery-hint">新消息不会替代问题的明确回答，请在问题卡片中提交。</div>}
      <AutonomyPanels sessionId={session.id} events={events} />
      {session.status === 'context_limit' && <Alert tone="info">上下文已达上限，自动整理未能完成。历史记录已保留；可检查错误后重试压缩。</Alert>}
      {session.status === 'auto_compacting' && <div className="auto-compacting" role="status"><LoaderCircle size={13} className="spin" />自动压缩上下文 · 无需确认，完成后继续</div>}
      {!model && <Alert tone="info">当前模型已从配置中移除。请在上方选择可用连接。<button className="text-button" onClick={onOpenSettings}>打开设置</button></Alert>}
      <form className={`composer ${running ? 'agent-working' : ''}`} onSubmit={event => void send(event)}>
        {images.length > 0 && <div className="attachment-previews">{images.map((image, i) => <div className="attachment-preview" key={i}><button type="button" className="attachment-open" aria-label={`预览待发送图片 ${i + 1}`} onClick={() => openPreview(images, i)}><img src={renderImage(image)} alt={`待发送图片 ${i + 1}`} /></button><button type="button" className="attachment-remove" aria-label={`移除图片 ${i + 1}`} disabled={sending} onClick={() => setImages(current => current.filter((_, n) => n !== i))}><X size={12} /></button></div>)}</div>}
        <textarea ref={input} aria-label="消息内容" aria-describedby="composer-delivery-hint" aria-keyshortcuts="Escape" rows={1} placeholder={running ? '继续补充想法，任务不会因发送而停止…' : '描述目标，粘贴文字或多张图片…'} value={text} disabled={sending} onPaste={paste} onCompositionStart={() => { composing.current = true; }} onCompositionEnd={() => { composing.current = false; }} onChange={e => setText(e.target.value)} onKeyDown={e => {
          if (e.defaultPrevented || composing.current || e.nativeEvent.isComposing || e.keyCode === 229) return;
          if (e.key === 'Escape') {
            if (!preview && !filesOpen && !computerOpen && !document.querySelector('[role="dialog"][aria-modal="true"]') && pendingMessage && !pendingMessage.immediate) { e.preventDefault(); e.stopPropagation(); void escalateMessage(); }
            return;
          }
          if (e.key === 'Enter' && !e.shiftKey && !/Mobi|Android/i.test(navigator.userAgent)) { e.preventDefault(); void send(); }
        }} />
        <div className="composer-toolbar"><div className="composer-tools"><input ref={upload} hidden type="file" multiple accept="image/png,image/jpeg,image/webp,image/gif" onChange={event => void readImages(event)} /><button type="button" className="icon-button" title={model?.model.supports_images ? '添加图片' : '此模型未开启图片输入'} aria-label="添加图片" disabled={!model?.model.supports_images || sending || readingImages || images.length >= MAX_IMAGES} onClick={() => upload.current?.click()}>{readingImages ? <LoaderCircle size={18} className="spin" /> : <ImagePlus size={19} />}</button><span className="delivery-default" title="新消息默认在工具边界后交付，不中断任务">工具边界后</span><span className={`delivery-shortcut ${pendingMessage?.immediate ? 'active' : ''}`} title="发送后，在消息输入框按 Esc 将最新待交付消息升级为强制介入；不会发送草稿">{escalating ? '正在请求…' : pendingMessage?.immediate ? '强制介入待交付' : <><kbd>Esc</kbd> 强制介入</>}</span></div>
          <div className="composer-send-actions">{running && <button className="stop-button" type="button" onClick={() => void runAction('cancel')} disabled={action !== null}>{action === 'cancel' ? <LoaderCircle size={14} className="spin" /> : <Square size={12} fill="currentColor" />}停止</button>}<button className="send-button" type="submit" aria-label="发送消息" disabled={sending || readingImages || (!text.trim() && !images.length) || !model || action !== null}>{sending ? <LoaderCircle size={18} className="spin" /> : <ArrowUp size={20} />}</button></div>
        </div>
        <div id="composer-delivery-hint" className="delivery-hint delivery-explanation" role="status">{escalationStatus || (pendingMessage ? '已发送消息等待交付 · Esc 强制介入，不发送草稿、不停止任务' : '新消息默认工具边界后交付 · 发送后可按 Esc 强制介入')}</div>
      </form>
      <div className="context-meter" title={contextWindow > 0 ? `上下文占用 ${contextUsed.toLocaleString()} / ${contextWindow.toLocaleString()} (${contextPct}% · 自动压缩阈值 ${compactThresholdPct}%)${contextLevel === 'full' ? ' · 即将压缩上下文' : ''}` : '暂无上下文窗口数据'}>
        <div className="context-meter-track">
          <div
            className={`context-meter-fill ${contextLevel}`}
            style={{ width: `${contextWindow > 0 ? (contextUsed > 0 ? Math.max(2, contextPct) : 0) : 0}%` }}
          />
        </div>
        <span className={`context-meter-label ${contextLevel}`}>
          {contextWindow > 0
            ? `上下文 ${contextPct}% / ${compactThresholdPct}% (${contextUsed.toLocaleString()} / ${contextWindow.toLocaleString()})${contextLevel === 'full' ? ' · 即将压缩' : ''}`
            : `上下文 0% / ${compactThresholdPct}%`}
        </span>
      </div>
      <div className="composer-footnote"><span>{connection === 'reconnecting' ? <><span className="connection-dot warning" />连接中断，正从最新事件重连</> : <><span className={`connection-dot ${connection === 'connected' ? '' : 'warning'}`} />{connection === 'connected' ? '已连接' : '未连接'}<span className="desktop-only"> · {running ? '断开页面不会取消任务' : 'Shift + Enter 换行'}</span></>}</span><div className="composer-footnote-actions">{usage && Number.isFinite(usageInput) && Number.isFinite(usageOutput) && <span className="usage-inline" title={`上下文窗口 ${usage.context_window} · 最大输出 ${usage.max_output_tokens}${usageCached !== null ? ` · 缓存命中 ${usageCached.toLocaleString()} / ${usageInput.toLocaleString()}${cacheHitRate !== null ? ` (${cacheHitRate.toFixed(1)}%)` : ''}` : ''}`}>输入 {usageInput.toLocaleString()} / 输出 {usageOutput.toLocaleString()}{cacheHitRate !== null && ` · 缓存 ${cacheHitRate.toFixed(1)}%`}</span>}<button type="button" className="icon-button compact-button" onClick={() => void runAction('compact')} disabled={running || sending || action !== null || !hasUserMessage} title="手动压缩上下文" aria-label="手动压缩上下文">{action === 'compact' ? <LoaderCircle size={14} className="spin" /> : <Minimize2 size={14} />}</button></div></div>
    </div></div>
  </div>{preview && <ImageLightbox images={preview.images} initialIndex={preview.index} onClose={() => setPreview(null)} />}<FileSidebar sessionId={session.id} events={events} open={filesOpen} onClose={closeFiles} toggleRef={fileToggle} />{computerOpen && <ComputerPanel sessionId={session.id} events={events} authorizedBefore={computerAuthorized} onAuthorized={() => setComputerAuthorized(true)} onClose={() => setComputerOpen(false)} />}</div>;
}

export function Welcome({ onCreate, onSettings, hasModels }: { onCreate: () => void; onSettings: () => void; hasModels: boolean }) {
  return <div className="welcome"><div className="welcome-orbit" aria-hidden="true"><span /><span /><span /><LogoMark size={56} /></div><h1>你的工作台</h1><button className="button primary welcome-button" onClick={hasModels ? onCreate : onSettings}>{hasModels ? <><PlusIcon />开启新会话</> : <><Zap size={17} />连接第一个模型</>}</button></div>;
}
function PlusIcon() { return <span aria-hidden="true" className="plus-icon">+</span>; }
