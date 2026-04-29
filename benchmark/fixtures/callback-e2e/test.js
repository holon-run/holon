/**
 * Test harness for the callback capability end-to-end fixture.
 *
 * This test:
 * 1. Creates a callback capability via the runtime
 * 2. Registers it with an external CI system
 * 3. Triggers the external system to call back
 * 4. Verifies the agent receives the correct message
 * 5. Cleans up the callback
 */

import assert from 'node:assert/strict';
import { CISimulator } from './src/ci-simulator.js';
import { runAgentScenario, verifyCallback, cleanupCallback } from './src/agent-scenario.js';

// Mock runtime that simulates Holon's callback tools
class MockRuntime {
  #callbacks = new Map();
  #messages = [];

  constructor() {
    this.callbackBaseUrl = 'http://localhost:8080';
  }

  async createCallback({ summary, source, condition, delivery_mode }) {
    const waiting_intent_id = `wi-${Date.now()}`;
    const callback_descriptor_id = `cd-${Date.now()}`;
    const token = this.#generateToken();
    const callback_url = `${this.callbackBaseUrl}/callback/${token}`;

    this.#callbacks.set(waiting_intent_id, {
      waiting_intent_id,
      callback_descriptor_id,
      callback_url,
      summary,
      source,
      condition,
      delivery_mode,
    });

    return {
      waiting_intent_id,
      callback_descriptor_id,
      callback_url,
      target_agent_id: 'agent-test',
      delivery_mode,
    };
  }

  async cancelWaiting(waiting_intent_id) {
    const callback = this.#callbacks.get(waiting_intent_id);
    if (!callback) {
      throw new Error(`Waiting intent ${waiting_intent_id} not found`);
    }

    this.#callbacks.delete(waiting_intent_id);

    return {
      waiting_intent_id,
      callback_descriptor_id: callback.callback_descriptor_id,
      status: 'Cancelled',
    };
  }

  #generateToken() {
    return `token-${Math.random().toString(36).substring(2, 15)}`;
  }

  // Simulate receiving a callback message
  async receiveCallback(payload) {
    this.#messages.push({
      id: `msg-${Date.now()}`,
      origin: {
        type: 'Callback',
        descriptor_id: payload.descriptor_id,
        source: payload.source,
      },
      body: {
        json: payload.json,
      },
      metadata: {
        waiting_intent_id: payload.waiting_intent_id,
        callback_descriptor_id: payload.descriptor_id,
        source: payload.source,
        resource: payload.resource,
        metadata: payload.metadata,
      },
    });
  }

  getMessages() {
    return this.#messages;
  }

  getLastMessage() {
    return this.#messages[this.#messages.length - 1];
  }

  // Helper for test to access the callback
  _getCallback(waitingIntentId) {
    return this.#callbacks.get(waitingIntentId);
  }

  // Helper for test to check remaining callbacks
  _getRemainingCallbacksCount() {
    return this.#callbacks.size;
  }
}

async function runEnqueueMessageTest(runtime, ciSystem) {
  console.log('\n--- Test 1: enqueue_message mode ---\n');

  // Step 1: Run the agent scenario
  console.log('Step 1: Agent creates callback and starts build');
  const scenario = await runAgentScenario(runtime, ciSystem);
  console.log('');

  // Set up test delivery function to bypass real HTTP
  ciSystem.setTestDeliveryFn(async (token, body) => {
    const callback = runtime._getCallback(scenario.waiting_intent_id);
    await runtime.receiveCallback({
      waiting_intent_id: scenario.waiting_intent_id,
      descriptor_id: callback.callback_descriptor_id,
      source: 'ci-simulator',
      json: body.json,
      metadata: body.metadata,
    });
  });

  // Step 2: Complete the build (triggers webhook)
  console.log('Step 2: CI system completes build and delivers webhook');
  await ciSystem.completeBuild(scenario.buildId, 'success');
  console.log('');

  // Step 3: Verify the callback was received
  console.log('Step 3: Verify agent received correct message');
  const message = runtime.getLastMessage();
  assert(message, 'Agent should have received a message');
  const verified = verifyCallback(message, scenario.buildId);
  assert(verified, 'Callback verification should pass');
  console.log('');

  // Step 4: Clean up
  console.log('Step 4: Clean up callback capability');
  const cancelResult = await cleanupCallback(runtime, scenario.waiting_intent_id);
  assert.equal(cancelResult.status, 'Cancelled');
  console.log('');

  // Verify cleanup
  const remainingCallbacks = runtime._getRemainingCallbacksCount();
  assert.equal(remainingCallbacks, 0, 'All callbacks should be cancelled');
}

