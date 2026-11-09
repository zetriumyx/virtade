import { useEffect, useState } from 'react';
import { Modal, Button } from './ui';
import { PluginPanel, type Contribution, type PluginSnapshot } from './Plugins';
import type { DesktopBridge, Snapshot } from '../types';
export type Extension = [string, Contribution];
export function pluginFor(snapshot: Snapshot, owner: string): PluginSnapshot | undefined {
  return snapshot.plugins?.find(
    (plugin) => plugin.package.manifest.id === owner && plugin.generation,
  );
}
export function matchRenderer(
  snapshot: Snapshot,
  kind: string,
  event: { provider: string; native_method: string },
): Extension | undefined {
  return snapshot.contributions?.find(
    ([, view]) =>
      view.kind === kind &&
      (!view.spec.provider || view.spec.provider === event.provider) &&
      (!view.spec.method || view.spec.method === event.native_method),
  );
}
export function ExtensionRenderer({
  snapshot,
  extension,
  bridge,
  input,
}: {
  snapshot: Snapshot;
  extension: Extension;
  bridge: DesktopBridge;
  input?: unknown;
}) {
  const plugin = pluginFor(snapshot, extension[0]);
  return plugin ? (
    <PluginPanel
      owner={extension[0]}
      plugin={plugin}
      view={extension[1]}
      bridge={bridge}
      input={input}
    />
  ) : null;
}
export function ExtensionControls({
  snapshot,
  bridge,
  onOpen,
}: {
  snapshot: Snapshot;
  bridge: DesktopBridge;
  onOpen: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [error, setError] = useState('');
  const [query, setQuery] = useState('');
  const [busy, setBusy] = useState(false);
  const all = snapshot.contributions ?? [];
  const commands = all.filter(([, view]) => ['command', 'workflow'].includes(view.kind));
  const execute = async (extension: Extension) => {
    const [owner, view] = extension;
    const plugin = pluginFor(snapshot, owner);
    if (!plugin) return;
    setBusy(true);
    setError('');
    try {
      if (typeof view.spec.view === 'string') {
        const target = all.find(([id, v]) => id === owner && v.id === view.spec.view);
        if (!target) throw new Error('扩展视图不存在');
        onOpen(target[1].id);
      } else {
        if (!view.spec.service) throw new Error('命令缺少服务入口');
        await bridge.request({
          method: 'plugin_request',
          params: {
            id: owner,
            generation: plugin.generation,
            request: {
              method: 'service_call',
              params: { key: view.spec.service, request: view.spec.request ?? null },
            },
          },
        });
      }
      setOpen(false);
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      const chord = [
        event.ctrlKey || event.metaKey ? 'mod' : '',
        event.altKey ? 'alt' : '',
        event.shiftKey ? 'shift' : '',
        event.key.toLowerCase(),
      ]
        .filter(Boolean)
        .join('+');
      if (chord === 'mod+shift+p') {
        event.preventDefault();
        setOpen((value) => !value);
        return;
      }
      if (
        [
          'mod+s',
          'mod+c',
          'mod+v',
          'mod+x',
          'mod+a',
          'mod+z',
          'mod+shift+z',
          'mod+w',
          'mod+r',
        ].includes(chord) ||
        (event.target instanceof HTMLElement &&
          event.target.closest('input,textarea,[contenteditable="true"],.monaco-editor'))
      )
        return;
      const bindings = all
        .filter(([, view]) => view.kind === 'keybinding' && view.spec.keys?.toLowerCase() === chord)
        .sort((a, b) => a[1].id.localeCompare(b[1].id));
      const binding = bindings[0];
      if (!binding) return;
      const command = commands.find(
        ([owner, view]) => owner === binding[0] && view.id === binding[1].spec.command,
      );
      if (command) {
        event.preventDefault();
        void execute(command);
      }
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, [snapshot, bridge]);
  return (
    <>
      <Button variant="ghost" size="small" onClick={() => setOpen(true)} aria-label="命令面板">
        命令 <kbd>Ctrl ⇧ P</kbd>
      </Button>
      {all
        .filter(([, view]) => view.kind === 'menu')
        .map(([owner, view]) => (
          <Button
            key={view.id}
            size="small"
            variant="ghost"
            onClick={() => {
              const command = commands.find(
                ([id, c]) => id === owner && c.id === view.spec.command,
              );
              if (command) void execute(command);
            }}
          >
            {view.title}
          </Button>
        ))}
      <Modal
        open={open}
        onOpenChange={setOpen}
        title="命令面板"
        description="执行插件命令和工作流。快捷键冲突按扩展 ID 排序，系统编辑快捷键保留。"
      >
        <label className="field">
          查找命令
          <input autoFocus value={query} onChange={(event) => setQuery(event.target.value)} />
        </label>
        {error && (
          <p role="alert" className="diagnostic">
            {error}
          </p>
        )}
        <div className="extension-command-list">
          {commands
            .filter(([, view]) => view.title.toLowerCase().includes(query.toLowerCase()))
            .map((extension) => (
              <Button
                key={extension[1].id}
                variant="ghost"
                disabled={busy}
                onClick={() => void execute(extension)}
              >
                {extension[1].title}
                <small>{extension[0]}</small>
              </Button>
            ))}
          {!commands.length && <p className="muted">尚无插件命令。</p>}
        </div>
      </Modal>
    </>
  );
}
