import { useEffect, useRef } from 'react';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import {
  Bot,
  Check,
  Circle,
  CornerDownLeft,
  FileCode2,
  Send,
  ShieldCheck,
  Square,
  Terminal,
  UserRound,
} from 'lucide-react';
import { providerName } from '../types';
import type { Approval, History, LiveSession, Session } from '../types';
import { ExtensionRenderer, matchRenderer } from './Extensions';
import type { Snapshot, DesktopBridge } from '../types';
import { Button } from './ui';

function message(event: History['events'][number]['event']): string {
  const p = event.payload;
  if (event.native_method === 'item/agentMessage/delta') return String(p.delta ?? '');
  if (event.native_method === 'session/update' && p.update?.sessionUpdate === 'agent_message_chunk')
    return String(p.update?.content?.text ?? '');
  if (event.native_method === 'sdk/message') {
    if (p.message?.type === 'stream_event') return String(p.message.event?.delta?.text ?? '');
    if (p.message?.type === 'assistant')
      return (p.message.message?.content ?? [])
        .filter((part: any) => part.type === 'text')
        .map((part: any) => part.text)
        .join('\n');
  }
  return '';
}
export function Chat({
  snapshot,
  bridge,
  session,
  history,
  live,
  draft,
  setDraft,
  busy,
  onSubmit,
  onCancel,
  onApprove,
}: {
  snapshot: Snapshot;
  bridge: DesktopBridge;
  session?: Session;
  history: History;
  live?: LiveSession;
  draft: string;
  setDraft: (value: string) => void;
  busy: boolean;
  onSubmit: () => void;
  onCancel: () => void;
  onApprove: (approval: Approval, decision: 'allow_once' | 'deny') => void;
}) {
  const end = useRef<HTMLDivElement>(null);
  useEffect(() => {
    end.current?.scrollIntoView({ block: 'end', behavior: 'instant' });
  }, [history.events.length, live?.active]);
  const approvals = live?.approvals ?? [];
  const active = live?.active;
  return (
    <div className="chat">
      <div className="chat-scroll">
        {!history.tasks.length && (
          <div className="chat-welcome">
            <div className="welcome-icon">
              <Bot size={30} />
            </div>
            <span className="eyebrow">YOUR WORKSPACE, AMPLIFIED</span>
            <h1>从一个想法，到下一次提交。</h1>
            <p>
              把任务交给你熟悉的 Coding Agent。
              <br />
              在同一个工作台里理解代码、审阅变更、掌握每一步。
            </p>
            <div className="suggestions">
              {['解释这个项目的架构', '查找并修复一个问题', '为最近的改动添加测试'].map((text) => (
                <button key={text} onClick={() => setDraft(text)}>
                  <FileCode2 size={15} />
                  {text}
                  <CornerDownLeft size={13} />
                </button>
              ))}
            </div>
          </div>
        )}
        {history.tasks.map((task) => {
          const events = history.events.filter((event) => event.task_id === task.id);
          const deltas = events
            .filter(
              ({ event }) =>
                event.native_method === 'item/agentMessage/delta' ||
                event.payload.message?.type === 'stream_event',
            )
            .map(({ event }) => message(event))
            .join('');
          const text = deltas || events.map(({ event }) => message(event)).join('');
          const failures = events.filter(
            ({ event }) => event.native_method === 'error' || event.outcome === 'failed',
          );
          const tools = events.filter(
            ({ event }) => event.kind === 'tool_state' || event.kind === 'approval_requested',
          );
          return (
            <section key={task.id} className="conversation">
              <div className="message user-message">
                <div className="avatar">
                  <UserRound size={15} />
                </div>
                <div>
                  <div className="message-meta">你</div>
                  <p>{task.prompt}</p>
                </div>
              </div>
              <div className="message agent-message">
                <div className="avatar agent-avatar">
                  <Bot size={16} />
                </div>
                <div className="message-body">
                  <div className="message-meta">
                    {live?.info.name ?? providerName(session?.provider)}
                    <span className={'task-tag ' + task.state}>{task.state}</span>
                  </div>
                  {tools.map(({ sequence, event }) => {
                    const renderer = matchRenderer(snapshot, 'tool_renderer', event);
                    return (
                      <details className="tool-card" key={sequence}>
                        <summary>
                          <Terminal size={14} />
                          {event.payload.tool ?? event.payload.item?.type ?? event.native_method}
                          <Check size={13} />
                        </summary>
                        {renderer ? (
                          <ExtensionRenderer
                            snapshot={snapshot}
                            extension={renderer}
                            bridge={bridge}
                            input={event}
                          />
                        ) : (
                          <pre>{JSON.stringify(event.payload, null, 2)}</pre>
                        )}
                      </details>
                    );
                  })}
                  {events
                    .filter(({ event }) => matchRenderer(snapshot, 'chat_renderer', event))
                    .map(({ sequence, event }) => (
                      <ExtensionRenderer
                        key={sequence}
                        snapshot={snapshot}
                        extension={matchRenderer(snapshot, 'chat_renderer', event)!}
                        bridge={bridge}
                        input={event}
                      />
                    ))}
                  {failures.length > 0 && (
                    <details className="diagnostic" open>
                      <summary>Agent 报告任务失败</summary>
                      <pre>
                        {failures
                          .map(({ event }) => JSON.stringify(event.payload, null, 2))
                          .join('\n')}
                      </pre>
                    </details>
                  )}
                  {text ? (
                    <Markdown remarkPlugins={[remarkGfm]}>{text}</Markdown>
                  ) : (
                    <p className="muted">
                      {task.state === 'running'
                        ? '正在处理任务…'
                        : task.state === 'unknown'
                          ? '连接中断，任务结果尚未确认。核对原生会话历史后再继续。'
                          : '任务已记录。'}
                    </p>
                  )}
                </div>
              </div>
            </section>
          );
        })}
        {approvals.map((approval) => (
          <section className="approval" key={approval.id}>
            <div className="approval-title">
              <ShieldCheck size={18} />
              <strong>Agent 请求你的批准</strong>
              <span>仅此次操作</span>
            </div>
            <pre>
              {String(
                approval.details.command ??
                  approval.details.input?.command ??
                  JSON.stringify(approval.details.input ?? approval.details, null, 2),
              )}
            </pre>
            <p>请确认操作范围。批准不会为后续操作自动授权。</p>
            <div className="approval-actions">
              <Button variant="outline" onClick={() => onApprove(approval, 'deny')}>
                拒绝
              </Button>
              <Button onClick={() => onApprove(approval, 'allow_once')}>批准一次</Button>
            </div>
          </section>
        ))}
        <div ref={end} />
      </div>
      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          onSubmit();
        }}
      >
        <textarea
          aria-label="任务说明"
          placeholder={session ? '描述任务，或提出一个问题…' : '先连接工作区并创建 Agent 会话…'}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          disabled={!session || busy}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
              event.preventDefault();
              onSubmit();
            }
          }}
        />
        <div className="composer-bottom">
          <span className="model-label">
            <Circle size={8} fill="currentColor" />
            {live?.info.name ?? providerName(session?.provider)}
            <span className="composer-hint">原生执行</span>
          </span>
          <span className="compose-shortcut">⌘ / Ctrl + Enter</span>
          {active ? (
            <Button type="button" variant="outline" size="small" onClick={onCancel}>
              <Square size={12} />
              取消任务
            </Button>
          ) : (
            <Button
              type="submit"
              size="icon"
              aria-label="发送任务"
              disabled={!session || !live || busy || !draft.trim()}
            >
              <Send size={16} />
            </Button>
          )}
        </div>
      </form>
      <div className="chat-footnote">
        <ShieldCheck size={12} /> 操作审批由你掌握 · 会话保存在本地
      </div>
    </div>
  );
}
