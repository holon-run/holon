import assert from 'node:assert/strict';
import test from 'node:test';
import {
  ConversationController, ConversationClient, ConversationCapabilityError,
  ConversationProtocolError, CONVERSATION_HISTORY_BIDIRECTIONAL_CAPABILITY,
  ConversationDecodeError, decodeConversationLocateResponse,
} from '../dist/index.js';
import { summary, turn, batch, detail } from './helpers.mjs';

function page(ids, before = null, after = null, extra = {}) {
  const key = id => ({ turn_index: id, turn_id: `turn-${id}` });
  return summary({
    turns: ids.map(id => turn(`turn-${id}`, id)),
    next_before_cursor: before, next_after_cursor: after, has_more: before !== null,
    coverage: {
      first_turn_key: ids.length ? key(ids[0]) : null,
      last_turn_key: ids.length ? key(ids.at(-1)) : null,
      membership_upper_bound: ids.length ? key(Math.max(10, ...ids)) : null,
      has_older: before !== null, has_newer: after !== null, reaches_oldest: before === null, reaches_latest: after === null,
    },
    ...extra,
  });
}
function located(window = page([4, 5], 'b4', 'a5'), target = { kind: 'input', message_id: 'message-5', turn_id: 'turn-5' }) {
  return { status: 'located', source_ref: 'message:message-5', reason: null, target, window,
    runtime_id: window.runtime_id, event_log_epoch: window.event_log_epoch, visibility_scope_id: window.visibility_scope_id };
}
function deferred() { let resolve, reject; const promise = new Promise((r, j) => { resolve = r; reject = j; }); return { promise, resolve, reject }; }
async function tick() { await new Promise(r => setImmediate(r)); }
async function fixture(options = {}) {
  const queue = [];
  let wake;
  const calls = [];
  const client = {
    baseUrl: 'http://fake.local',
    async requireCapability() { return {}; },
    async summary(_agent, opts = {}) {
      calls.push(opts);
      return { summary: await (opts.before || opts.after ? options.page(opts) : page([9, 10], 'b9')), etag: null };
    },
    async locateSource(_agent, ref, opts) { return options.locate ? options.locate(ref, opts) : located(); },
    async activities(_agent, turnId, opts) {
      if (options.activities) return options.activities(turnId, opts);
      throw new Error('not used');
    },
    async brief() { throw new Error('not used'); },
    async *stream(_agent, { signal }) {
      while (!signal.aborted) {
        if (queue.length) { yield queue.shift(); continue; }
        await new Promise(resolve => {
          const resume = () => { signal.removeEventListener('abort', resume); resolve(); };
          wake = resume;
          signal.addEventListener('abort', resume, { once: true });
        });
      }
    },
  };
  if (options.noLocate) delete client.locateSource;
  const controller = new ConversationController({ client, agentId: 'web', ...(options.limits ? { limits: options.limits } : {}) });
  controller.start();
  await tick();
  assert.equal(controller.status.kind, 'ready');
  return { controller, calls, async push(item) { queue.push(item); wake?.(); await tick(); } };
}

test('locate keeps the live tail/checkpoint isolated and exposes both history directions', async () => {
  const f = await fixture({ page: opts => opts.before ? page([2, 3], 'b2', 'a3') : page([6, 7], 'b6', 'a7') });
  try {
    await f.controller.locateSource('message:message-5');
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id), ['turn-4', 'turn-5']);
    assert.deepEqual(f.controller.view().turns.map(t => t.turn_id), ['turn-9', 'turn-10']);
    assert.equal(f.controller.view().checkpoint, 'checkpoint-10');
    assert.equal(f.controller.browsingView().checkpoint, null);
    assert.equal(f.controller.displayView().checkpoint, null);
    await f.push({ type: 'batch', batch: batch({ mutations: [{ type: 'turn_summary_upsert', turn: turn('turn-11', 11) }] }) });
    assert.equal(f.controller.browseState().hasLiveUpdates, true);
    assert.equal(f.controller.displayView().turns.some(t => t.turn_id === 'turn-11'), false);
    assert.equal(f.controller.view().turns.some(t => t.turn_id === 'turn-11'), true);
    await f.controller.loadOlderHistory();
    await f.controller.loadNewerHistory();
    assert.equal(f.calls.at(-2).before, 'b4');
    assert.equal(f.calls.at(-1).after, 'a5');
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id), ['turn-2', 'turn-3', 'turn-4', 'turn-5', 'turn-6', 'turn-7']);
    f.controller.returnToLatest();
    assert.equal(f.controller.browseState(), null);
    assert.equal(f.controller.view().checkpoint, 'checkpoint-11');
    assert.equal(f.controller.displayView().turns.some(t => t.turn_id === 'turn-11'), true);
  } finally { f.controller.dispose(); }
});

