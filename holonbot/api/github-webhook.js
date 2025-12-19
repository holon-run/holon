/**
 * Vercel Serverless Function for GitHub Webhooks
 *
 * This function handles GitHub webhook events and forwards them to the Probot app.
 * It's designed to work with Vercel's serverless function environment.
 */

import { createNodeMiddleware } from 'probot';
import botHandler from '../lib/bot-handler.js';
import { probot } from '../lib/probot-client.js';

// Export the middleware directly as the default handler for Vercel
// This is the recommended pattern for Probot v14 in serverless environments.
// The middleware will handle:
// 1. Signature verification (using WEBHOOK_SECRET)
// 2. Body parsing
// 3. Routing to botHandler
export default createNodeMiddleware(botHandler, {
  probot,
  webhooksPath: '/api/github-webhook'
});