async function runWakeOnlyTest(runtime, ciSystem) {
  console.log('\n--- Test 2: wake_only mode ---\n');

  // Create a wake_only callback
  console.log('Step 1: Agent creates wake_only callback');
  const wakeCallback = await runtime.createCallback({
    summary: 'Wake-only notification for build',
    source: 'ci-simulator',
    condition: 'build_status != "running"',
    delivery_mode: 'wake_only',
  });

  console.log('[Agent] Wake-only callback created:', {
    waiting_intent_id: wakeCallback.waiting_intent_id,
    callback_url: wakeCallback.callback_url,
  });

  // Start a build with the wake_only webhook
  const buildId = ciSystem.startBuild('holon/holon', 'def456');
  ciSystem.registerWebhook({
    callbackUrl: wakeCallback.callback_url,
    buildId,
    expectedStatus: 'success',
  });

  console.log('[Agent] Build started with wake_only webhook');

  // Set up test delivery function for wake_only
  ciSystem.setTestDeliveryFn(async (token, body) => {
    const storedCallback = runtime._getCallback(wakeCallback.waiting_intent_id);
    // For wake_only, no payload is delivered - just the wakeup signal
    await runtime.receiveCallback({
      waiting_intent_id: wakeCallback.waiting_intent_id,
      descriptor_id: storedCallback.callback_descriptor_id,
      source: 'ci-simulator',
      json: null, // wake_only delivers no payload
      metadata: {
        delivered_at: new Date().toISOString(),
      },
    });
  });

  // Complete the build
  console.log('Step 2: CI system completes build');
  await ciSystem.completeBuild(buildId, 'success');
  console.log('[Agent] Wake-only callback received');

  // Verify we got a message (even though it has no payload)
  const message = runtime.getLastMessage();
  assert(message, 'Agent should receive wakeup signal');
  assert.equal(message.origin.type, 'Callback');
  assert.equal(message.body.json, null, 'wake_only should deliver no payload');
  console.log('[Agent] Wake-only signal verified');

  // Clean up
  await cleanupCallback(runtime, wakeCallback.waiting_intent_id);
  console.log('');
}

async function runCancelThenCallbackTest(runtime, ciSystem) {
  console.log('\n--- Test 3: callback after CancelWaiting ---\n');

  // Create a callback
  console.log('Step 1: Agent creates callback');
  const callback = await runtime.createCallback({
    summary: 'Test callback for cancellation',
    source: 'ci-simulator',
    condition: 'build_status != "running"',
    delivery_mode: 'enqueue_message',
  });

  console.log('[Agent] Callback created:', {
    waiting_intent_id: callback.waiting_intent_id,
    callback_url: callback.callback_url,
  });

  // Start a build
  const buildId = ciSystem.startBuild('holon/holon', 'ghi789');
  ciSystem.registerWebhook({
    callbackUrl: callback.callback_url,
    buildId,
    expectedStatus: 'success',
  });

  console.log('[Agent] Build started');

  // Cancel the waiting intent
  console.log('Step 2: Agent cancels waiting intent');
  const cancelResult = await runtime.cancelWaiting(callback.waiting_intent_id);
  assert.equal(cancelResult.status, 'Cancelled');
  console.log('[Agent] Waiting intent cancelled:', cancelResult.waiting_intent_id);

  // Set up test delivery function that should be rejected
  let callbackAttempted = false;
  let callbackRejected = false;
  ciSystem.setTestDeliveryFn(async (token, body) => {
    callbackAttempted = true;
    try {
      // Try to deliver callback after cancellation
      const storedCallback = runtime._getCallback(callback.waiting_intent_id);
      if (!storedCallback) {
        callbackRejected = true;
        console.log('[Agent] Callback rejected: waiting intent not found');
        return;
      }
      await runtime.receiveCallback({
        waiting_intent_id: callback.waiting_intent_id,
        descriptor_id: storedCallback.callback_descriptor_id,
        source: 'ci-simulator',
        json: body.json,
        metadata: body.metadata,
      });
    } catch (err) {
      callbackRejected = true;
      console.log('[Agent] Callback rejected with error:', err.message);
    }
  });

  // Complete the build - this should trigger a callback attempt
  console.log('Step 3: CI system completes build (callback should be ignored)');
  await ciSystem.completeBuild(buildId, 'success');

  // Verify the callback was attempted but rejected
  assert(callbackAttempted, 'CI system should have attempted callback delivery');
  assert(callbackRejected, 'Callback delivery should be rejected after cancellation');
  console.log('[Agent] Callback correctly rejected after cancellation');

  // Verify no new messages were delivered after cancellation
  const messageCount = runtime.getMessages().length;
  assert.equal(messageCount, 0, 'No messages should be delivered after cancellation');
  console.log('[Agent] Verified: no messages delivered after cancellation');
  console.log('');
}

async function main() {
  console.log('=== Callback Capability End-to-End Test ===');

  // Test 1: enqueue_message mode
  const runtime1 = new MockRuntime();
  const ciSystem1 = new CISimulator('http://localhost:8080');
  await runEnqueueMessageTest(runtime1, ciSystem1);

  // Test 2: wake_only mode
  const runtime2 = new MockRuntime();
  const ciSystem2 = new CISimulator('http://localhost:8080');
  await runWakeOnlyTest(runtime2, ciSystem2);

  // Test 3: callback after CancelWaiting
  const runtime3 = new MockRuntime();
  const ciSystem3 = new CISimulator('http://localhost:8080');
  await runCancelThenCallbackTest(runtime3, ciSystem3);

  console.log('=== All tests passed ===');
}

main().catch((err) => {
  console.error('Test failed:', err);
  process.exit(1);
});