test('location UI adapters distinguish loading, history window, and normal tail', async () => {
  const response = deferred();
  const f = await fixture({ locate: () => response.promise });
  try {
    assert.deepEqual(f.controller.locateState(), { status: 'idle' });
    assert.equal(f.controller.browsingView(), null);
    assert.deepEqual(f.controller.displayView(), f.controller.view());
    const request = f.controller.locateSource('message:message-5');
    assert.equal(f.controller.locateState().status, 'loading');
    assert.equal(f.controller.browsingView(), null);
    assert.equal(f.controller.view().checkpoint, 'checkpoint-10');
    response.resolve(located());
    await request;
    assert.equal(f.controller.locateState().status, 'ready');
    assert.equal(f.controller.locateState().target.turn_id, 'turn-5');
    assert.deepEqual(f.controller.browsingView().turns, f.controller.displayView().turns);
    assert.equal(f.controller.browsingView().checkpoint, null);
    assert.equal(f.controller.view().checkpoint, 'checkpoint-10');
    f.controller.returnToLatest();
    assert.deepEqual(f.controller.locateState(), { status: 'idle' });
    assert.equal(f.controller.browsingView(), null);
  } finally { f.controller.dispose(); }
});

test('controller subscriptions expose browse transitions and display membership', async () => {
  const f = await fixture({ page: opts => opts.before ? page([2, 3], 'b2', 'a3') : page([6, 7], 'b6', 'a7') });
  const changes = [];
  const unsubscribe = f.controller.subscribe(() => {
    const browse = f.controller.browseState();
    changes.push({
      status: browse?.status ?? null,
      older: browse?.older.kind ?? null,
      newer: browse?.newer.kind ?? null,
      hasLiveUpdates: browse?.hasLiveUpdates ?? false,
      ids: f.controller.displayView().turns.map(t => t.turn_id),
    });
  });
  try {
    await f.controller.locateSource('message:message-5');
    assert.equal(changes[0].status, 'resolving');
    assert.ok(changes.some(change => change.status === 'ready' && change.ids.join() === 'turn-4,turn-5'));
    await f.controller.loadOlderHistory();
    assert.ok(changes.some(change => change.older === 'loading'));
    assert.deepEqual(changes.at(-1).ids, ['turn-2', 'turn-3', 'turn-4', 'turn-5']);
    await f.controller.loadNewerHistory();
    assert.ok(changes.some(change => change.newer === 'loading'));
    assert.equal(changes.at(-1).ids.at(-1), 'turn-7');
    await f.push({ type: 'batch', batch: batch({ mutations: [{ type: 'turn_summary_upsert', turn: turn('turn-11', 11) }] }) });
    assert.equal(changes.at(-1).hasLiveUpdates, true);
    f.controller.returnToLatest();
    assert.equal(changes.at(-1).status, null);
    assert.equal(changes.at(-1).ids.at(-1), 'turn-11');
    unsubscribe();
    const count = changes.length;
    f.controller.returnToLatest();
    assert.equal(changes.length, count);
  } finally { unsubscribe(); f.controller.dispose(); }
});

test('newer history connects live only after fixed membership is continuously covered', async () => {
  const f = await fixture({ page: () => page([6, 7, 8, 9, 10], 'b6', null, { snapshot_through_seq: 11, event_head_seq: 11 }) });
  try {
    await f.controller.locateSource('message:message-5');
    await f.push({ type: 'batch', batch: batch({ mutations: [{ type: 'turn_summary_upsert', turn: turn('turn-11', 11) }] }) });
    assert.equal(f.controller.browseState().liveConnected, false);
    await f.controller.loadNewerHistory();
    assert.equal(f.controller.browseState().liveConnected, true);
    assert.equal(f.controller.view().checkpoint, 'checkpoint-11');
    assert.equal(f.controller.view().through_seq, 11);
    assert.equal(f.controller.displayView().turns.at(-1).turn_id, 'turn-11');
    await f.push({ type: 'batch', batch: batch({ from: 11, through: 12, mutations: [{ type: 'turn_summary_upsert', turn: turn('turn-12', 12) }] }) });
    assert.equal(f.controller.displayView().turns.at(-1).turn_id, 'turn-12');
    assert.equal(f.controller.view().checkpoint, 'checkpoint-12');
    assert.equal(f.controller.browsingView().checkpoint, null);
    assert.equal(f.controller.displayView().checkpoint, null);
    assert.equal(f.controller.displayView().through_seq, null);
    assert.equal(f.controller.browseState().status, 'ready');
  } finally { f.controller.dispose(); }
});

