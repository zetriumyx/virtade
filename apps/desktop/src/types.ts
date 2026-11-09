export interface Workspace {
  id: string;
  root: string;
  name: string;
}
export interface Session {
  id: string;
  workspace_id: string;
  provider: string;
  native_id: string;
  native_version: string;
  reserved: boolean;
}
export type TaskState = 'submitting' | 'running' | 'unknown' | 'completed' | 'failed' | 'cancelled';
export interface Task {
  id: string;
  session_id: string;
  native_id: string | null;
  state: TaskState;
  request_key: string;
  prompt: string;
}
export interface AgentEvent {
  schema_version: number;
  provider: string;
  native_version: string;
  sequence: number;
  timestamp_ms: number;
  session_id: string | null;
  task_id: string | null;
  kind: string;
  native_method: string;
  payload: Record<string, any>;
  approval_id: string | null;
  outcome?: string;
}
export interface StoredEvent {
  sequence: number;
  task_id: string | null;
  event: AgentEvent;
}
export interface Approval {
  id: string;
  session_id: string;
  task_id: string;
  details: Record<string, any>;
}
export interface ProviderSettings {
  codex_executable: string;
  node_executable: string;
  claude_bridge: string;
  claude_executable: string;
  dsh_executable: string;
}
export interface LiveSession {
  session_id: string;
  info: { id: string; name: string; native_version: string; capabilities: Record<string, unknown> };
  active: Task | null;
  approvals: Approval[];
  cursor: number;
  diagnostics: string[];
  connected?: boolean;
}
export interface Snapshot {
  workspaces: Workspace[];
  sessions: Session[];
  live: LiveSession[];
  recovered_tasks: number;
  settings: ProviderSettings;
  plugins?: import('./components/Plugins').PluginSnapshot[];
  contributions?: [string, import('./components/Plugins').Contribution][];
}
export interface History {
  tasks: Task[];
  events: StoredEvent[];
}
export interface Request {
  method: string;
  params?: Record<string, unknown>;
}
export interface DesktopBridge {
  available: boolean;
  request<T>(request: Request): Promise<T>;
}

export function providerName(id?: string): string {
  return id === 'claude'
    ? 'Claude Code'
    : id === 'codex'
      ? 'OpenAI Codex'
      : id === 'dsh'
        ? 'DeepSeek Harness'
        : (id ?? '选择 Agent');
}
