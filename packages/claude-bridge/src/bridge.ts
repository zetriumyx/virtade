import { randomUUID } from 'node:crypto';
import { isAbsolute } from 'node:path';
import { stat } from 'node:fs/promises';
import { createRequire } from 'node:module';
import {
  query,
  getSessionInfo,
  type Query,
  type PermissionResult,
  type SDKMessage,
  type SDKUserMessage,
} from '@anthropic-ai/claude-agent-sdk';

const pkg = createRequire(import.meta.url)('../package.json') as {
  dependencies: Record<string, string>;
};
export const SDK_VERSION = pkg.dependencies['@anthropic-ai/claude-agent-sdk']!;
export type Frame = Record<string, unknown>;
type Sdk = { query: typeof query; getSessionInfo: typeof getSessionInfo };
type Session = {
  cwd: string;
  persisted: boolean;
  active?: Active;
  model?: string;
  unknown?: boolean;
};
type Active = { id: string; query: Query; abort: AbortController; cancelled: boolean };
type Pending = {
  session: string;
  task: string;
  resolve: (value: PermissionResult) => void;
  input: Record<string, unknown>;
  timer: NodeJS.Timeout;
  signal: AbortSignal;
  onAbort: () => void;
};

function object(value: unknown): Frame {
  if (!value || typeof value !== 'object' || Array.isArray(value))
    throw new Error('expected object');
  return value as Frame;
}
function text(value: unknown, key: string): string {
  if (typeof value !== 'string' || !value) throw new Error(`missing ${key}`);
  return value;
}

/** Official SDK adapter only; reasoning, tools and context remain in Claude Code. */
export class ClaudeBridge {
  private sessions = new Map<string, Session>();
  private pending = new Map<string, Pending>();
  private closed = false;
  constructor(
    private send: (frame: Frame) => void,
    private sdk: Sdk = { query, getSessionInfo },
    private approvalTimeout = 120_000,
  ) {}

  private event(method: string, params: Frame) {
    this.send({ method, params });
  }

  async handle(frame: Frame): Promise<void> {
    if (this.closed) throw new Error('bridge closed');
    const id = frame.id;
    // Native approval response: one-time opaque ID, never a general tool execution endpoint.
    if (typeof frame.method !== 'string') {
      if (typeof id !== 'string') throw new Error('invalid response id');
      const pending = this.pending.get(id);
      if (!pending) throw new Error('approval expired or already resolved');
      const result = object(frame.result);
      if (result.decision !== 'allow_once' && result.decision !== 'deny')
        throw new Error('invalid decision');
      this.resolve(
        id,
        result.decision === 'allow_once'
          ? { behavior: 'allow', updatedInput: pending.input }
          : { behavior: 'deny', message: 'Denied by virtade host' },
      );
      return;
    }
    if (typeof id !== 'number' && typeof id !== 'string') throw new Error('request id required');
    try {
      const params = object(frame.params ?? {});
      let result: unknown;
      switch (frame.method) {
        case 'initialize':
          result = {
            protocol_version: 1,
            sdk_version: SDK_VERSION,
            capabilities: [
              'session.reserve',
              'session.resume',
              'task.stream',
              'task.interrupt',
              'tool.approval.once',
            ],
          };
          break;
        case 'session/create': {
          const cwd = await this.cwd(params.cwd);
          if (this.sessions.size >= 1024) throw new Error('session quota exceeded');
          const options = object(params.provider_options ?? {});
          for (const key of Object.keys(options))
            if (key !== 'model' || typeof options[key] !== 'string')
              throw new Error(`unsupported option ${key}`);
          const sessionId = randomUUID();
          this.sessions.set(sessionId, {
            cwd,
            persisted: false,
            model: options.model as string | undefined,
          });
          result = { session_id: sessionId, cwd, reserved: true };
          break;
        }
        case 'session/resume': {
          const sessionId = text(params.session_id, 'session_id');
          const cwd = await this.cwd(params.cwd);
          if (this.sessions.get(sessionId)?.active) throw new Error('session busy');
          const info = await this.sdk.getSessionInfo(sessionId, { dir: cwd });
          if (!info) throw new Error('native session not found');
          if (this.closed || this.sessions.get(sessionId)?.active)
            throw new Error('session busy or bridge closed');
          if (!this.sessions.has(sessionId) && this.sessions.size >= 1024)
            throw new Error('session quota exceeded');
          this.sessions.set(sessionId, { cwd, persisted: true });
          result = { session_id: sessionId, cwd, reserved: false };
          break;
        }
        case 'task/submit': {
          const sessionId = text(params.session_id, 'session_id');
          const prompt = text(params.text, 'text');
          const session = this.sessions.get(sessionId);
          if (!session) throw new Error('session not created or resumed');
          if (session.active) throw new Error('session busy');
          if (session.unknown)
            throw new Error('unknown task outcome; explicitly resume before submitting again');
          const taskId = randomUUID();
          const abort = new AbortController();
          async function* input(): AsyncGenerator<SDKUserMessage> {
            yield {
              type: 'user',
              session_id: sessionId,
              parent_tool_use_id: null,
              message: { role: 'user', content: prompt },
            };
          }
          const stream = this.sdk.query({
            prompt: input(),
            options: {
              cwd: session.cwd,
              ...(session.persisted ? { resume: sessionId } : { sessionId }),
              model: session.model,
              abortController: abort,
              includePartialMessages: true,
              permissionMode: 'default',
              settingSources: ['user', 'project', 'local'],
              ...(process.env.VIRTADE_CLAUDE_EXECUTABLE
                ? { pathToClaudeCodeExecutable: process.env.VIRTADE_CLAUDE_EXECUTABLE }
                : {}),
              canUseTool: async (tool, input, options) =>
                this.approve(sessionId, taskId, tool, input, options.signal),
            },
          });
          const active: Active = { id: taskId, query: stream, abort, cancelled: false };
          session.active = active;
          this.send({ id, result: { session_id: sessionId, task_id: taskId } });
          void this.consume(sessionId, session, active);
          return;
        }
        case 'task/cancel': {
          const session = this.sessions.get(text(params.session_id, 'session_id'));
          const taskId = text(params.task_id, 'task_id');
          if (!session?.active || session.active.id !== taskId) throw new Error('task not active');
          session.active.cancelled = true;
          await session.active.query.interrupt();
          result = { cancellation_requested: true };
          break;
        }
        default:
          throw new Error(`unsupported method ${frame.method}`);
      }
      this.send({ id, result });
    } catch (error) {
      this.send({
        id,
        error: { code: -32000, message: error instanceof Error ? error.message : 'bridge failure' },
      });
    }
  }