test('live-connected browsing retains reloadable page boundaries when paging away and reconnecting', async () => {
  const f = await fixture({
    limits: { max_turns: 8 },
    page: opts => opts.before
      ? page([2, 3], 'b2', 'a3')
      : page([6, 7, 8, 9, 10], 'b6', null, { snapshot_through_seq: 11, event_head_seq: 11 }),
  });
  try {
    await f.controller.locateSource('message:message-5');
    await f.push({ type: 'batch', batch: batch({ mutations: [{ type: 'turn_summary_upsert', turn: turn('turn-11', 11) }] }) });
    await f.controller.loadNewerHistory();
    assert.equal(f.controller.browseState().liveConnected, true);
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id),
      ['turn-4', 'turn-5', 'turn-6', 'turn-7', 'turn-8', 'turn-9', 'turn-10', 'turn-11']);
    const tail = f.controller.view();
    await f.controller.loadOlderHistory();
    assert.equal(f.calls.at(-1).before, 'b4');
    assert.equal(f.controller.browseState().liveConnected, false);
    assert.equal(f.controller.browseState().hasNewer, true);
    assert.equal(f.controller.displayView().next_before_cursor, 'b2');
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id), ['turn-2', 'turn-3', 'turn-4', 'turn-5']);
    assert.deepEqual(f.controller.view(), tail);
    await f.controller.loadNewerHistory();
    assert.equal(f.calls.at(-1).after, 'a5');
    assert.equal(f.controller.browseState().liveConnected, true);
    assert.equal(f.controller.browseState().hasOlder, true);
    assert.equal(f.controller.browseState().hasNewer, false);
    assert.equal(f.controller.historyState().kind, 'idle');
    assert.equal(f.controller.displayView().next_before_cursor, 'b4');
    assert.equal(f.controller.displayView().has_more, true);
    assert.equal(f.controller.displayView().checkpoint, null);
    assert.equal(f.controller.displayView().through_seq, null);
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id),
      ['turn-4', 'turn-5', 'turn-6', 'turn-7', 'turn-8', 'turn-9', 'turn-10', 'turn-11']);
    assert.deepEqual(f.controller.view(), tail);
  } finally { f.controller.dispose(); }
});

test('bounded browsing evicts whole far-end pages while keeping reload cursors', async () => {
  const f = await fixture({ limits: { max_turns: 4 }, page: opts => opts.before === 'b4' ? page([2, 3], 'b2', 'a3') : page([0, 1], null, 'a1') });
  try {
    await f.controller.locateSource('message:message-5');
    await f.controller.loadOlderHistory();
    await f.controller.loadOlderHistory();
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id), ['turn-0', 'turn-1', 'turn-2', 'turn-3']);
    assert.equal(f.controller.browseState().hasNewer, true);
    assert.equal(f.controller.displayView().turns.length, 4);
  } finally { f.controller.dispose(); }
});

test('superseded locate and return-to-latest fence late responses even when fetch ignores abort', async () => {
  const first = deferred();
  const f = await fixture({ locate: ref => ref === 'first' ? first.promise : located(page([7, 8], 'b7', 'a8'), { kind: 'input', message_id: 'message-8', turn_id: 'turn-8' }) });
  try {
    const old = f.controller.locateSource('first');
    await f.controller.locateSource('second');
    first.resolve(located());
    assert.equal(await old, null);
    assert.equal(f.controller.browseState().sourceRef, 'second');
    assert.equal(f.controller.displayView().turns[0].turn_id, 'turn-7');
    f.controller.returnToLatest();
    assert.equal(f.controller.displayView().turns[0].turn_id, 'turn-9');
  } finally { f.controller.dispose(); }
});

