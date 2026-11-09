import { lazy, Suspense, useCallback, useEffect, useState, useRef } from 'react';
import {
  Check,
  ChevronRight,
  GitBranch,
  GitCommitHorizontal,
  GitCompare,
  Plus,
  RefreshCw,
  Trash2,
  Undo2,
} from 'lucide-react';
import { Button, Modal } from './ui';
import type { DesktopBridge, Workspace } from '../types';
const Diff = lazy(() => import('./CodeEditor').then((module) => ({ default: module.CodeDiff })));
interface Change {
  path: string;
  status: string;
  staged: boolean;
}
export function parseStatus(output: string) {
  const parts = output.split('\0').filter(Boolean);
  let branch = '';
  const changes: Change[] = [];
  for (let i = 0; i < parts.length; i++) {
    const line = parts[i]!;
    if (line.startsWith('##')) {
      branch = line.slice(3).split('...')[0] ?? '';
      continue;
    }
    const state = line.slice(0, 2);
    const path = line.slice(3);
    if (!path) continue;
    if (state === '??') changes.push({ path, status: '?', staged: false });
    else {
      if (state[0] !== ' ' && state[0] !== '?')
        changes.push({ path, status: state[0]!, staged: true });
      if (state[1] !== ' ' && state[1] !== '?')
        changes.push({ path, status: state[1]!, staged: false });
    }
    if (state.includes('R') || state.includes('C')) i++;
  }
  return { branch, changes };
}
interface Worktree {
  path: string;
  branch: string;
  head: string;
}
function parseWorktrees(output: string): Worktree[] {
  return output
    .trim()
    .split('\n\n')
    .filter(Boolean)
    .map((block) => {
      const lines = block.split('\n');
      return {
        path: lines.find((line) => line.startsWith('worktree '))?.slice(9) ?? '',
        branch:
          lines
            .find((line) => line.startsWith('branch '))
            ?.slice(7)
            .replace('refs/heads/', '') ?? '(detached)',
        head: lines.find((line) => line.startsWith('HEAD '))?.slice(5) ?? '',
      };
    });
}
export function GitView({
  workspace,
  bridge,
  theme,
}: {
  workspace?: Workspace;
  bridge: DesktopBridge;
  theme: 'dark' | 'light';
}) {
  const [changes, setChanges] = useState<Change[]>([]);
  const [branch, setBranch] = useState('');
  const [worktrees, setWorktrees] = useState<Worktree[]>([]);
  const [selected, setSelected] = useState<Change | null>(null);
  const [diff, setDiff] = useState({ original: '', modified: '' });
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [newWorktree, setNewWorktree] = useState(false);
  const [path, setPath] = useState('');
  const [newBranch, setNewBranch] = useState('');
  const owner = useRef(workspace?.id);
  owner.current = workspace?.id;
  const selection = useRef(0);
  const call = useCallback(
    <T,>(operation: string, params?: Record<string, unknown>) =>
      bridge.request<T>({
        method: 'workbench',
        params: { workspace_id: workspace?.id, request: { operation, params } },
      }),
    [bridge, workspace?.id],
  );
  const refresh = useCallback(async () => {
    if (!workspace || !bridge.available) return;
    const current = workspace.id;
    try {
      const [status, trees] = await Promise.all([
        call<{ output: string }>('git_status'),
        call<{ output: string }>('git_worktrees'),
      ]);
      if (owner.current !== current) return;
      const parsed = parseStatus(status.output);
      setChanges(parsed.changes);
      setBranch(parsed.branch);
      setWorktrees(parseWorktrees(trees.output));
      setError('');
    } catch (error) {
      if (owner.current === current)
        setError(String(error instanceof Error ? error.message : error));
    }
  }, [call, workspace, bridge.available]);
  useEffect(() => {
    selection.current++;
    setSelected(null);
    setChanges([]);
    setWorktrees([]);
    setBranch('');
    setDiff({ original: '', modified: '' });
    setError('');
    void refresh();
    const timer = setInterval(() => void refresh(), 3000);
    return () => clearInterval(timer);
  }, [workspace?.id, call]);
  const act = async (operation: () => Promise<void>) => {
    setBusy(true);
    try {
      await operation();
      await refresh();
    } catch (error) {
      setError(String(error instanceof Error ? error.message : error));
    } finally {
      setBusy(false);
    }
  };
  const open = async (change: Change) => {
    const current = workspace?.id;
    const order = ++selection.current;
    setSelected(change);
    setDiff({ original: '', modified: '' });
    try {
      const original = change.staged
        ? await call<{ output: string }>('git_file', { path: change.path, index: false }).catch(
            () => ({ output: '' }),
          )
        : await call<{ output: string }>('git_file', { path: change.path, index: true }).catch(
            () => ({ output: '' }),
          );
      const modified =
        change.status === 'D'
          ? { content: '' }
          : change.staged
            ? {
                content: (
                  await call<{ output: string }>('git_file', { path: change.path, index: true })
                ).output,
              }
            : await call<{ content: string }>('read_file', { path: change.path });
      if (owner.current === current && selection.current === order)
        setDiff({ original: original.output, modified: modified.content });
    } catch (error) {
      if (owner.current === current && selection.current === order)
        setError(String(error instanceof Error ? error.message : error));
    }
  };
  if (!workspace)
    return (
      <div className="workspace-view">
        <GitBranch size={30} />
        <h2>选择工作区以查看版本控制</h2>
      </div>
    );
  return (
    <div className="git-view">
      <aside className="git-sidebar">
        <div className="section-heading">
          <span>
            <GitBranch size={13} />
            {branch || '版本控制'}
          </span>
          <Button variant="ghost" size="icon" aria-label="刷新 Git" onClick={() => void refresh()}>
            <RefreshCw size={13} />
          </Button>
        </div>
        <div className="commit-box">
          <textarea
            aria-label="提交说明"
            placeholder="描述这次提交…"
            value={message}
            onChange={(event) => setMessage(event.target.value)}
          />
          <Button
            variant="outline"
            size="small"
            disabled={busy || !message.trim() || !changes.some((change) => change.staged)}
            onClick={() =>
              void act(async () => {
                await call('git_commit', { message });
                setMessage('');
                setSelected(null);
              })
            }
          >
            <GitCommitHorizontal size={13} />
            提交已暂存变更
          </Button>
        </div>
        {[true, false].map((staged) => (
          <div key={String(staged)} className="change-group">
            <div className="section-heading">
              <span>{staged ? '已暂存' : '工作区变更'}</span>
              <small>{changes.filter((change) => change.staged === staged).length}</small>
            </div>
            {changes
              .filter((change) => change.staged === staged)
              .map((change) => (
                <div
                  key={change.path}
                  className={
                    'git-file ' +
                    (selected?.path === change.path && selected?.staged === staged
                      ? 'selected'
                      : '')
                  }
                >
                  <button onClick={() => void open(change)}>
                    <span className={'git-letter ' + change.status}>{change.status}</span>
                    <span>{change.path}</span>
                  </button>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`${staged ? '取消暂存' : '暂存'} ${change.path}`}
                    disabled={busy}
                    onClick={() =>
                      void act(async () => {
                        await call(staged ? 'git_unstage' : 'git_stage', { path: change.path });
                      })
                    }
                  >
                    {staged ? <Undo2 size={12} /> : <Plus size={12} />}
                  </Button>
                </div>
              ))}
          </div>
        ))}
        <div className="section-heading worktree-heading">
          <span>WORKTREES</span>
          <Button
            variant="ghost"
            size="icon"
            aria-label="新建 Worktree"
            onClick={() => setNewWorktree(true)}
          >
            <Plus size={13} />
          </Button>
        </div>
        {worktrees.map((tree) => (
          <div key={tree.path} className="worktree-item">
            <GitBranch size={13} />
            <span>
              <strong>{tree.branch}</strong>
              <small>{tree.path}</small>
            </span>
            {tree.path !== workspace.root && (
              <Button
                variant="ghost"
                size="icon"
                aria-label={`移除 Worktree ${tree.branch}`}
                disabled={busy}
                onClick={() => {
                  if (window.confirm(`移除工作树 ${tree.path}？Git 会保留有未提交改动的目录。`))
                    void act(async () => {
                      await call('worktree_remove', { path: tree.path });
                    });
                }}
              >
                <Trash2 size={12} />
              </Button>
            )}
          </div>
        ))}
      </aside>
      <div className="git-diff-pane">
        <div className="diff-heading">
          <GitCompare size={15} />
          {selected ? selected.path : '变更审阅'}
          <span>{selected?.staged ? 'HEAD → 暂存区' : selected ? '暂存区 → 工作区' : ''}</span>
        </div>
        {error && (
          <div role="alert" className="error-banner">
            {error}
            {error.toLowerCase().includes('not a git repository') && (
              <Button
                size="small"
                variant="outline"
                onClick={() =>
                  void act(async () => {
                    await call('git_init');
                  })
                }
              >
                初始化 Git 仓库
              </Button>
            )}
          </div>
        )}
        {selected ? (
          <Suspense fallback={<p className="empty-caption">正在加载 Diff…</p>}>
            <Diff
              path={selected.path}
              original={diff.original}
              modified={diff.modified}
              theme={theme}
            />
          </Suspense>
        ) : (
          <div className="workspace-view">
            <Check size={32} />
            <h2>{changes.length ? '逐项审阅你的改动' : '工作区保持整洁'}</h2>
            <p>选择一个文件查看 Diff。</p>
          </div>
        )}
      </div>
      <Modal
        open={newWorktree}
        onOpenChange={setNewWorktree}
        title="新建 Worktree"
        description="为并行 Agent 任务创建独立工作树，减少共享目录冲突。"
      >
        <label className="field">
          新分支名
          <input
            value={newBranch}
            onChange={(event) => setNewBranch(event.target.value)}
            placeholder="agent/task-name"
          />
        </label>
        <label className="field">
          工作树绝对路径
          <input
            value={path}
            onChange={(event) => setPath(event.target.value)}
            placeholder="/path/to/independent-worktree"
          />
        </label>
        <div className="modal-actions">
          <Button variant="outline" onClick={() => setNewWorktree(false)}>
            取消
          </Button>
          <Button
            disabled={busy || !path || !newBranch}
            onClick={() =>
              void act(async () => {
                await call('worktree_add', { path, branch: newBranch });
                setNewWorktree(false);
                setPath('');
                setNewBranch('');
              })
            }
          >
            创建
          </Button>
        </div>
      </Modal>
    </div>
  );
}