  private async cwd(value: unknown): Promise<string> {
    const cwd = text(value, 'cwd');
    if (!isAbsolute(cwd) || !(await stat(cwd)).isDirectory())
      throw new Error('cwd must be an existing absolute directory');
    return cwd;
  }

  private approve(
    session: string,
    task: string,
    tool: string,
    input: Record<string, unknown>,
    signal: AbortSignal,
  ): Promise<PermissionResult> {
    if (this.closed || signal.aborted || this.pending.size >= 128)
      return Promise.resolve({ behavior: 'deny', message: 'Approval unavailable' });
    return new Promise((resolve) => {
      const id = `approval:${randomUUID()}`;
      const onAbort = () =>
        this.resolve(id, { behavior: 'deny', message: 'Native approval cancelled' });
      const timer = setTimeout(
        () => this.resolve(id, { behavior: 'deny', message: 'Approval expired' }),
        this.approvalTimeout,
      );
      this.pending.set(id, { session, task, input, resolve, timer, signal, onAbort });
      signal.addEventListener('abort', onAbort, { once: true });
      this.send({
        id,
        method: 'tool/approval',
        params: { session_id: session, task_id: task, tool, input },
      });
    });
  }

  private resolve(id: string, result: PermissionResult) {
    const pending = this.pending.get(id);
    if (!pending) return;
    this.pending.delete(id);
    clearTimeout(pending.timer);
    pending.signal.removeEventListener('abort', pending.onAbort);
    pending.resolve(result);
  }

  private async consume(sessionId: string, session: Session, active: Active) {
    let result: SDKMessage | undefined;
    let error: string | undefined;
    try {
      for await (const message of active.query) {
        // Keep all provider-specific fields. Never reduce the SDK to text deltas only.
        this.event('sdk/message', { session_id: sessionId, task_id: active.id, message });
        if (message.type === 'system' && message.subtype === 'init') {
          if (message.session_id !== sessionId)
            throw new Error('SDK returned an unexpected session ID');
          session.persisted = true;
        }
        if (message.type === 'result') {
          result = message;
          session.persisted = true;
        }
      }
    } catch (caught) {
      error = caught instanceof Error ? caught.message : 'SDK stream failed';
    } finally {
      for (const [id, pending] of this.pending)
        if (pending.session === sessionId && pending.task === active.id)
          this.resolve(id, { behavior: 'deny', message: 'Task ended' });
      session.active = undefined;
      session.unknown = !result;
      this.event('task/ended', {
        session_id: sessionId,
        task_id: active.id,
        native_result: result ?? null,
        error: error ?? null,
        cancellation_requested: active.cancelled,
        outcome: result ? 'native_result' : 'unknown',
      });
    }
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    for (const id of this.pending.keys())
      this.resolve(id, { behavior: 'deny', message: 'Host disconnected' });
    for (const session of this.sessions.values()) session.active?.abort.abort();
  }
}

/** Bound input while accumulating, including streams without a trailing newline. */
export class FrameDecoder {
  private pending = Buffer.alloc(0);
  constructor(private maxBytes = 1024 * 1024) {}
  push(chunk: Buffer): Frame[] {
    const frames: Frame[] = [];
    let start = 0;
    while (start < chunk.length) {
      const newline = chunk.indexOf(10, start);
      const end = newline < 0 ? chunk.length : newline + 1;
      if (this.pending.length + end - start > this.maxBytes)
        throw new Error('inbound frame exceeds limit');
      this.pending = Buffer.concat([this.pending, chunk.subarray(start, end)]);
      if (newline >= 0) {
        frames.push(object(JSON.parse(this.pending.toString('utf8'))));
        this.pending = Buffer.alloc(0);
      }
      start = end;
    }
    return frames;
  }
  end() {
    if (this.pending.length) throw new Error('truncated frame');
  }
}
