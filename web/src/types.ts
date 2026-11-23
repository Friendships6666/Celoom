export interface Bootstrap { initialized: boolean; authenticated: boolean; allow_setup: boolean; version: string; workspace?: string; data_dir?: string }
export interface ModelConfig {
  id: string; name: string; model: string; supports_images: boolean;
  max_output_tokens: number; context_window: number; auto_compact_percent?: number; reasoning_efforts: string[];
  reasoning_effort: string; strict_tools: boolean; omit_tool_strict?: boolean; reasoning_replay: boolean; send_store: boolean; cache_routing?: boolean;
}
export interface ApiKey { id: string; name: string; secret?: string; has_secret?: boolean; models: ModelConfig[] }
export interface Site { id: string; name: string; base_url: string; keys: ApiKey[] }
export interface NetworkConfig {
  proxy_mode: 'environment' | 'manual' | 'direct'; proxy_url: string; no_proxy: string;
  connect_timeout_secs: number; headers_timeout_secs: number; idle_timeout_secs: number;
  request_timeout_secs: number; max_attempts: number;
}
export type SearchProvider = 'auto' | 'duckduckgo' | 'bing' | 'wikipedia' | 'github' | 'arxiv' | 'hackernews' | 'stackexchange' | 'brave' | 'searxng';
export interface SearchConfig {
  provider: SearchProvider; base_url: string; api_key?: string; has_api_key?: boolean;
}
export const defaultSearchConfig = (): SearchConfig => ({ provider: 'duckduckgo', base_url: '' });
export interface AppConfig { revision: number; sites: Site[]; network: NetworkConfig; search?: SearchConfig; subagent_models?: SubagentModel[] }
export interface Selection { site_id: string; key_id: string; model_id: string }
export interface SubagentModel { id: string; name: string; selection: Selection; reasoning_effort: string | null }
export interface SubagentSource { site_name?: string; model_name?: string; model?: string; reasoning_effort?: string | null; model_id?: string | null }
export interface Question {
  id: string; session_id: string; call_id: string; question: string; blocking: boolean; options: string[];
  status: 'pending' | 'answered' | 'withdrawn'; answer: string | null; delivered: boolean; created_at: string; updated_at: string;
}
export interface Goal {
  id: string; session_id: string; title: string; objective: string; status: 'pending' | 'active' | 'paused' | 'completed' | 'cancelled';
  progress: string; rounds: number; created_at: string; updated_at: string;
}
export type GoalAction = 'start' | 'pause' | 'resume' | 'cancel';
export type SessionStatus = 'idle' | 'running' | 'compacting' | 'auto_compacting' | 'waiting_for_answer' | 'waiting_for_network' | 'paused' | 'completed' | 'failed' | 'cancelled' | 'context_limit' | 'interrupted';
export interface Session {
  id: string; title: string; cwd: string; selection: Selection; reasoning_effort: string | null; status: SessionStatus;
  created_at: string; updated_at: string; parent_id?: string | null; depth: number; context_epoch: number;
  encryption?: 'none' | 'half' | 'full'; locked?: boolean;
}
export interface SessionSettings { selection: Selection; reasoning_effort: string | null }
export interface AssistantSource { attempt_id: string; site_name: string; model_name: string; model: string; reasoning_effort: string | null }
export type FileChangeKind = 'new' | 'modified' | 'deleted';
export interface FileEntry { path: string; name: string; is_dir: boolean; size: number; modified_at?: string; status?: 'editing' | 'new' | 'modified' }
export interface FileListing { path: string; entries: FileEntry[] }
export interface TurnFile { path: string; name: string; kind: FileChangeKind }
export interface ImageInput { mime: string; data: string }
export interface AgentEvent { seq: number; session_id: string; type: string; data: Record<string, unknown>; created_at: string }
export interface ComputerStatus {
  browser: unknown; desktop: unknown; policy: 'isolated_first'; busy: boolean; owner?: string;
  privilege: { uid: number; mode: 'root' | 'sudo' | 'user'; root_available: boolean };
  host: { enabled: boolean; lease_owner?: string; lease_remaining_secs: number; physical_monitor: boolean; human_idle_ms: number | null };
}
export interface ComputerActivity {
  session_id: string; seq: number; action: string; target: string; summary: string; is_error: boolean;
  capture_id?: string; width?: number; height?: number; created_at: string;
}
export interface SessionDetail { session: Session; events: AgentEvent[] }
export type DeliveryMode = 'after_call' | 'immediate';
export interface MessageInput { text: string; images?: ImageInput[]; client_message_id: string; delivery_mode?: DeliveryMode }
export interface InterruptionResult { client_message_id: string; promoted: boolean; delivered: boolean; pending_acceptance: boolean }
export interface Diagnostic { ok: boolean; stage: string; message: string; revision: number; route: unknown; model: string; latency_ms: number }
export interface ModelOption { selection: Selection; model: ModelConfig; site: Site; key: ApiKey }
export const activeStatus = (status?: string) => ['running', 'compacting', 'auto_compacting', 'waiting_for_answer', 'waiting_for_network'].includes(status ?? '');
export const statusLabels: Record<SessionStatus, string> = {
  idle: '就绪', running: '执行中', compacting: '压缩中', completed: '已完成', failed: '失败',
  cancelled: '已停止', context_limit: '上下文已满', interrupted: '已中断',
  auto_compacting: '自动压缩中', waiting_for_answer: '等待回答', waiting_for_network: '等待网络', paused: '已暂停',
};
export function modelOptions(config?: AppConfig | null): ModelOption[] {
  return config?.sites.flatMap(site => site.keys.flatMap(key => key.models.map(model => ({
    selection: { site_id: site.id, key_id: key.id, model_id: model.id }, site, key, model,
  })))) ?? [];
}
export const selectionValue = (s: Selection) => JSON.stringify([s.site_id, s.key_id, s.model_id]);
export const sameSelection = (a: Selection, b: Selection) => selectionValue(a) === selectionValue(b);

export interface RuntimeItem {
  id: string;
  name: string;
  running: boolean;
  status: string;
  detail?: string;
  parent_id?: string;
  is_current?: boolean;
  is_current_session?: boolean;
}
export interface RuntimeJobItem {
  id: string;
  owner: string;
  status: string;
  running: boolean;
  exit_code?: number | null;
  bytes: number;
  privilege: string;
}
export interface RuntimeOverview {
  services: RuntimeItem[];
  processes: RuntimeItem[];
  jobs: RuntimeJobItem[];
  session_tasks: RuntimeItem[];
  subagents: RuntimeItem[];
  last_cancelled_subagents?: string[];
}