test('epoch/scope changes and stream reset cannot publish an old locate result', async () => {
  const pending = deferred();
  const f = await fixture({ locate: () => pending.promise });
  try {
    const locating = f.controller.locateSource('old');
    await f.push({ type: 'reset_required', reset: { type: 'reset_required', reason: 'event_log_epoch_mismatch' } });
    pending.resolve(located());
    assert.equal(await locating, null);
    assert.equal(f.controller.browseState().status, 'error');
    assert.equal(f.controller.view().checkpoint, null);
  } finally { f.controller.dispose(); }
});

test('newer page failures remain directionally retryable and do not change membership', async () => {
  let fail = true;
  const f = await fixture({ page: () => { if (fail) throw new TypeError('offline'); return page([6], 'b6', 'a6'); } });
  try {
    await f.controller.locateSource('source');
    assert.equal((await f.controller.loadNewerHistory()).kind, 'error');
    assert.equal(f.controller.browseState().newer.retryable, true);
    assert.equal(f.controller.displayView().turns.at(-1).turn_id, 'turn-5');
    fail = false;
    await f.controller.loadNewerHistory();
    assert.equal(f.controller.displayView().turns.at(-1).turn_id, 'turn-6');
  } finally { f.controller.dispose(); }
});

for (const bound of [undefined, null]) {
  const missingBound = window => {
    if (bound === undefined) delete window.coverage.membership_upper_bound;
    else window.coverage.membership_upper_bound = bound;
    return window;
  };
  test(`initial locate rejects a nonempty window with ${bound} membership bound`, async () => {
    const f = await fixture({ locate: () => located(missingBound(page([4, 5], 'b4', 'a5'))) });
    try {
      const tail = f.controller.view();
      const state = await f.controller.locateSource('source');
      assert.equal(state.status, 'error');
      assert.ok(state.error instanceof ConversationProtocolError);
      assert.match(state.error.message, /membership upper bound/);
      assert.equal(f.controller.browsingView(), null);
      assert.deepEqual(f.controller.view(), tail);
    } finally { f.controller.dispose(); }
  });

  for (const direction of ['older', 'newer']) {
    test(`${direction} paging rejects a nonempty window with ${bound} membership bound`, async () => {
      const f = await fixture({
        page: () => missingBound(direction === 'older' ? page([2, 3], 'b2', 'a3') : page([6, 7], 'b6', 'a7')),
      });
      try {
        await f.controller.locateSource('source');
        const view = f.controller.displayView();
        const result = await (direction === 'older' ? f.controller.loadOlderHistory() : f.controller.loadNewerHistory());
        assert.equal(result.kind, 'error');
        assert.ok(result.error instanceof ConversationProtocolError);
        assert.match(result.error.message, /membership upper bound/);
        assert.equal(f.controller.browseState().status, 'ready');
        assert.deepEqual(f.controller.displayView(), view);
      } finally { f.controller.dispose(); }
    });
  }
}

test('decoder normalizes canonical wire targets and rejects targets outside the window', () => {
  const wire = located(page([5]), { kind: 'input_message', message_id: 'm5', turn_id: 'turn-5', brief_id: null, activity_id: null });
  assert.equal(decodeConversationLocateResponse(wire).target.kind, 'input');
  assert.throws(() => decodeConversationLocateResponse({ ...wire, target: { ...wire.target, turn_id: 'other' } }), ConversationDecodeError);
  assert.throws(() => decodeConversationLocateResponse({ ...wire, runtime_id: 'other' }), ConversationDecodeError);
});

test('server status/window DTO supports located brief, pending input and source-only', () => {
  const boundary = { schema_version: 1, query_version: 1, agent_id: 'web' };
  const briefWindow = page([5], null, null, { turns: [turn('turn-5', 5, 1, { brief_ids: ['brief-5'] })] });
  const brief = decodeConversationLocateResponse({
    ...boundary, ...located(briefWindow, { kind: 'brief', brief_id: 'brief-5', turn_id: 'turn-5' }),
  });
  assert.equal(brief.status, 'located');
  assert.equal(brief.target.brief_id, 'brief-5');
  const pending = { message_id: 'pending-1', revision: 1, state: 'queued', preview: 'waiting' };
  const queuedWire = {
    ...boundary,
    ...located(page([], null, null, { pending_inputs: [pending] }), {
      kind: 'input_message', message_id: pending.message_id, turn_id: null, activity_id: null,
    }),
    status: 'pending',
  };
  const queued = decodeConversationLocateResponse(queuedWire);
  assert.equal(queued.status, 'pending');
  assert.deepEqual(queued.target, { kind: 'pending_input', message_id: pending.message_id });
  const sourceOnly = decodeConversationLocateResponse({
    ...queuedWire, status: 'source_only', reason: 'unattributed_input', window: null,
  });
  assert.equal(sourceOnly.status, 'source_only');
  assert.equal(sourceOnly.reason, 'unattributed_input');
  assert.equal(sourceOnly.window, null);
});

