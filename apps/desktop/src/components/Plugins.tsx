import { useEffect, useRef, useState } from 'react';
import {
  Check,
  Download,
  FileBox,
  Play,
  Power,
  Puzzle,
  RefreshCw,
  ShieldCheck,
  Trash2,
} from 'lucide-react';
import { Button, Modal } from './ui';
import type { DesktopBridge } from '../types';
export interface Permission {
  kind: string;
  target?: unknown;
}
export interface PluginManifest {
  id: string;
  version: string;
  api: string;
  dependencies: { id: string; version: string }[];
  permissions: Permission[];
}
export interface PluginSnapshot {
  package: {
    manifest: PluginManifest;
    metadata: { schema_version: number; component: string; ui: string | null };
    directory: string;
    digest: string;
    granted: Permission[];
    enabled: boolean;
  };
  state: string;
  generation: string | null;
  diagnostics: string[];
}
export interface Contribution {
  id: string;
  kind: string;
  title: string;
  schema_version: number;
  spec: Record<string, any>;
}
export function PluginPanel({
  owner,
  plugin,
  view,
  bridge,
  input = null,
}: {
  input?: unknown;
  owner: string;
  plugin: PluginSnapshot;
  view: Contribution;
  bridge: DesktopBridge;
}) {
  const frame = useRef<HTMLIFrameElement>(null);
  const port = useRef<MessagePort | null>(null);
  const [html, setHtml] = useState('');
  const [error, setError] = useState('');
  useEffect(() => {
    if (view.spec.type !== 'iframe') return;
    let alive = true;
    void bridge
      .request<string>({
        method: 'plugin_ui',
        params: { id: owner, generation: plugin.generation },
      })
      .then((value) => {
        if (alive) setHtml(value);
      })
      .catch((error) => setError(String(error)));
    return () => {
      alive = false;
      port.current?.close();
      port.current = null;
    };
  }, [bridge, owner, plugin.generation]);
  useEffect(() => {
    const ready = (event: MessageEvent) => {
      if (
        event.source === frame.current?.contentWindow &&
        event.data?.type === 'virt:ready' &&
        !port.current
      )
        connect();
    };
    window.addEventListener('message', ready);
    return () => window.removeEventListener('message', ready);
  }, [html, owner, plugin.generation]);
  const connect = () => {
    if (!frame.current?.contentWindow || port.current) return;
    const channel = new MessageChannel();
    port.current = channel.port1;
    let pending = 0;
    channel.port1.onmessage = async (event) => {
      const data = event.data;
      if (data?.type !== 'virt:request' || !Number.isSafeInteger(data.id)) return;
      try {
        if (++pending > 128 || JSON.stringify(data).length > 1024 * 1024)
          throw new Error('extension request quota exceeded');
        const result = await bridge.request({
          method: 'plugin_request',
          params: { id: owner, generation: plugin.generation, request: data.request },
        });
        channel.port1.postMessage({ type: 'virt:response', id: data.id, result });
      } catch (error) {
        channel.port1.postMessage({
          type: 'virt:response',
          id: data.id,
          error: error instanceof Error ? error.message : String(error),
        });
      } finally {
        pending--;
      }
    };
    channel.port1.start();
    frame.current.contentWindow.postMessage(
      { type: 'virt:init', apiVersion: 1, pluginId: owner, viewId: view.id, input },
      '*',
      [channel.port2],
    );
  };
  const secure = html
    ? `<!doctype html><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; frame-src 'none'; form-action 'none'; base-uri 'none';">${html}`
    : '';
  if (view.spec.type !== 'iframe')
    return (
      <div className="extension-text">
        <Puzzle size={24} />
        <h2>{view.title}</h2>
        <p>{String(view.spec.text ?? '')}</p>
      </div>
    );
  return (
    <div className="plugin-panel">
      {error ? (
        <p role="alert">{error}</p>
      ) : html ? (
        <iframe
          ref={frame}
          title={view.title}
          sandbox="allow-scripts"
          srcDoc={secure}
          onLoad={connect}
        />
      ) : (
        <p className="muted">正在加载插件视图…</p>
      )}
    </div>
  );
}
export function PluginManagerView({
  plugins,
  bridge,
  onChanged,
  onSelect,
}: {
  plugins: PluginSnapshot[];
  bridge: DesktopBridge;
  onChanged: () => Promise<void>;
  onSelect: (owner: string, viewId?: string) => void;
}) {
  const exportDiagnostics = async () => {
    try {
      const value = await bridge.request({ method: 'diagnostics' });
      const url = URL.createObjectURL(
        new Blob([JSON.stringify(value, null, 2)], { type: 'application/json' }),
      );
      const link = document.createElement('a');
      link.href = url;
      link.download = 'virtade-diagnostics.json';
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (error) {
      setError(String(error));
    }
  };
  const [source, setSource] = useState('');
  const [manifest, setManifest] = useState<PluginManifest | null>(null);
  const [grants, setGrants] = useState<Set<number>>(new Set());
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [review, setReview] = useState(false);
  const act = async (method: string, params: Record<string, unknown>) => {
    setBusy(true);
    setError('');
    try {
      await bridge.request({ method, params });
      await onChanged();
      return true;
    } catch (error) {
      setError(error instanceof Error ? error.message : String(error));
      return false;
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="plugin-manager">
      <div className="view-heading">
        <div>
          <span className="eyebrow">BUILD YOUR ENVIRONMENT</span>
          <h1>插件</h1>
          <p className="muted">让工作台适应你的工作方式。</p>
        </div>
        <Button
          variant="outline"
          disabled={!bridge.available}
          onClick={() => void exportDiagnostics()}
        >
          导出诊断
        </Button>
      </div>
      <div className="plugin-install">
        <label className="field">
          本地插件目录
          <input
            value={source}
            placeholder="包含 manifest.json 和 component.wasm 的目录"
            onChange={(event) => setSource(event.target.value)}
          />
        </label>
        <Button
          variant="outline"
          disabled={!bridge.available || !source || busy}
          onClick={() => {
            setError('');
            void bridge
              .request<PluginManifest>({ method: 'plugin_inspect', params: { source } })
              .then((value) => {
                setManifest(value);
                setGrants(new Set());
                setReview(true);
              })
              .catch((error) => setError(String(error)));
          }}
        >
          <Download size={14} />
          检查并安装
        </Button>
      </div>
      {error && (
        <p role="alert" className="diagnostic">
          {error}
        </p>
      )}
      <div className="plugin-cards">
        {plugins.map((plugin) => {
          const id = plugin.package.manifest.id;
          return (
            <article key={id} className="plugin-card">
              <div className="plugin-card-title">
                <span className="plugin-icon">
                  <Puzzle size={22} />
                </span>
                <div>
                  <h3>{id}</h3>
                  <small>v{plugin.package.manifest.version} · WASM Component</small>
                </div>
                <span className={'plugin-state ' + (plugin.package.enabled ? 'enabled' : '')}>
                  {plugin.state}
                </span>
              </div>
              <div className="plugin-permissions">
                <ShieldCheck size={12} />
                {plugin.package.granted.length} 项已授权能力 · API {plugin.package.manifest.api}
              </div>
              {plugin.diagnostics.length > 0 && (
                <details>
                  <summary>诊断记录</summary>
                  <pre>{plugin.diagnostics.join('\n')}</pre>
                </details>
              )}
              <div className="plugin-card-actions">
                {plugin.package.enabled ? (
                  <>
                    <Button
                      size="small"
                      variant="outline"
                      onClick={() => void act('plugin_disable', { id })}
                    >
                      <Power size={12} />
                      禁用
                    </Button>
                    <Button
                      size="small"
                      variant="ghost"
                      onClick={() => void act('plugin_reload', { id })}
                    >
                      <RefreshCw size={12} />
                      重载
                    </Button>
                  </>
                ) : (
                  <Button
                    size="small"
                    variant="outline"
                    onClick={() => void act('plugin_enable', { id })}
                  >
                    <Play size={12} />
                    启用
                  </Button>
                )}
                {plugin.package.enabled && (
                  <Button size="small" variant="ghost" onClick={() => onSelect(id)}>
                    查看扩展
                  </Button>
                )}
                <Button
                  size="small"
                  variant="ghost"
                  onClick={() => void act('plugin_uninstall', { id })}
                >
                  <Trash2 size={12} />
                  卸载
                </Button>
              </div>
            </article>
          );
        })}
        {!plugins.length && (
          <div className="plugins-empty">
            <FileBox size={34} />
            <h3>创建属于你的工作台</h3>
            <p>
              从一个本地插件开始。插件运行在隔离环境中，
              <br />
              只能使用你明确授予的能力。
            </p>
          </div>
        )}
      </div>
      <Modal
        open={review}
        onOpenChange={setReview}
        title="安装插件"
        description="权限声明不是授权。选择需要授予的能力，未勾选的能力将被拒绝。"
      >
        <div className="plugin-review-title">
          <Puzzle size={20} />
          <strong>{manifest?.id}</strong>
          <span>v{manifest?.version}</span>
        </div>
        <div className="permission-options">
          {manifest?.permissions.map((permission, index) => (
            <label key={index}>
              <input
                type="checkbox"
                checked={grants.has(index)}
                onChange={(event) => {
                  const next = new Set(grants);
                  event.target.checked ? next.add(index) : next.delete(index);
                  setGrants(next);
                }}
              />
              <span>
                <strong>{permission.kind}</strong>
                <code>
                  {typeof permission.target === 'string'
                    ? permission.target
                    : permission.target
                      ? JSON.stringify(permission.target)
                      : '插件私有配置'}
                </code>
              </span>
            </label>
          ))}
        </div>
        <p className="muted small-note">
          文件写入和进程执行会产生真实副作用。UI 扩展可能影响界面响应，请核实插件来源。
        </p>
        <div className="modal-actions">
          <Button variant="outline" onClick={() => setReview(false)}>
            取消
          </Button>
          <Button
            disabled={busy}
            onClick={() => {
              void act('plugin_install', {
                source,
                granted: manifest?.permissions.filter((_, index) => grants.has(index)) ?? [],
              }).then((success) => {
                if (success) setReview(false);
              });
            }}
          >
            <Check size={14} />
            安装并保留禁用
          </Button>
        </div>
      </Modal>
    </div>
  );
}
