import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import type { DesktopBridge, Workspace } from '../types';
const terminals = new Map<string, string>();
const opening = new Map<string, Promise<string>>();
export async function closeTerminal(bridge: DesktopBridge, workspaceId: string) {
  const id = terminals.get(workspaceId) ?? (await opening.get(workspaceId));
  terminals.delete(workspaceId);
  opening.delete(workspaceId);
  if (id)
    await bridge.request({
      method: 'workbench',
      params: {
        workspace_id: workspaceId,
        request: { operation: 'terminal_close', params: { id } },
      },
    });
}
export function TerminalPane({
  bridge,
  workspace,
  theme,
}: {
  bridge: DesktopBridge;
  workspace?: Workspace;
  theme: 'dark' | 'light';
}) {
  const container = useRef<HTMLDivElement>(null);
  const [error, setError] = useState('');
  useEffect(() => {
    if (!workspace || !container.current || !bridge.available) return;
    let disposed = false;
    let id = '';
    let cursor = 0;
    let polling = false;
    let timer: ReturnType<typeof setInterval> | undefined;
    let resizeTimer: ReturnType<typeof setTimeout> | undefined;
    const terminal = new Terminal({
      fontSize: 12,
      fontFamily: 'Cascadia Code, SFMono-Regular, Consolas, monospace',
      scrollback: 3000,
      convertEol: false,
      theme: {
        background: theme === 'dark' ? '#101315' : '#f9faf8',
        foreground: theme === 'dark' ? '#dce4df' : '#26362e',
        cursor: '#8de1bf',
      },
    });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(container.current);
    fit.fit();
    const call = <T,>(operation: string, params: Record<string, unknown>) =>
      bridge.request<T>({
        method: 'workbench',
        params: { workspace_id: workspace.id, request: { operation, params } },
      });
    const ensure = async () => {
      if (terminals.has(workspace.id)) return terminals.get(workspace.id)!;
      let promise = opening.get(workspace.id);
      if (!promise) {
        promise = call<{ id: string }>('terminal_open', {
          cols: Math.max(2, terminal.cols),
          rows: Math.max(2, terminal.rows),
        }).then((value) => {
          terminals.set(workspace.id, value.id);
          opening.delete(workspace.id);
          return value.id;
        });
        opening.set(workspace.id, promise);
      }
      return promise;
    };
    void ensure()
      .then((value) => {
        if (disposed) return;
        id = value;
        const poll = async () => {
          if (polling || disposed) return;
          polling = true;
          try {
            const value = await call<{
              cursor: number;
              bytes: number[];
              dropped: boolean;
              exited: number | null;
            }>('terminal_poll', { id, after: cursor });
            if (disposed) return;
            if (value.dropped) terminal.writeln('\r\n[较早的终端输出已超出缓冲区]\r\n');
            if (value.bytes.length) terminal.write(new Uint8Array(value.bytes));
            cursor = value.cursor;
            if (value.exited !== null && value.bytes.length < 65536) {
              terminal.writeln(`\r\n[进程已退出：${value.exited}]`);
              if (timer) clearInterval(timer);
              terminals.delete(workspace.id);
            }
          } catch (error) {
            setError(String(error instanceof Error ? error.message : error));
            terminals.delete(workspace.id);
            if (timer) clearInterval(timer);
          } finally {
            polling = false;
          }
        };
        void poll();
        timer = setInterval(() => void poll(), 100);
        terminal.focus();
      })
      .catch((error) => {
        opening.delete(workspace.id);
        setError(String(error));
      });
    const input = terminal.onData((data) => {
      if (id) void call('terminal_write', { id, data }).catch((error) => setError(String(error)));
    });
    const observer = new ResizeObserver(() => {
      if (resizeTimer) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        if (disposed) return;
        fit.fit();
        if (id)
          void call('terminal_resize', {
            id,
            cols: Math.max(2, terminal.cols),
            rows: Math.max(2, terminal.rows),
          }).catch((error) => setError(String(error)));
      }, 80);
    });
    observer.observe(container.current);
    return () => {
      disposed = true;
      if (timer) clearInterval(timer);
      if (resizeTimer) clearTimeout(resizeTimer);
      observer.disconnect();
      input.dispose();
      terminal.dispose();
    };
  }, [bridge, workspace?.id, theme]);
  return (
    <>
      {error && (
        <p role="alert" className="diagnostic">
          {error}
        </p>
      )}
      <div className="xterm-container" ref={container} />
      {!workspace && <p className="empty-caption">打开工作区以启动终端。</p>}
    </>
  );
}