test('client encodes source identity, gates locate capability and rejects mixed directions', async () => {
  const calls = [];
  const client = new ConversationClient({ baseUrl: 'http://fake.local/api', fetch: async (url, init) => {
    calls.push({ url, init });
    if (url.endsWith('/handshake')) return Response.json({ ok: true, protocol: { name: 'holon-control', version: 1 }, auth: { mode: 'none', required: false }, capabilities: ['agents.conversation-read.v1', 'agents.conversation-locate.v1', CONVERSATION_HISTORY_BIDIRECTIONAL_CAPABILITY], runtime: {} });
    if (init.method === 'POST') return Response.json(located(page([5]), { kind: 'input_message', message_id: 'm5', turn_id: 'turn-5' }));
    return Response.json(page([6], 'b6', 'a6'));
  } });
  await client.locateSource('a/b', 'message:with?#chars', { contextLimit: 15 });
  assert.match(calls.at(-1).url, /agents\/a%2Fb\/conversation\/locate$/);
  assert.deepEqual(JSON.parse(calls.at(-1).init.body), { source_ref: 'message:with?#chars', limit: 15 });
  await client.summary('web', { after: 'opaque+/=' });
  assert.equal(new URL(calls.at(-1).url).searchParams.get('after'), 'opaque+/=');
  await assert.rejects(client.summary('web', { before: 'x', after: 'y' }));
  const old = new ConversationClient({ baseUrl: 'http://fake.local', fetch: async () => Response.json({ ok: true, protocol: { name: 'holon-control', version: 1 }, auth: { mode: 'none', required: false }, capabilities: ['agents.conversation-read.v1'], runtime: {} }) });
  await assert.rejects(old.locateSource('web', 'message:x'), ConversationCapabilityError);
});

test('after history gates version-skewed servers without changing before/latest requests', async () => {
  const calls = [];
  const capabilities = ['agents.conversation-read.v1', 'agents.conversation-locate.v1'];
  const client = new ConversationClient({ baseUrl: 'http://fake.local/api', fetch: async (url, init) => {
    calls.push({ url, init });
    if (url.endsWith('/handshake')) return Response.json({
      ok: true, protocol: { name: 'holon-control', version: 1 },
      auth: { mode: 'none', required: false }, capabilities, runtime: {},
    });
    if (init.method === 'POST') return Response.json(located(page([5]), { kind: 'input_message', message_id: 'm5', turn_id: 'turn-5' }));
    return Response.json(page([6], 'b6', 'a6'));
  } });
  await client.locateSource('web', 'message:m5');
  calls.length = 0;
  await client.summary('web');
  await client.summary('web', { before: 'b5' });
  assert.equal(calls.length, 2);
  assert.ok(calls.every(call => !call.url.endsWith('/handshake')));
  assert.equal(new URL(calls.at(-1).url).searchParams.get('before'), 'b5');
  calls.length = 0;
  await assert.rejects(client.summary('web', { after: 'a5' }), error =>
    error instanceof ConversationCapabilityError && error.capability === CONVERSATION_HISTORY_BIDIRECTIONAL_CAPABILITY);
  assert.equal(calls.length, 1);
  assert.ok(calls[0].url.endsWith('/handshake'));

  capabilities.push(CONVERSATION_HISTORY_BIDIRECTIONAL_CAPABILITY);
  calls.length = 0;
  const abort = new AbortController();
  const result = await client.summary('web', { after: 'opaque+/=', signal: abort.signal });
  assert.equal(calls.length, 2);
  assert.ok(calls[0].url.endsWith('/handshake'));
  assert.equal(new URL(calls[1].url).searchParams.get('after'), 'opaque+/=');
  assert.deepEqual(result.summary.turns.map(turn => turn.turn_id), ['turn-6']);

  capabilities.splice(capabilities.indexOf('agents.conversation-read.v1'), 1);
  calls.length = 0;
  await assert.rejects(client.summary('web', { after: 'a5' }), error =>
    error instanceof ConversationCapabilityError && error.capability === 'agents.conversation-read.v1');
  assert.equal(calls.length, 1);
  assert.ok(calls[0].url.endsWith('/handshake'));
});

