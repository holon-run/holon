/**
 * Simulates an external CI system that can register webhooks and deliver build results.
 *
 * This represents the provider-specific side of the callback integration:
 * - It knows about CI builds and their states
 * - It can register webhook callbacks
 * - It calls back when builds complete
 *
 * In a real integration, this would be GitHub Actions, CircleCI, etc.
 */
export class CISimulator {
  #baseUrl;
  #pendingBuilds = new Map();
  #webhooks = [];
  #testDeliveryFn = null;

  constructor(baseUrl) {
    this.#baseUrl = baseUrl;
  }

  /**
   * Register a webhook URL to be notified when a build completes.
   *
   * @param {Object} options - Webhook registration options
   * @param {string} options.callbackUrl - The Holon callback URL
   * @param {string} options.buildId - The build to watch
   * @param {'success'|'failure'} options.expectedStatus - What status to simulate
   */
  registerWebhook({ callbackUrl, buildId, expectedStatus }) {
    this.#webhooks.push({ callbackUrl, buildId, expectedStatus });
    console.log(`[CI] Registered webhook for build ${buildId}`);
  }

  /**
   * Start a new build and return its ID.
   *
   * @param {string} repo - Repository identifier
   * @param {string} commit - Commit SHA
   * @returns {string} Build ID
   */
  startBuild(repo, commit) {
    const buildId = `build-${Date.now()}`;
    this.#pendingBuilds.set(buildId, { repo, commit, status: 'running' });
    console.log(`[CI] Started build ${buildId} for ${repo}@${commit}`);
    return buildId;
  }

  /**
   * Complete a build and trigger webhook notifications.
   *
   * @param {string} buildId - Build ID to complete
   * @param {'success'|'failure'} status - Final build status
   */
  async completeBuild(buildId, status = 'success') {
    const build = this.#pendingBuilds.get(buildId);
    if (!build) {
      throw new Error(`Build ${buildId} not found`);
    }

    build.status = status;
    console.log(`[CI] Build ${buildId} completed with status: ${status}`);

    // Find and trigger matching webhooks
    const matchingWebhooks = this.#webhooks.filter((w) => w.buildId === buildId);
    const deliveryPromises = matchingWebhooks.map((webhook) =>
      this.#deliverWebhook(webhook, {
        buildId,
        repo: build.repo,
        commit: build.commit,
        status,
        timestamp: new Date().toISOString(),
      }, this.#testDeliveryFn)
    );

    await Promise.all(deliveryPromises);
  }

  /**
   * Deliver a webhook payload to a callback URL.
   *
   * @param {Object} webhook - Webhook configuration
   * @param {Object} payload - Build result payload
   * @param {Function} deliverFn - Optional custom delivery function (for testing)
   */
  async #deliverWebhook(webhook, payload, deliverFn) {
    const { callbackUrl, expectedStatus } = webhook;

    console.log(`[CI] Delivering webhook to ${callbackUrl}`);

    try {
      // Extract token from callback URL: http://localhost:8080/callback/{token}
      const url = new URL(callbackUrl);
      const token = url.pathname.split('/').pop();

      const responseBody = {
        token,
        json: payload,
        metadata: {
          source: 'ci-simulator',
          buildId: payload.buildId,
        },
      };

      // Use custom delivery function if provided (for testing), otherwise make real HTTP request
      if (deliverFn) {
        await deliverFn(token, responseBody);
      } else {
        const response = await fetch(new URL(url.pathname, this.#baseUrl), {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
          },
          body: JSON.stringify(responseBody),
        });

        if (!response.ok) {
          console.error(`[CI] Webhook delivery failed: ${response.status}`);
        } else {
          console.log(`[CI] Webhook delivered successfully`);
        }
      }
    } catch (error) {
      console.error(`[CI] Webhook delivery error:`, error.message);
      throw error;
    }
  }

  /**
   * Get the current status of a build.
   *
   * @param {string} buildId - Build ID
   * @returns {Object} Build status
   */
  getBuildStatus(buildId) {
    return this.#pendingBuilds.get(buildId);
  }

  /**
   * Set a custom delivery function for testing.
   *
   * @param {Function} fn - Custom delivery function
   */
  setTestDeliveryFn(fn) {
    this.#testDeliveryFn = fn;
  }
}
