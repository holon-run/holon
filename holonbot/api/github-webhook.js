/**
 * Vercel Serverless Function for GitHub Webhooks
 *
 * This function handles GitHub webhook events and forwards them to the Probot app.
 * It's designed to work with Vercel's serverless function environment.
 */

import { createNodeMiddleware, createProbot } from 'probot';
import app from '../lib/bot-handler.js';

// Configure environment variables for Probot
const probotOptions = {
  appId: process.env.APP_ID,
  privateKey: process.env.PRIVATE_KEY,
  webhookSecret: process.env.WEBHOOK_SECRET,
  logLevel: process.env.LOG_LEVEL || 'info',
};

// Create Probot instance
const probot = createProbot(probotOptions);

// Create middleware to handle webhooks
const middleware = createNodeMiddleware(app, { probot });

// Export as default handler for Vercel
export default async function handler(req, res) {
  try {
    // Log incoming request for debugging
    console.log(`${req.method} ${req.url} - ${req.headers['x-github-event'] || 'No event'}`);

    // Only accept POST requests for webhook events
    if (req.method !== 'POST') {
      res.setHeader('Allow', 'POST');
      return res.status(405).json({ error: 'Method not allowed' });
    }

    // Basic validation of GitHub webhook headers
    const githubEvent = req.headers['x-github-event'];
    const githubDelivery = req.headers['x-github-delivery'];
    const githubSignature = req.headers['x-hub-signature-256'];

    if (!githubEvent || !githubDelivery || !githubSignature) {
      console.error('Missing required GitHub webhook headers');
      return res.status(400).json({
        error: 'Missing required GitHub webhook headers',
        missing: {
          event: !githubEvent,
          delivery: !githubDelivery,
          signature: !githubSignature
        }
      });
    }

    // Ensure environment variables are set
    if (!process.env.APP_ID || !process.env.PRIVATE_KEY || !process.env.WEBHOOK_SECRET) {
      console.error('Missing required environment variables');
      return res.status(500).json({
        error: 'Server configuration error',
        message: 'Missing required environment variables (APP_ID, PRIVATE_KEY, WEBHOOK_SECRET)'
      });
    }

    // Forward request to Probot middleware
    return await middleware(req, res);

  } catch (error) {
    console.error('Error handling webhook:', error);
    const isProduction = process.env.NODE_ENV === 'production';
    return res.status(500).json({
      error: 'Internal server error',
      message: isProduction ? 'An unexpected error occurred.' : error.message
    });
  }
}