test('expired bridge coverage stays historical and reports re-locate instead of stitching distant tail', async () => {
  const f = await fixture({ page: () => page([6, 7, 8, 9, 10], 'b6', null, { snapshot_through_seq: 267, event_head_seq: 267 }) });
  try {
    await f.controller.locateSource('source');
    for (let seq = 10; seq < 267; seq++) {
      await f.push({ type: 'batch', batch: batch({ from: seq, through: seq + 1 }) });
    }
    const result = await f.controller.loadNewerHistory();
    assert.equal(result.kind, 'error');
    assert.equal(result.retryable, true);
    assert.equal(f.controller.browseState().status, 'error');
    assert.equal(f.controller.browseState().liveConnected, false);
    assert.equal(f.controller.view().checkpoint, 'checkpoint-267');
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id), ['turn-4', 'turn-5', 'turn-6', 'turn-7', 'turn-8', 'turn-9', 'turn-10']);
  } finally { f.controller.dispose(); }
});

test('locate response with a changed runtime/epoch/scope never replaces the visible membership', async () => {
  const f = await fixture({ locate: () => located(page([5], null, null, { visibility_scope_id: 'other-scope' })) });
  try {
    await f.controller.locateSource('source');
    assert.equal(f.controller.browseState().status, 'error');
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id), ['turn-9', 'turn-10']);
    assert.equal(f.controller.view().checkpoint, 'checkpoint-10');
  } finally { f.controller.dispose(); }
});

test('pending target survives a transient remove then re-resolves by its original message ID', async () => {
  let requests = 0;
  const pending = { message_id: 'pending-1', revision: 1, state: 'queued', preview: 'waiting' };
  const f = await fixture({ locate: () => {
    requests++;
    if (requests === 1) return { ...located(page([9, 10], 'b9', null, { pending_inputs: [pending], snapshot_through_seq: 11, event_head_seq: 11 }), { kind: 'pending_input', message_id: pending.message_id }), status: 'pending' };
    return located(page([11], 'b11', null, { snapshot_through_seq: 13, event_head_seq: 13, turns: [turn('turn-11', 11, 1, { inputs: [{ message_id: pending.message_id, preview: pending.preview }] })] }), { kind: 'input', message_id: pending.message_id, turn_id: 'turn-11' });
  } });
  try {
    // Make the main tail's pending membership match the server snapshot.
    await f.push({ type: 'batch', batch: batch({ mutations: [{ type: 'operator_upsert', input: pending }] }) });
    await f.controller.locateSource('message:pending-1');
    await f.push({ type: 'batch', batch: batch({ from: 11, through: 12, mutations: [{ type: 'operator_remove', message_id: pending.message_id, revision: 2 }] }) });
    assert.equal(f.controller.displayView().pending_inputs[0].message_id, pending.message_id);
    await f.push({ type: 'batch', batch: batch({ from: 12, through: 13, mutations: [{ type: 'turn_summary_upsert', turn: turn('turn-11', 11, 1, { inputs: [{ message_id: pending.message_id, preview: pending.preview }] }) }] }) });
    await tick();
    assert.equal(requests, 2);
    assert.equal(f.controller.browseState().sourceRef, 'message:pending-1');
    assert.equal(f.controller.browseState().target.kind, 'input');
    assert.equal(f.controller.browseState().target.message_id, pending.message_id);
  } finally { f.controller.dispose(); }
});

