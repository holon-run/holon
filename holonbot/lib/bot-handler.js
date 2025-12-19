/**
 * Holon GitHub App Bot
 *
 * This bot handles various GitHub webhook events for the holon repository.
 * Currently, it serves as a passive logger and placeholder for future automation.
 */

export default async function botHandler(app) {
  // Log when the app is initialized
  app.log.info('Holon Bot is starting up!');

  // Listen to all relevant events
  app.onAny(async (context) => {
    const { name, payload } = context;
    const action = payload.action ? `.${payload.action}` : '';

    app.log.info(`Received event: ${name}${action}`);

    // Detailed logging for debugging if needed, can be removed in production
    // app.log.debug(payload);
  });

  // Error handling
  app.onError((error) => {
    app.log.error('Error occurred in the app:', error);
  });

  // Log when app is loaded
  app.log.info('Holon Bot is ready to receive events!');
}