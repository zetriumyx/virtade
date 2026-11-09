import { lazy, Suspense, useCallback, useEffect, useRef, useState } from 'react';
import { ChevronDown, ChevronRight, FileCode2, Folder, RefreshCw, Save, X } from 'lucide-react';
import { Button } from './ui';
import type { DesktopBridge, Workspace } from '../types';
const Editor = lazy(() => import('./CodeEditor'));
interface Entry {
  name: string;
  directory: boolean;
  symlink: boolean;
}
interface BufferState {
  path: string;
  original: string;
  content: string;
  version: string;
}
function Tree({
  path,
  entries,
  load,
  open,
  expanded,
  toggle,
}: {
  path: string;
  entries: Record<string, Entry[]>;
  load: (path: string) => void;
  open: (path: string) => void;
  expanded: Set<string>;
  toggle: (path: string) => void;
}) {
  return (
    <div className="file-tree">
      {(entries[path] ?? []).map((entry) => {
        const name = path ? `${path}/${entry.name}` : entry.name;
        return (
          <div key={name}>
            <button
              className="file-entry"
              onClick={() =>
                entry.directory ? (toggle(name), !entries[name] && load(name)) : open(name)
              }
            >
              {entry.directory ? (
                expanded.has(name) ? (
                  <ChevronDown size={12} />
                ) : (
                  <ChevronRight size={12} />
                )
              ) : (
                <span className="tree-spacer" />
              )}
              {entry.directory ? <Folder size={14} /> : <FileCode2 size={14} />}
              <span>{entry.name}</span>
              {entry.symlink && <small>↗</small>}
            </button>
            {entry.directory && expanded.has(name) && (
              <div className="tree-children">
                <Tree
                  path={name}
                  entries={entries}
                  load={load}
                  open={open}
                  expanded={expanded}
                  toggle={toggle}
                />
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
export function FilesView({
  workspace,
  bridge,
  theme,
}: {
  workspace?: Workspace;
  bridge: DesktopBridge;
  theme: 'dark' | 'light';
}) {
  const [entries, setEntries] = useState<Record<string, Entry[]>>({});
  const [expanded, setExpanded] = useState(new Set<string>());
  const [buffers, setBuffers] = useState<BufferState[]>([]);
  const [selected, setSelected] = useState('');
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  const cache = useRef(new Map<string, { buffers: BufferState[]; selected: string }>());
  const previous = useRef(workspace?.id ?? '');
  const latest = useRef(buffers);
  latest.current = buffers;
  const activePath = useRef(selected);
  activePath.current = selected;
  const currentWorkspace = useRef(workspace?.id);
  currentWorkspace.current = workspace?.id;
  const call = useCallback(
    <T,>(operation: string, params?: Record<string, unknown>) =>
      bridge.request<T>({
        method: 'workbench',
        params: { workspace_id: workspace?.id, request: { operation, params } },
      }),
    [bridge, workspace?.id],
  );
  const load = useCallback(
    (path: string) => {
      if (!workspace) return;
      const owner = workspace.id;
      void call<Entry[]>('list_files', { path })
        .then((value) => {
          if (currentWorkspace.current === owner) setEntries((old) => ({ ...old, [path]: value }));
        })
        .catch((error) => setError(String(error.message ?? error)));
    },
    [call, workspace],
  );
  useEffect(() => {
    if (previous.current)
      cache.current.set(previous.current, {
        buffers: latest.current,
        selected: activePath.current,
      });
    previous.current = workspace?.id ?? '';
    const saved = cache.current.get(previous.current);
    setEntries({});
    setBuffers(saved?.buffers ?? []);
    setSelected(saved?.selected ?? '');
    setError('');
    load('');
  }, [workspace?.id]);
  useEffect(() => {
    const guard = (event: BeforeUnloadEvent) => {
      if (
        latest.current.some((buffer) => buffer.content !== buffer.original) ||
        [...cache.current.values()].some((value) =>
          value.buffers.some((buffer) => buffer.content !== buffer.original),
        )
      ) {
        event.preventDefault();
        event.returnValue = '';
      }
    };
    window.addEventListener('beforeunload', guard);
    return () => window.removeEventListener('beforeunload', guard);
  }, []);
  const open = async (path: string) => {
    const owner = workspace?.id;
    setSelected(path);
    if (buffers.some((buffer) => buffer.path === path)) return;
    try {
      const value = await call<{ content: string; version: string }>('read_file', { path });
      if (currentWorkspace.current !== owner) return;
      setBuffers((old) =>
        old.some((buffer) => buffer.path === path)
          ? old
          : [...old, { path, original: value.content, ...value }],
      );
    } catch (error) {
      setError(String(error instanceof Error ? error.message : error));
    }
  };
  const save = async () => {
    const owner = workspace?.id;
    const buffer = latest.current.find((buffer) => buffer.path === activePath.current);
    if (!buffer || buffer.content === buffer.original) return;
    setSaving(true);
    setError('');
    const content = buffer.content;
    try {
      const value = await call<{ version: string }>('write_file', {
        path: buffer.path,
        content,
        expected: buffer.version,
      });
      if (currentWorkspace.current !== owner) return;
      setBuffers((old) =>
        old.map((item) =>
          item.path === buffer.path ? { ...item, original: content, version: value.version } : item,
        ),
      );
    } catch (error) {
      setError(String(error instanceof Error ? error.message : error));
    } finally {
      setSaving(false);
    }
  };
  const active = buffers.find((buffer) => buffer.path === selected);
  if (!workspace)
    return (
      <div className="workspace-view">
        <Folder size={32} />
        <h2>打开工作区以浏览文件</h2>
      </div>
    );
  return (
    <div className="files-view">
      <aside className="files-sidebar">
        <div className="section-heading">
          <span>{workspace.name}</span>
          <Button variant="ghost" size="icon" aria-label="刷新文件树" onClick={() => load('')}>
            <RefreshCw size={13} />
          </Button>
        </div>
        <Tree
          path=""
          entries={entries}
          load={load}
          open={(path) => void open(path)}
          expanded={expanded}
          toggle={(path) =>
            setExpanded((old) => {
              const next = new Set(old);
              next.has(path) ? next.delete(path) : next.add(path);
              return next;
            })
          }
        />
      </aside>
      <div className="editor-pane">
        <div className="file-tabs">
          {buffers.map((buffer) => (
            <button
              className={selected === buffer.path ? 'selected' : ''}
              key={buffer.path}
              onClick={() => setSelected(buffer.path)}
            >
              <FileCode2 size={12} />
              {buffer.path.split('/').pop()}
              {buffer.content !== buffer.original && <span className="dirty-dot" />}
              <span
                role="button"
                tabIndex={0}
                aria-label={`关闭 ${buffer.path}`}
                onClick={(event) => {
                  event.stopPropagation();
                  if (
                    buffer.content !== buffer.original &&
                    !window.confirm('有未保存的改动，确定关闭这个文件？')
                  )
                    return;
                  setBuffers((old) => old.filter((item) => item.path !== buffer.path));
                  if (selected === buffer.path) setSelected('');
                }}
              >
                <X size={11} />
              </span>
            </button>
          ))}
          {active && (
            <Button
              variant="ghost"
              size="small"
              aria-label="保存文件"
              disabled={saving || active.content === active.original}
              onClick={() => void save()}
            >
              <Save size={13} />
              保存
            </Button>
          )}
        </div>
        {error && (
          <div role="alert" className="error-banner">
            {error}
            <Button
              variant="ghost"
              size="small"
              onClick={() => {
                if (
                  active?.content !== active?.original &&
                  !window.confirm('重新加载会丢弃未保存的改动，是否继续？')
                )
                  return;
                const owner = workspace.id;
                setBuffers((old) => old.filter((buffer) => buffer.path !== selected));
                setError('');
                void call<{ content: string; version: string }>('read_file', { path: selected })
                  .then((value) => {
                    if (currentWorkspace.current === owner)
                      setBuffers((old) => [
                        ...old,
                        { path: selected, original: value.content, ...value },
                      ]);
                  })
                  .catch((error) => setError(String(error)));
              }}
            >
              重新加载
            </Button>
          </div>
        )}
        {active ? (
          <Suspense fallback={<p className="empty-caption">正在加载编辑器…</p>}>
            <Editor
              path={`${workspace.id}/${active.path}`}
              value={active.content}
              revision={active.version}
              onChange={(content) => {
                const next = latest.current.map((buffer) =>
                  buffer.path === active.path ? { ...buffer, content } : buffer,
                );
                latest.current = next;
                setBuffers(next);
              }}
              onSave={() => void save()}
              theme={theme}
            />
          </Suspense>
        ) : (
          <div className="workspace-view">
            <FileCode2 size={30} />
            <p>选择一个文件开始编辑</p>
            <small>Ctrl / ⌘ + S 保存</small>
          </div>
        )}
      </div>
    </div>
  );
}