test('detail hydration belongs to the selected browse window and fences late navigation results', async () => {
  const late = deferred();
  const f = await fixture({
    locate: () => located(page([4, 5], 'b4', 'a5'), { kind: 'turn', turn_id: 'turn-5', turn_key: { turn_index: 5, turn_id: 'turn-5' } }),
    activities: id => id === 'turn-5' ? late.promise : detail({ turn: turn(id, 4) }),
  });
  try {
    await f.controller.locateSource('source');
    assert.equal((await f.controller.loadDetail('turn-4')).kind, 'idle');
    assert.equal(f.controller.displayView().details[0].turn_id, 'turn-4');
    assert.equal(f.controller.view().details.length, 0);
    const loading = f.controller.loadDetail('turn-5');
    f.controller.returnToLatest();
    late.resolve(detail({ turn: turn('turn-5', 5) }));
    assert.equal((await loading).kind, 'idle');
    assert.equal(f.controller.displayView().details.length, 0);
    assert.equal(f.controller.displayView().turns.some(t => t.turn_id === 'turn-5'), false);
  } finally { f.controller.dispose(); }
});

test('late historical page cannot mutate a newer navigation or the live tail', async () => {
  const late = deferred();
  const f = await fixture({ page: () => late.promise });
  try {
    await f.controller.locateSource('source');
    const loading = f.controller.loadOlderHistory();
    f.controller.returnToLatest();
    late.resolve(page([2, 3], 'b2', 'a3'));
    assert.equal((await loading).kind, 'idle');
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id), ['turn-9', 'turn-10']);
    assert.equal(f.controller.view().checkpoint, 'checkpoint-10');
  } finally { f.controller.dispose(); }
});

test('source-only is not positioned and preserves main tail authority for consumer gating', async () => {
  const f = await fixture({ locate: () => ({
    status: 'source_only', source_ref: 'task:task-1', reason: 'non_conversation_source',
    target: null, window: null, runtime_id: 'runtime-a', event_log_epoch: 'epoch-a', visibility_scope_id: 'scope-a',
  }) });
  try {
    await f.controller.locateSource('task:task-1');
    assert.equal(f.controller.browseState().status, 'source_only');
    assert.equal(f.controller.browseState().target, null);
    assert.equal(f.controller.view().checkpoint, 'checkpoint-10');
    f.controller.returnToLatest();
    assert.equal(f.controller.view().checkpoint, 'checkpoint-10');
  } finally { f.controller.dispose(); }
});

test('missing locate support degrades navigation without stopping live conversation', async () => {
  for (const options of [
    { noLocate: true },
    { locate: () => { throw new ConversationCapabilityError('agents.conversation-locate.v1'); } },
    { locate: () => located(summary()) },
  ]) {
    const f = await fixture(options);
    try {
      const tail = f.controller.view();
      assert.equal((await f.controller.locateSource('source')).status, 'error');
      assert.equal(f.controller.browsingView(), null);
      assert.equal(f.controller.status.kind, 'ready');
      assert.deepEqual(f.controller.displayView(), tail);
      await f.push({ type: 'batch', batch: batch({}) });
      assert.equal(f.controller.view().checkpoint, 'checkpoint-11');
      f.controller.returnToLatest();
      assert.equal(f.controller.locateState().status, 'idle');
    } finally { f.controller.dispose(); }
  }
});

test('old server capability prevents any locate HTTP request and still permits summary reads', async () => {
  const calls = [];
  const client = new ConversationClient({ baseUrl: 'http://fake.local', fetch: async (url) => {
    calls.push(url);
    if (url.endsWith('/handshake')) return Response.json({
      ok: true, protocol: { name: 'holon-control', version: 1 },
      auth: { mode: 'none', required: false }, capabilities: ['agents.conversation-read.v1'], runtime: {},
    });
    return Response.json(summary());
  } });
  await assert.rejects(client.locateSource('web', 'message:x'), ConversationCapabilityError);
  assert.equal(calls.length, 1);
  assert.equal(calls.some(url => url.endsWith('/locate')), false);
  assert.equal((await client.summary('web')).summary.turns[0].turn_id, 'turn-10');
});

test('opposite-direction pages may finish out of order and duplicate loads stay single-flight', async () => {
  const older = deferred(), newer = deferred();
  const f = await fixture({ page: opts => opts.before ? older.promise : newer.promise });
  try {
    await f.controller.locateSource('source');
    const tail = f.controller.view();
    const loadingOlder = f.controller.loadOlderHistory();
    const loadingNewer = f.controller.loadNewerHistory();
    assert.equal((await f.controller.loadOlderHistory()).kind, 'loading');
    assert.equal((await f.controller.loadNewerHistory()).kind, 'loading');
    assert.equal(f.calls.filter(opts => opts.before || opts.after).length, 2);
    newer.resolve(page([6, 7], 'b6', 'a7'));
    await loadingNewer;
    assert.equal(f.controller.browseState().older.kind, 'loading');
    older.resolve(page([2, 3], 'b2', 'a3'));
    await loadingOlder;
    assert.deepEqual(f.controller.displayView().turns.map(t => t.turn_id),
      ['turn-2', 'turn-3', 'turn-4', 'turn-5', 'turn-6', 'turn-7']);
    assert.equal(f.controller.displayView().next_before_cursor, 'b2');
    assert.equal(f.controller.browseState().hasNewer, true);
    assert.deepEqual(f.controller.view(), tail);
  } finally { f.controller.dispose(); }
});

