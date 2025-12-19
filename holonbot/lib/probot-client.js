import { createProbot } from 'probot';

// Configure environment variables for Probot
const probotOptions = {
    appId: process.env.APP_ID,
    privateKey: process.env.PRIVATE_KEY?.replace(/\\n/g, '\n'),
    secret: process.env.WEBHOOK_SECRET,
    logLevel: process.env.LOG_LEVEL || 'info',
};

// Create a singleton Probot instance for reuse in serverless warm starts
export const probot = createProbot(probotOptions);
