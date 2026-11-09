/** UI SDK v0.1.0. Runs inside an opaque sandbox; only the transferred port is an authority. */
export interface ServiceKey {
  name: string;
  major: number;
}
export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type ContributionKind =
  | 'panel'
  | 'sidebar'
  | 'chat_renderer'
  | 'tool_renderer'
  | 'command'
  | 'menu'
  | 'keybinding'
  | 'status'
  | 'settings'
  | 'editor'
  | 'workspace_view'
  | 'agent_provider'
  | 'workflow';
export interface Contribution {
  id: string;
  kind: ContributionKind;
  title: string;
  schema_version: 1;
  spec: Json;
}
export type ResourceRequest =
  | RpcResourceRequest
  | { operation: 'read_file'; params: { root: string; path: string } }
  | { operation: 'write_file'; params: { root: string; path: string; content: string } }
  | { operation: 'fetch'; params: { url: string } }
  | { operation: 'run'; params: { program: string; cwd: string; args: string[] } };
export type RpcResourceRequest =
  | {
      operation: 'rpc_open';
      params: { program: string; cwd: string; args: string[]; json_rpc_2?: boolean };
    }
  | {
      operation: 'rpc_start';
      params: { id: string; method: string; params: Json; timeout_ms: number };
    }
  | { operation: 'rpc_notify'; params: { id: string; method: string; params: Json } }
  | {
      operation: 'rpc_respond';
      params: {
        id: string;
        request_id: string | number;
        result?: Json;
        error?: { code: number; message: string };
      };
    }
  | { operation: 'rpc_poll'; params: { id: string; after: number } }
  | { operation: 'rpc_close'; params: { id: string } };