test('late locate errors cannot overwrite newer navigation or return-to-latest', async () => {
  for (const returnToLatest of [false, true]) {
    const late = deferred();
    const f = await fixture({ locate: ref => ref === 'late' ? late.promise : located() });
    try {
      const loading = f.controller.locateSource('late');
      if (returnToLatest) f.controller.returnToLatest();
      else await f.controller.locateSource('current');
      late.reject(new TypeError('late network failure'));
      assert.equal(await loading, null);
      assert.equal(f.controller.locateState().status, returnToLatest ? 'idle' : 'ready');
      assert.equal(f.controller.browseState()?.sourceRef ?? null, returnToLatest ? null : 'current');
      assert.equal(f.controller.view().checkpoint, 'checkpoint-10');
    } finally { f.controller.dispose(); }
  }
});

test('late newer page failures cannot overwrite navigation or a reset', async () => {
  for (const reset of [false, true]) {
    const late = deferred();
    const f = await fixture({ page: () => late.promise });
    try {
      await f.controller.locateSource('source');
      const loading = f.controller.loadNewerHistory();
      if (reset) await f.push({ type: 'reset_required', reset: { type: 'reset_required', reason: 'event_log_epoch_mismatch' } });
      else await f.controller.locateSource('current');
      late.reject(new TypeError('late page failure'));
      assert.equal((await loading).kind, 'idle');
      assert.equal(f.controller.browseState().status, reset ? 'error' : 'ready');
      if (!reset) assert.equal(f.controller.browseState().newer.kind, 'idle');
    } finally { f.controller.dispose(); }
  }
});

test('coverage decoder retains canonical TurnKey boundaries and rejects malformed keys', () => {
  const coverage = {
    ...page([4, 5], 'b4', 'a5').coverage,
    first_turn_key: { turn_index: 4, turn_id: 'turn-4' },
    last_turn_key: { turn_index: 5, turn_id: 'turn-5' },
    membership_upper_bound: { turn_index: 10, turn_id: 'turn-10' },
  };
  const wire = located(page([4, 5], 'b4', 'a5', { coverage }));
  assert.deepEqual(decodeConversationLocateResponse(wire).window.coverage, coverage);
  assert.throws(() => decodeConversationLocateResponse({
    ...wire, window: { ...wire.window, coverage: { ...coverage, membership_upper_bound: { turn_index: -1, turn_id: 'bad' } } },
  }), ConversationDecodeError);
});

test('pages with changed fixed membership cannot join unrelated history', async () => {
  for (const bound of [{ turn_index: 11, turn_id: 'turn-11' }, null, undefined]) {
    const initial = page([4, 5], 'b4', 'a5');
    const next = page([6, 7], 'b6', 'a7');
    const { membership_upper_bound: _bound, ...nextCoverage } = next.coverage;
    const f = await fixture({
      locate: () => located({ ...initial, coverage: { ...initial.coverage, membership_upper_bound: { turn_index: 10, turn_id: 'turn-10' } } }),
      page: () => ({ ...next, coverage: { ...nextCoverage, ...(bound === undefined ? {} : { membership_upper_bound: bound }) } }),
    });
    try {
      await f.controller.locateSource('source');
      const displayed = f.controller.displayView().turns;
      const result = await f.controller.loadNewerHistory();
      assert.equal(result.kind, 'error');
      assert.equal(f.controller.browseState().status, bound == null ? 'ready' : 'error');
      assert.equal(f.controller.browseState().liveConnected, false);
      assert.deepEqual(f.controller.displayView().turns, displayed);
      assert.equal(f.controller.view().checkpoint, 'checkpoint-10');
    } finally { f.controller.dispose(); }
  }
});
