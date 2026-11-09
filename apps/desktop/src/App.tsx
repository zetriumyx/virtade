import { useCallback, useEffect, useRef, useState } from 'react';
import { Panel, PanelGroup, PanelResizeHandle } from 'react-resizable-panels';
import {
  Activity,
  Bot,
  ChevronDown,
  ChevronRight,
  Command,
  Folder,
  FolderOpen,
  GitBranch,
  LayoutPanelLeft,
  LoaderCircle,
  MessageSquare,
  Moon,
  PanelBottom,
  PanelRight,
  Plus,
  Puzzle,
  Settings2,
  ShieldCheck,
  Sparkles,
  Sun,
  Terminal,
  X,
} from 'lucide-react';
import { Button, Modal } from './components/ui';
import { Chat } from './components/Chat';
import { PluginManagerView, PluginPanel } from './components/Plugins';
import { ExtensionControls, ExtensionRenderer } from './components/Extensions';
import { FilesView } from './components/Files';
import { GitView } from './components/Git';
import { TerminalPane, closeTerminal } from './components/TerminalPane';
import { useUI } from './store';
import { providerName } from './types';
import type { DesktopBridge, History, ProviderSettings, Snapshot } from './types';

const initial: Snapshot = {
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
export function App({ bridge }: { bridge: DesktopBridge }) {
  const ui = useUI();
  const [snapshot, setSnapshot] = useState<Snapshot>(initial);
  const [history, setHistory] = useState<History>({ tasks: [], events: [] });
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [filesMounted, setFilesMounted] = useState(false);
  useEffect(() => {
    if (ui.tab === 'files') setFilesMounted(true);
  }, [ui.tab]);
  const [openProject, setOpenProject] = useState(false);
  const [newSession, setNewSession] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [root, setRoot] = useState('');
  const [provider, setProvider] = useState('codex');
  const [settings, setSettings] = useState<ProviderSettings>(initial.settings);
  const selected = useRef(ui.session);
  selected.current = ui.session;
  const cursor = useRef(0);
  const loading = useRef(false);
  const workspace = snapshot.workspaces.find((item) => item.id === ui.workspace);
  const session = snapshot.sessions.find((item) => item.id === ui.session);
  const sessionLive = snapshot.live.find((item) => item.session_id === ui.session);
  const live = sessionLive?.connected !== false ? sessionLive : undefined;
  const extension = snapshot.contributions?.find(([, view]) => view.id === ui.extension);
  const extensionPlugin = snapshot.plugins?.find(
    (plugin) => plugin.package.manifest.id === extension?.[0],
  );
  const sessions = snapshot.sessions.filter((item) => item.workspace_id === ui.workspace);
  const request = useCallback(
    async <T,>(method: string, params?: Record<string, unknown>) =>
      bridge.request<T>({ method, params }),
    [bridge],
  );
  const refresh = useCallback(async () => {
    if (!bridge.available) return;
    try {
      const value = await request<Snapshot>('snapshot');
      setSnapshot(value);
    } catch (error) {
      setError(String(error instanceof Error ? error.message : error));
    }
  }, [bridge.available, request]);
  useEffect(() => {
    document.documentElement.dataset.theme = ui.theme;
  }, [ui.theme]);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 1000);
    return () => clearInterval(timer);
  }, [refresh]);
  useEffect(() => {
    cursor.current = 0;
    setHistory({ tasks: [], events: [] });
  }, [ui.session]);
  useEffect(() => {
    if (!bridge.available || !ui.session) return;
    const id = ui.session;
    const load = async () => {
      if (loading.current) return;
      loading.current = true;
      try {
        const value = await request<History>('history', {
          session_id: id,
          after: cursor.current,
          limit: 1000,
        });
        if (selected.current !== id) return;
        const after = cursor.current;
        setHistory((previous) => ({
          tasks: value.tasks,
          events: [...previous.events, ...value.events.filter((event) => event.sequence > after)],
        }));
        if (value.events.length) cursor.current = value.events.at(-1)!.sequence;
      } catch (error) {
        setError(String(error instanceof Error ? error.message : error));
      } finally {
        loading.current = false;
      }
    };
    void load();
    const timer = setInterval(() => void load(), 500);
    return () => clearInterval(timer);
  }, [bridge.available, ui.session, request]);
  const act = async (operation: () => Promise<void>) => {
    setBusy(true);
    setError('');
    try {
      await operation();
      await refresh();
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };
  const open = () =>
    void act(async () => {
      const item = await request<{ id: string }>('open_workspace', { root });
      ui.set({ workspace: item.id, session: '' });
      setOpenProject(false);
      setRoot('');
    });
  const connect = (resume?: string) =>
    void act(async () => {
      const item = await request<{ id: string }>('open_session', {
        workspace_id: ui.workspace,
        provider: resume ? session?.provider : provider,
        resume: resume ?? null,
      });
      ui.set({ session: item.id, tab: 'chat' });
      setNewSession(false);
    });
  const draft = ui.drafts[ui.session || 'welcome'] ?? '';
  const setDraft = (text: string) =>
    ui.set({ drafts: { ...useUI.getState().drafts, [ui.session || 'welcome']: text } });
  const submit = () => {
    if (!draft.trim() || !session || !live || live.active || busy) return;
    const text = draft;
    void act(async () => {
      await request('submit', { session_id: session.id, request_key: crypto.randomUUID(), text });
      setDraft('');
    });
  };

  return (
    <div className="workbench">
      <header className="topbar">
        <div className="brand">
          <span className="brand-symbol">
            v<span>•</span>
          </span>
          <strong>virtade</strong>
          <span className="brand-divider" />
          <span className="workspace-title">{workspace?.name ?? '你的开发空间'}</span>
          <ChevronDown size={12} />
        </div>
        <div className="command-hint">
          <Command size={12} />
          <span>专注于创造，让 Agent 处理细节</span>
        </div>
        <div className="top-actions">
          <ExtensionControls
            snapshot={snapshot}
            bridge={bridge}
            onOpen={(id) => ui.set({ tab: 'extension', extension: id })}
          />
          <Button
            variant="ghost"
            size="icon"
            aria-label="切换终端"
            onClick={() => ui.set({ terminal: !ui.terminal })}
          >
            <PanelBottom size={16} />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            aria-label="切换侧栏"
            onClick={() => ui.set({ right: !ui.right })}
          >
            <PanelRight size={16} />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            aria-label="切换主题"
            onClick={() => ui.set({ theme: ui.theme === 'dark' ? 'light' : 'dark' })}
          >
            {ui.theme === 'dark' ? <Sun size={16} /> : <Moon size={16} />}
          </Button>
        </div>
      </header>
      <div className="workbench-body">
        <nav className="activity-rail" aria-label="工作台导航">
          <span className="rail-group">
            {(
              [
                { id: 'chat', icon: MessageSquare, label: 'Agent 对话' },
                { id: 'files', icon: FolderOpen, label: '文件浏览' },
                { id: 'git', icon: GitBranch, label: 'Git 变更' },
                { id: 'plugins', icon: Puzzle, label: '插件' },
              ] as const
            ).map(({ id, icon: Icon, label }) => (
              <button
                key={id}
                title={label}
                aria-label={label}
                aria-current={ui.tab === id ? 'page' : undefined}
                onClick={() => ui.set({ tab: id })}
              >
                <Icon size={21} />
              </button>
            ))}
          </span>
          <span className="rail-group">
            <button
              title="设置"
              aria-label="设置"
              onClick={() => {
                setSettings(snapshot.settings);
                setSettingsOpen(true);
              }}
            >
              <Settings2 size={21} />
            </button>
            <div className="user-dot">V</div>
          </span>
        </nav>
        <PanelGroup direction="horizontal" autoSaveId="virtade.layout.v1">
          <Panel defaultSize={19} minSize={14} maxSize={30} className="explorer">
            <div className="section-heading">
              <span>工作空间</span>
              <Button
                variant="ghost"
                size="icon"
                aria-label="打开工作区"
                disabled={!bridge.available}
                onClick={() => setOpenProject(true)}
              >
                <Plus size={15} />
              </Button>
            </div>
            <div className="workspace-list">
              {snapshot.workspaces.map((item) => (
                <button
                  key={item.id}
                  className={'workspace-item ' + (ui.workspace === item.id ? 'selected' : '')}
                  onClick={() => ui.set({ workspace: item.id, session: '' })}
                >
                  <Folder size={16} />
                  <span>{item.name}</span>
                  <ChevronRight size={13} />
                </button>
              ))}
              {!snapshot.workspaces.length && (
                <div className="empty-project">
                  <FolderOpen size={22} />
                  <p>一个项目，无限可能</p>
                  <span>打开本地文件夹，开始你的工作。</span>
                  <Button
                    variant="outline"
                    size="small"
                    disabled={!bridge.available}
                    onClick={() => setOpenProject(true)}
                  >
                    打开工作区
                  </Button>
                </div>
              )}
            </div>
            <div className="section-heading sessions-heading">
              <span>
                AGENT 会话 <small>{sessions.length.toString().padStart(2, '0')}</small>
              </span>
              <Button
                variant="ghost"
                size="icon"
                aria-label="新建会话"
                disabled={!workspace || !bridge.available}
                onClick={() => setNewSession(true)}
              >
                <Plus size={15} />
              </Button>
            </div>
            <div className="session-list">
              {sessions.map((item, index) => {
                const live = snapshot.live.find((live) => live.session_id === item.id);
                return (
                  <button
                    key={item.id}
                    className={'session-item ' + (ui.session === item.id ? 'selected' : '')}
                    onClick={() => ui.set({ session: item.id, tab: 'chat' })}
                  >
                    <span className={'session-avatar ' + item.provider}>
                      <Bot size={15} />
                    </span>
                    <span>
                      <strong>
                        {live?.info.name ?? providerName(item.provider)}
                        <small>会话 {String(index + 1).padStart(2, '0')}</small>
                      </strong>
                      <span className="session-state">
                        {live?.active ? '正在执行任务' : live ? '已连接' : '本地历史 · 可恢复'}
                      </span>
                    </span>
                    {live?.active && <span className="running-dot" />}
                  </button>
                );
              })}
              {workspace && !sessions.length && (
                <p className="empty-caption">创建会话，与原生 Agent 一起工作。</p>
              )}
            </div>
            {(snapshot.contributions ?? [])
              .filter(
                ([, view]) =>
                  ['panel', 'sidebar', 'workspace_view', 'editor'].includes(view.kind) &&
                  view.spec.type !== 'builtin',
              )
              .map(([owner, view]) => (
                <button
                  className="extension-nav"
                  key={view.id}
                  onClick={() => ui.set({ tab: 'extension', extension: view.id })}
                >
                  <Puzzle size={14} />
                  {view.title}
                </button>
              ))}
            <div className="explorer-bottom">
              <ShieldCheck size={14} />
              <span>本地优先，由你掌控</span>
            </div>
          </Panel>
          <PanelResizeHandle className="resize-handle" />
          <Panel minSize={42}>
            <PanelGroup direction="vertical" autoSaveId="virtade.main-layout.v1">
              <Panel minSize={40} className="main-panel">
                <div className="panel-tabs">
                  <button className="active">
                    <span className="tab-dot" />
                    {ui.tab === 'chat'
                      ? 'Agent 对话'
                      : ui.tab === 'files'
                        ? '文件浏览'
                        : ui.tab === 'git'
                          ? 'Git 变更'
                          : ui.tab === 'extension'
                            ? (extension?.[1].title ?? '扩展视图')
                            : '插件管理'}
                    <X size={12} />
                  </button>
                  <div className="panel-tabs-right">
                    {session && !live && (
                      <Button
                        size="small"
                        variant="outline"
                        disabled={busy || !bridge.available}
                        onClick={() => connect(session.id)}
                      >
                        恢复会话
                      </Button>
                    )}
                    <span className="subtle-tag">{session?.provider ?? 'WORKBENCH'}</span>
                  </div>
                </div>
                {!bridge.available && (
                  <div className="preview-notice">
                    <LayoutPanelLeft size={14} /> 浏览器预览 · 在桌面应用中连接本地 Agent 和工作区
                  </div>
                )}
                {error && (
                  <div role="alert" className="error-banner">
                    <span>{error}</span>
                    <button aria-label="关闭提示" onClick={() => setError('')}>
                      <X size={14} />
                    </button>
                  </div>
                )}
                {busy && (
                  <div className="busy-line">
                    <LoaderCircle className="spin" size={13} /> 正在连接或处理请求…
                  </div>
                )}
                {filesMounted && (
                  <div
                    className="persistent-view"
                    style={{ display: ui.tab === 'files' ? 'flex' : 'none' }}
                  >
                    <FilesView workspace={workspace} bridge={bridge} theme={ui.theme} />
                  </div>
                )}
                {ui.tab === 'files' ? null : ui.tab === 'git' ? (
                  <GitView workspace={workspace} bridge={bridge} theme={ui.theme} />
                ) : ui.tab === 'chat' ? (
                  <Chat
                    snapshot={snapshot}
                    bridge={bridge}
                    session={session}
                    history={history}
                    live={live}
                    draft={draft}
                    setDraft={setDraft}
                    busy={busy}
                    onSubmit={submit}
                    onCancel={() =>
                      void act(async () => {
                        await request('cancel', { session_id: ui.session });
                      })
                    }
                    onApprove={(approval, decision) =>
                      void act(async () => {
                        await request('approve', {
                          session_id: approval.session_id,
                          approval_id: approval.id,
                          decision,
                        });
                      })
                    }
                  />
                ) : ui.tab === 'plugins' ? (
                  <PluginManagerView
                    plugins={snapshot.plugins ?? []}
                    bridge={bridge}
                    onChanged={refresh}
                    onSelect={(owner) => {
                      const view = snapshot.contributions?.find(([id]) => id === owner);
                      if (view) ui.set({ tab: 'extension', extension: view[1].id });
                    }}
                  />
                ) : ui.tab === 'extension' && extension && extensionPlugin ? (
                  <PluginPanel
                    key={extension[1].id + extensionPlugin.generation}
                    owner={extension[0]}
                    plugin={extensionPlugin}
                    view={extension[1]}
                    bridge={bridge}
                  />
                ) : (
                  <div className="workspace-view">
                    <FolderOpen size={32} />
                    <h2>扩展视图已关闭</h2>
                    <p className="muted">
                      {workspace ? workspace.root : '选择一个工作区，查看项目内容。'}
                    </p>
                  </div>
                )}
              </Panel>
              {ui.terminal && (
                <>
                  <PanelResizeHandle className="resize-handle horizontal" />
                  <Panel defaultSize={27} minSize={15} className="terminal-panel">
                    <div className="section-heading">
                      <span>
                        <Terminal size={13} /> 终端
                      </span>
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label="关闭终端"
                        onClick={() =>
                          void act(async () => {
                            if (workspace) await closeTerminal(bridge, workspace.id);
                            ui.set({ terminal: false });
                          })
                        }
                      >
                        <X size={14} />
                      </Button>
                    </div>
                    <TerminalPane workspace={workspace} bridge={bridge} theme={ui.theme} />
                  </Panel>
                </>
              )}
            </PanelGroup>
          </Panel>
          {ui.right && (
            <>
              <PanelResizeHandle className="resize-handle" />
              <Panel defaultSize={22} minSize={17} maxSize={32} className="inspector">
                <div className="section-heading">
                  <span>
                    <Activity size={13} /> 运行概览
                  </span>
                  <span className="subtle-tag">LIVE</span>
                </div>
                <div className="overview-heading">
                  <span className="eyebrow">IN YOUR CONTROL</span>
                  <h2>让每一步都清晰。</h2>
                  <p>
                    Agent 状态、操作审批与任务记录，
                    <br />
                    尽在掌握。
                  </p>
                </div>
                <div className="metrics">
                  <div>
                    <strong>{snapshot.live.length.toString().padStart(2, '0')}</strong>
                    <span>已连接 Agent</span>
                  </div>
                  <div>
                    <strong>
                      {snapshot.live
                        .filter((live) => live.active)
                        .length.toString()
                        .padStart(2, '0')}
                    </strong>
                    <span>进行中任务</span>
                  </div>
                </div>
                <div className="inspector-section">
                  <span className="section-label">当前会话</span>
                  <div className="agent-info">
                    <span className="session-avatar">
                      <Bot size={20} />
                    </span>
                    <div>
                      <strong>
                        {session
                          ? (sessionLive?.info.name ?? providerName(session.provider))
                          : '等待连接'}
                      </strong>
                      <small>
                        {live?.active ? '执行中' : live ? '已就绪' : '选择一个 Agent 会话'}
                      </small>
                    </div>
                  </div>
                  <div className="detail-row">
                    <span>执行方式</span>
                    <span>原生 Harness</span>
                  </div>
                  <div className="detail-row">
                    <span>会话存储</span>
                    <span>本地 SQLite</span>
                  </div>
                  <div className="detail-row">
                    <span>操作审批</span>
                    <span className="accent">逐次确认</span>
                  </div>
                </div>
                {sessionLive && (
                  <details className="inspector-section">
                    <summary>原生能力与版本</summary>
                    <small>{sessionLive.info.native_version}</small>
                    {Object.entries(sessionLive.info.capabilities).map(([name, value]) => (
                      <div className="detail-row" key={name}>
                        <span>{name}</span>
                        <span title={JSON.stringify(value)}>
                          {String((value as { support?: string }).support ?? 'unknown')}
                        </span>
                      </div>
                    ))}
                  </details>
                )}
                {workspace &&
                  snapshot.live.filter(
                    (item) =>
                      item.active &&
                      snapshot.sessions.some(
                        (s) => s.id === item.session_id && s.workspace_id === workspace.id,
                      ),
                  ).length > 1 && (
                    <p role="status" className="diagnostic">
                      多个 Agent 正在同一工作区运行。建议使用独立 Worktree，避免文件冲突。
                    </p>
                  )}
                <div className="inspector-section">
                  <span className="section-label">任务活动</span>
                  {history.tasks.length ? (
                    history.tasks
                      .slice(-5)
                      .reverse()
                      .map((task) => (
                        <div className="activity-item" key={task.id}>
                          <span className={'activity-status ' + task.state} />
                          <div>
                            <p>{task.prompt.slice(0, 48)}</p>
                            <small>{task.state}</small>
                          </div>
                        </div>
                      ))
                  ) : (
                    <div className="quiet-state">
                      <Sparkles size={20} />
                      <p>一切就绪</p>
                      <span>发送第一条任务，开始创造。</span>
                    </div>
                  )}
                </div>
                {sessionLive?.diagnostics.map((message, index) => (
                  <div className="diagnostic" key={index}>
                    {message}
                  </div>
                ))}
                <div className="local-note">
                  <ShieldCheck size={17} />
                  <p>
                    你的代码留在你的工作区。
                    <br />
                    <span>Agent 的联网行为遵循其原生配置。</span>
                  </p>
                </div>
              </Panel>
            </>
          )}
        </PanelGroup>
      </div>
      <footer className="statusbar">
        <span>
          <span className={'connection-dot ' + (bridge.available ? 'connected' : '')} />
          {bridge.available ? '桌面已连接' : '预览模式'}
          <span className="status-divider" />
          virt <small>0.1.0</small>
        </span>
        <span>
          <GitBranch size={12} />
          {workspace?.name ?? '无工作区'}
          <span className="status-divider" />
          {snapshot.recovered_tasks ? `${snapshot.recovered_tasks} 个任务需要核对` : '本地会话'}
          <ShieldCheck size={12} />
          {(snapshot.contributions ?? [])
            .filter(([, view]) => view.kind === 'status')
            .map(([owner, view]) => (
              <span className="extension-status" key={view.id} title={owner}>
                {view.title}: {String(view.spec.text ?? '')}
              </span>
            ))}
        </span>
      </footer>
      <Modal
        open={openProject}
        onOpenChange={setOpenProject}
        title="打开工作区"
        description="选择已有的本地项目文件夹。文件操作限定在该工作区内。"
      >
        <label className="field">
          文件夹路径
          <input
            autoFocus
            value={root}
            placeholder="C:\\Projects\\my-app 或 /home/user/my-app"
            onChange={(event) => setRoot(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && root.trim()) open();
            }}
          />
        </label>
        <div className="modal-actions">
          <Button variant="outline" onClick={() => setOpenProject(false)}>
            取消
          </Button>
          <Button disabled={busy || !root.trim()} onClick={open}>
            打开工作区
          </Button>
        </div>
      </Modal>
      <Modal
        open={newSession}
        onOpenChange={setNewSession}
        title="新建 Agent 会话"
        description="使用 Agent 原生能力。认证由各 Agent 管理，virtade 不保存 API key。"
      >
        <div className="provider-options">
          {[
            { id: 'codex', name: 'OpenAI Codex', description: '原生 app-server · 工具与审批' },
            { id: 'claude', name: 'Claude Code', description: '官方 Agent SDK · 原生工具与会话' },
            {
              id: 'dsh',
              name: 'DeepSeek Harness',
              description: '原生 ACP v1 · 语义事件与会话恢复',
            },
            ...(snapshot.contributions ?? [])
              .filter(([, view]) => view.kind === 'agent_provider' && view.spec.type !== 'native')
              .map(([owner, view]) => ({
                id: view.id,
                name: view.title,
                description: `由 ${owner} 提供 · 插件权限控制`,
              })),
          ].map((item) => (
            <button
              className={provider === item.id ? 'selected' : ''}
              key={item.id}
              onClick={() => setProvider(item.id)}
            >
              <Bot size={22} />
              <span>
                <strong>{item.name}</strong>
                <small>{item.description}</small>
              </span>
              <span className="radio" />
            </button>
          ))}
        </div>
        <div className="modal-actions">
          <Button variant="outline" onClick={() => setNewSession(false)}>
            取消
          </Button>
          <Button disabled={busy} onClick={() => connect()}>
            连接 Agent
          </Button>
        </div>
      </Modal>
      <Modal
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        title="设置"
        description="配置本地 Agent 的启动入口。留空时使用系统 PATH 中的默认程序。"
      >
        <div className="settings-fields">
          {(
            [
              { key: 'codex_executable', label: 'Codex 程序', placeholder: 'codex' },
              { key: 'node_executable', label: 'Node.js 程序', placeholder: 'node' },
              {
                key: 'claude_bridge',
                label: 'Claude SDK bridge',
                placeholder: '…/claude-bridge/dist/main.js',
              },
              {
                key: 'claude_executable',
                label: 'Claude Code 程序',
                placeholder: '使用 SDK 默认程序',
              },
              { key: 'dsh_executable', label: 'DeepSeek Harness 程序', placeholder: 'dsh' },
            ] as const
          ).map((field) => (
            <label key={field.key} className="field">
              {field.label}
              <input
                value={settings[field.key]}
                placeholder={field.placeholder}
                onChange={(event) => setSettings({ ...settings, [field.key]: event.target.value })}
              />
            </label>
          ))}
        </div>
        <div className="extension-settings">
          {(snapshot.contributions ?? [])
            .filter(([, view]) => view.kind === 'settings')
            .map((extension) => (
              <details key={extension[1].id}>
                <summary>{extension[1].title}</summary>
                <ExtensionRenderer snapshot={snapshot} extension={extension} bridge={bridge} />
              </details>
            ))}
        </div>
        <div className="modal-actions">
          <Button variant="outline" onClick={() => setSettingsOpen(false)}>
            取消
          </Button>
          <Button
            disabled={!bridge.available || busy}
            onClick={() =>
              void act(async () => {
                await request('save_settings', { settings });
                setSettingsOpen(false);
              })
            }
          >
            保存设置
          </Button>
        </div>
      </Modal>
    </div>
  );
}
