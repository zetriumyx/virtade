import test from "node:test";
import assert from "node:assert/strict";
import { ClaudeBridge, FrameDecoder, SDK_VERSION } from "../dist/bridge.js";

const cwd = process.cwd();
const wait = async (predicate) => {
  const timeout = Date.now() + 2000;
  while (!predicate()) { if (Date.now() > timeout) throw new Error("condition timeout"); await new Promise((resolve) => setTimeout(resolve, 1)); }
};

test("framing bounds unterminated input, truncated frames, and multiple frames", () => {
  const decoder = new FrameDecoder(32);
  assert.deepEqual(decoder.push(Buffer.from('{"id":1}\n{"id":2}\n')), [{ id: 1 }, { id: 2 }]);
  decoder.push(Buffer.from('{"id":'));
  assert.throws(() => decoder.end(), /truncated/);
  assert.throws(() => decoder.push(Buffer.alloc(33)), /limit/);
});

test("official SDK adapter preserves sessions, streaming payload, and one-time approvals", async () => {
  const frames = []; const queries = [];
  const sdk = {
    getSessionInfo: async (id) => id === "saved" ? { sessionId: id } : undefined,
    query: (args) => {
      queries.push(args); const id = args.options.sessionId ?? args.options.resume;
      return {
        async *[Symbol.asyncIterator]() {
          yield { type: "system", subtype: "init", session_id: id, native_extra: "preserved" };
          const decision = await args.options.canUseTool("Bash", { command: "fixture" }, { signal: new AbortController().signal });
          yield { type: "result", session_id: id, subtype: "success", is_error: false, decision };
        },
        interrupt: async () => ({}),
      };
    },
  };
  const bridge = new ClaudeBridge((frame) => frames.push(frame), sdk);
  await bridge.handle({ id: 1, method: "initialize", params: {} }); assert.equal(frames[0].result.sdk_version, SDK_VERSION);
  await bridge.handle({ id: 2, method: "session/create", params: { cwd } });
  const id = frames.find((frame) => frame.id === 2).result.session_id;
  await bridge.handle({ id: 3, method: "task/submit", params: { session_id: id, text: "fixture prompt" } });
  await wait(() => frames.some((frame) => frame.method === "tool/approval"));
  const approval = frames.find((frame) => frame.method === "tool/approval");
  await bridge.handle({ id: approval.id, result: { decision: "allow_once" } });
  await assert.rejects(bridge.handle({ id: approval.id, result: { decision: "allow_once" } }), /resolved/);
  await wait(() => frames.some((frame) => frame.method === "task/ended"));
  assert.equal(queries[0].options.sessionId, id);
  assert.equal(queries[0].options.permissionMode, "default");
  assert.equal(frames.find((frame) => frame.method === "sdk/message").params.message.native_extra, "preserved");
  await bridge.handle({ id: 4, method: "task/submit", params: { session_id: id, text: "follow up" } });
  assert.equal(queries[1].options.resume, id); assert.equal(queries[1].options.sessionId, undefined);
  bridge.close();
});

test("approval timeout defaults to denial and missing sessions do not silently create new ones", async () => {
  const frames = []; let decision;
  const sdk = {
    getSessionInfo: async () => undefined,
    query: (args) => ({ async *[Symbol.asyncIterator]() {
      decision = await args.options.canUseTool("Write", { file_path: "fixture" }, { signal: new AbortController().signal });
      yield { type: "result", subtype: "success", session_id: args.options.sessionId, is_error: false };
    } }),
  };
  const bridge = new ClaudeBridge((frame) => frames.push(frame), sdk, 10);
  await bridge.handle({ id: 1, method: "session/resume", params: { cwd, session_id: "missing" } });
  assert.match(frames[0].error.message, /not found/);
  await bridge.handle({ id: 2, method: "session/create", params: { cwd } }); const session = frames[1].result.session_id;
  await bridge.handle({ id: 3, method: "task/submit", params: { session_id: session, text: "fixture" } });
  await wait(() => decision); assert.equal(decision.behavior, "deny"); bridge.close();
});

test("stream ending without native result remains unknown after interrupt request", async () => {
  const frames = []; let finish;
  const sdk = { getSessionInfo: async () => undefined, query: () => ({ async *[Symbol.asyncIterator]() { await new Promise((resolve) => { finish = resolve; }); }, interrupt: async () => { finish(); } }) };
  const bridge = new ClaudeBridge((frame) => frames.push(frame), sdk);
  await bridge.handle({ id: 1, method: "session/create", params: { cwd } }); const session = frames[0].result.session_id;
  await bridge.handle({ id: 2, method: "task/submit", params: { session_id: session, text: "fixture" } });
  const task = frames.find((frame) => frame.id === 2).result.task_id;
  await bridge.handle({ id: 3, method: "task/cancel", params: { session_id: session, task_id: task } });
  await wait(() => frames.some((frame) => frame.method === "task/ended"));
  const terminal = frames.find((frame) => frame.method === "task/ended").params;
  assert.equal(terminal.outcome, "unknown"); assert.equal(terminal.cancellation_requested, true);
  await bridge.handle({ id: 4, method: "task/submit", params: { session_id: session, text: "do not replay" } });
  assert.match(frames.find((frame) => frame.id === 4).error.message, /unknown task outcome/);
  bridge.close();
});
