// Synthetic UI boundary for tests only. Production never imports this module.
import ReactDOM from 'react-dom/client';
import { App } from '../src/App';
import '../src/styles.css';
import pluginHtml from '../../../examples/wasm-hello/ui.html?raw';
import type { DesktopBridge, History, Request, Snapshot } from '../src/types';
const snapshot: Snapshot = {
  workspaces: [],
  sessions: [],
  live: [],
  recovered_tasks: 0,
  settings: {
    codex_executable: '',
    node_executable: '',
    claude_bridge: '',
    claude_executable: '',
    dsh_executable: '',
  },
};
const history: History = { tasks: [], events: [] };
if (new URLSearchParams(location.search).has('extensions')) {
  snapshot.plugins = [
    {
      package: {
        manifest: { id: 'hello', version: '0.1.0', api: '^0.1', dependencies: [], permissions: [] },
        metadata: { schema_version: 1, component: 'component.wasm', ui: 'ui.html' },
        directory: '/fixture/hello',
        digest: 'fixture',
        granted: [],
        enabled: true,
      },
      state: 'Running',
      generation: 'generation-1',
      diagnostics: [],
    },
  ];
  snapshot.contributions = [
    [
      'hello',
      {
        id: 'hello.panel',
        kind: 'panel',
        title: 'Hello plugin',
        schema_version: 1,
        spec: { type: 'iframe' },
      },
    ],
  ];
  (window as any).__VIRTADE_HOST_TOKEN__ = 'fixture-private-host-token';
}

if (new URLSearchParams(location.search).has('git-race'))
  snapshot.workspaces = [
    { id: 'old', root: '/fixture/old', name: 'old-project' },
    { id: 'new', root: '/fixture/new', name: 'new-project' },
  ];
const requests: Request[] = [];
(window as any).__requests = requests;
const bridge: DesktopBridge = {
  available: true,
  async request<T>(request: Request): Promise<T> {
    requests.push(structuredClone(request));
    const p = request.params ?? {};
    let value: unknown = null;
    switch (request.method) {
      case 'snapshot':
        value = snapshot;
        break;
      case 'workbench': {
        const input = p.request as any;
        switch (input.operation) {
          case 'git_status': {
            if (p.workspace_id === 'old') {
              await new Promise((resolve) => setTimeout(resolve, 700));
              (window as any).__oldGitComplete = true;
            }
            value = { output: `## ${p.workspace_id}\0 M ${p.workspace_id}.txt\0` };
            break;
          }
          case 'git_worktrees':
            value = { output: '' };
            break;
          case 'list_files':
            value = [{ name: 'main.rs', directory: false, symlink: false }];
            break;
          case 'read_file':
            value = { content: 'fn main() {}', version: 'fixture-version' };
            break;
          case 'write_file':
            value = { version: 'saved-version' };
            break;
          default:
            throw new Error('Fixture operation unsupported: ' + input.operation);
        }
        break;
      }

      case 'plugin_ui':
        value = pluginHtml;
        break;
      case 'plugin_request': {
        const payload = p.request as any;
        if (p.id !== 'hello' || p.generation !== 'generation-1') throw new Error('stale identity');
        if (payload.method === 'service_call' && payload.params.key.name === 'hello.greeting')
          value = payload.params.request;
        else if (payload.method === 'event_poll') value = { events: [], dropped: 0 };
        else if (payload.method !== 'event_subscribe') throw new Error('permission denied');
        break;
      }

      case 'open_workspace': {
        const workspace = { id: 'workspace', root: String(p.root), name: 'test-project' };
        snapshot.workspaces.push(workspace);
        value = workspace;
        break;
      }
      case 'open_session': {
        const session = {
          id: 'session',
          workspace_id: 'workspace',
          provider: String(p.provider),
          native_id: 'fixture-session',
          native_version: 'synthetic-ui-fixture',
          reserved: false,
        };
        snapshot.sessions.push(session);
        snapshot.live.push({
          session_id: 'session',
          info: {
            id: session.provider,
            name: 'Fixture',
            native_version: session.native_version,
            capabilities: {},
          },
          active: null,
          approvals: [],
          cursor: 0,
          diagnostics: [],
          connected: true,
        });
        value = session;
        break;
      }
      case 'history':
        value = {
          tasks: history.tasks,
          events: history.events.filter((event) => event.sequence > Number(p.after)),
        };
        break;
      case 'submit': {
        const task = {
          id: 'task',
          session_id: 'session',
          native_id: 'fixture-task',
          state: 'running' as const,
          request_key: String(p.request_key),
          prompt: String(p.text),
        };
        history.tasks.push(task);
        snapshot.live[0]!.active = task;
        snapshot.live[0]!.approvals = [
          {
            id: 'host-token',
            session_id: 'session',
            task_id: 'fixture-task',
            details: { command: 'fixture build' },
          },
        ];
        snapshot.live[0]!.cursor = 1;
        value = task;
        break;
      }
      case 'approve': {
        if (new URLSearchParams(location.search).has('failure')) {
          history.tasks[0]!.state = 'failed';
          snapshot.live[0]!.active = null;
          snapshot.live[0]!.approvals = [];
          history.events.push({
            sequence: 1,
            task_id: 'task',
            event: {
              schema_version: 1,
              provider: 'codex',
              native_version: 'fixture',
              sequence: 1,
              timestamp_ms: 1,
              session_id: 'fixture-session',
              task_id: 'fixture-task',
              kind: 'provider_event',
              native_method: 'error',
              payload: { error: { message: 'fixture expired authentication' } },
              approval_id: null,
            },
          });
          break;
        }
        if (p.approval_id !== 'host-token') throw new Error('invalid approval');
        history.tasks[0]!.state = 'completed';
        snapshot.live[0]!.active = null;
        snapshot.live[0]!.approvals = [];
        snapshot.live[0]!.cursor = 2;
        history.events.push({
          sequence: 1,
          task_id: 'task',
          event: {
            schema_version: 1,
            provider: 'codex',
            native_version: 'fixture',
            sequence: 1,
            timestamp_ms: 1,
            session_id: 'fixture-session',
            task_id: 'fixture-task',
            kind: 'message_delta',
            native_method: 'item/agentMessage/delta',
            payload: { delta: '完成了 **fixture** 任务。' },
            approval_id: null,
          },
        });
        break;
      }
      case 'save_settings':
        snapshot.settings = p.settings as Snapshot['settings'];
        break;
    }
    return structuredClone(value) as T;
  },
};
ReactDOM.createRoot(document.getElementById('root')!).render(<App bridge={bridge} />);
