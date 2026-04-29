/**
 * Agent scenario: Wait for a CI build to complete using the callback capability.
 *
 * This demonstrates:
 * 1. Creating a callback capability with CreateCallback
 * 2. Handing that capability to an external system (CI simulator)
 * 3. Waiting for the callback to be delivered
 * 4. Receiving the result as a CallbackEvent message
 * 5. Cleaning up with CancelWaiting
 */

/**
 * Run the agent scenario.
 *
 * @param {Object} runtime - Holon runtime handle
 * @param {Object} ciSystem - CI simulator instance
 * @returns {Promise<Object>} Scenario result
 */
export async function runAgentScenario(runtime, ciSystem) {
  console.log('[Agent] Starting CI build wait scenario');

  // Step 1: Create a callback capability
  console.log('[Agent] Creating callback capability for CI build completion');
  const callback = await runtime.createCallback({
    summary: 'Wait for CI build to complete',
    source: 'ci-simulator',
    condition: 'build_status != "running"',
    delivery_mode: 'enqueue_message',
  });

  console.log('[Agent] Callback capability created:', {
    waiting_intent_id: callback.waiting_intent_id,
    callback_url: callback.callback_url,
  });

  // Step 2: Start a build and register the webhook
  const buildId = ciSystem.startBuild('holon/holon', 'abc123');
  ciSystem.registerWebhook({
    callbackUrl: callback.callback_url,
    buildId,
    expectedStatus: 'success',
  });

  console.log('[Agent] Build started and webhook registered');

  // Step 3: Wait for the callback to be delivered
  // In a real agent, this would be waiting for the next message
  // For the test harness, we'll poll until the message arrives
  console.log('[Agent] Waiting for build to complete...');

  // The test harness will deliver the callback
  // Here we just return the information needed to complete the flow
  return {
    waiting_intent_id: callback.waiting_intent_id,
    callback_descriptor_id: callback.callback_descriptor_id,
    buildId,
    callback_url: callback.callback_url,
  };
}

/**
 * Verify the callback was received correctly.
 *
 * @param {Object} message - The CallbackEvent message received
 * @param {string} expectedBuildId - Expected build ID
 * @returns {boolean} True if verification passes
 */
export function verifyCallback(message, expectedBuildId) {
  console.log('[Agent] Verifying callback message');

  // Check message origin
  if (message.origin?.type !== 'Callback') {
    console.error('[Agent] Message origin is not Callback');
    return false;
  }

  // Check metadata
  const payload = message.body.json;
  if (!payload) {
    console.error('[Agent] Message body is not JSON');
    return false;
  }

  if (payload.buildId !== expectedBuildId) {
    console.error('[Agent] Build ID mismatch:', {
      expected: expectedBuildId,
      received: payload.buildId,
    });
    return false;
  }

  if (payload.status !== 'success') {
    console.error('[Agent] Build status is not success:', payload.status);
    return false;
  }

  console.log('[Agent] Callback verified successfully');
  return true;
}

/**
 * Clean up the callback capability.
 *
 * @param {Object} runtime - Holon runtime handle
 * @param {string} waitingIntentId - Waiting intent ID to cancel
 */
export async function cleanupCallback(runtime, waitingIntentId) {
  console.log('[Agent] Cancelling waiting intent');
  const result = await runtime.cancelWaiting(waitingIntentId);
  console.log('[Agent] Waiting intent cancelled:', result);
  return result;
}