export interface PluginContext {
  readonly pluginId: string;
  readonly viewId: string;
  readonly input: Json;
  services: { call<T extends Json = Json>(key: ServiceKey, request: Json): Promise<T> };
  config: {
    get<T extends Json = Json>(key: string): Promise<T | null>;
    set(key: string, value: Json): Promise<void>;
  };
  resources: { request<T extends Json = Json>(request: ResourceRequest): Promise<T> };
  rpc: {
    open(config: {
      program: string;
      cwd: string;
      args: string[];
      json_rpc_2?: boolean;
    }): Promise<{ id: string }>;
    start(
      id: string,
      method: string,
      params: Json,
      timeoutMs?: number,
    ): Promise<{ operation_id: string }>;
    notify(id: string, method: string, params: Json): Promise<void>;
    respond(
      id: string,
      requestId: string | number,
      response: { result: Json } | { error: { code: number; message: string } },
    ): Promise<void>;
    poll(
      id: string,
      after?: number,
    ): Promise<{ events: { sequence: number; frame: Json }[]; closed: string | null }>;
    close(id: string): Promise<void>;
  };
  agents: {
    providers(): Promise<Json>;
    openSession(provider: string, root: string, resume?: string): Promise<Json>;
    submit(sessionId: string, text: string, requestKey?: string): Promise<Json>;
    cancel(sessionId: string): Promise<void>;
    history(sessionId: string, after?: number, limit?: number): Promise<Json>;
  };
  contribute(definition: Contribution): Promise<void>;
  on(topic: string, listener: (payload: Json) => void): Promise<() => void>;
  dispose(): void;
}
export async function connectVirt(timeout = 10_000): Promise<PluginContext> {
  if (window.parent === window) throw new Error('virt UI SDK requires the isolated extension host');
  const init = await new Promise<{
    port: MessagePort;
    pluginId: string;
    viewId: string;
    input: Json;
  }>((resolve, reject) => {
    const timer = setTimeout(() => {
      window.removeEventListener('message', listener);
      reject(new Error('extension host handshake timed out'));
    }, timeout);
    function listener(event: MessageEvent) {
      if (
        event.source !== window.parent ||
        event.data?.type !== 'virt:init' ||
        event.data?.apiVersion !== 1 ||
        !event.ports[0]
      )
        return;
      clearTimeout(timer);
      window.removeEventListener('message', listener);
      resolve({
        port: event.ports[0],
        pluginId: String(event.data.pluginId),
        viewId: String(event.data.viewId ?? ''),
        input: event.data.input ?? null,
      });
    }
    window.addEventListener('message', listener);
    window.parent.postMessage({ type: 'virt:ready', apiVersion: 1 }, '*');
  });
  let next = 0,
    closed = false,
    cursor = 0,
    dropped = 0;
  const pending = new Map<
    number,
    {
      resolve: (value: any) => void;
      reject: (error: Error) => void;
      timer: ReturnType<typeof setTimeout>;
    }
  >();
  const listeners = new Map<string, Set<(payload: Json) => void>>();
  init.port.onmessage = (event) => {
    const value = event.data;
    if (value?.type !== 'virt:response' || !Number.isSafeInteger(value.id)) return;
    const request = pending.get(value.id);
    if (!request) return;
    pending.delete(value.id);
    clearTimeout(request.timer);
    if (value.error) request.reject(new Error(String(value.error)));
    else request.resolve(value.result);
  };
  init.port.start();
  const request = <T>(method: string, params?: unknown) =>
    new Promise<T>((resolve, reject) => {
      if (closed) {
        reject(new Error('plugin context disposed'));
        return;
      }
      const id = ++next;
      const payload = { type: 'virt:request', id, request: { method, params } };
      if (JSON.stringify(payload).length > 1024 * 1024 || pending.size >= 128) {
        reject(new Error('plugin request quota exceeded'));
        return;
      }
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error('host request timed out; outcome may be unknown'));
      }, 5000);
      pending.set(id, { resolve, reject, timer });
      init.port.postMessage(payload);
    });
  const resource = <T>(payload: ResourceRequest) => request<T>('resource', { request: payload });
  let polling = false;
  const poll = setInterval(async () => {
    if (polling || !listeners.size || closed) return;
    polling = true;
    try {
      const result = await request<{
        events: { sequence: number; topic: string; payload: Json }[];
        dropped: number;
      }>('event_poll', { after: cursor });
      if (result.dropped > dropped) {
        console.warn('virt event queue dropped events', result.dropped - dropped);
        dropped = result.dropped;
      }
      for (const event of result.events) {
        cursor = Math.max(cursor, event.sequence);
        for (const listener of listeners.get(event.topic) ?? []) {
          try {
            listener(event.payload);
          } catch (error) {
            console.error('virt plugin listener failed', error);
          }
        }
      }
    } catch (error) {
      console.error('virt event subscription failed', error);
    } finally {
      polling = false;
    }
  }, 250);
  return {
    pluginId: init.pluginId,
    viewId: init.viewId,
    input: init.input,
    services: { call: (key, payload) => request('service_call', { key, request: payload }) },
    config: {
      get: (key) => request('config_get', { key }),
      set: (key, value) => request('config_set', { key, value }),
    },
    resources: { request: resource },
    rpc: {
      open: (config) => resource({ operation: 'rpc_open', params: config }),
      start: (id, method, params, timeout_ms = 120_000) =>
        resource({ operation: 'rpc_start', params: { id, method, params, timeout_ms } }),
      notify: (id, method, params) =>
        resource({ operation: 'rpc_notify', params: { id, method, params } }),
      respond: (id, request_id, response) =>
        resource({ operation: 'rpc_respond', params: { id, request_id, ...response } }),
      poll: (id, after = 0) => resource({ operation: 'rpc_poll', params: { id, after } }),
      close: (id) => resource({ operation: 'rpc_close', params: { id } }),
    },
    agents: {
      providers: () =>
        request('service_call', {
          key: { name: 'virtade.agents', major: 1 },
          request: { method: 'providers' },
        }),
      openSession: (provider, root, resume) =>
        request('service_call', {
          key: { name: 'virtade.agents', major: 1 },
          request: { method: 'open_session', params: { provider, root, resume: resume ?? null } },
        }),
      submit: (session_id, text, request_key = crypto.randomUUID()) =>
        request('service_call', {
          key: { name: 'virtade.agents', major: 1 },
          request: { method: 'submit', params: { session_id, text, request_key } },
        }),
      cancel: (session_id) =>
        request('service_call', {
          key: { name: 'virtade.agents', major: 1 },
          request: { method: 'cancel', params: { session_id } },
        }),
      history: (session_id, after = 0, limit = 100) =>
        request('service_call', {
          key: { name: 'virtade.agents', major: 1 },
          request: { method: 'history', params: { session_id, after, limit } },
        }),
    },
    contribute: (definition) => request('contribute', { definition }),
    on: async (topic, listener) => {
      await request('event_subscribe', { topic });
      let set = listeners.get(topic);
      if (!set) {
        set = new Set();
        listeners.set(topic, set);
      }
      set.add(listener);
      return () => {
        set!.delete(listener);
        if (!set!.size) listeners.delete(topic);
      };
    },
    dispose: () => {
      closed = true;
      clearInterval(poll);
      init.port.close();
      for (const value of pending.values()) {
        clearTimeout(value.timer);
        value.reject(new Error('plugin context disposed'));
      }
      pending.clear();
      listeners.clear();
    },
  };
}
