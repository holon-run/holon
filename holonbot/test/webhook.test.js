import { jest, describe, test, expect } from '@jest/globals';

// Mock dependencies
jest.unstable_mockModule('probot', () => ({
    createNodeMiddleware: jest.fn(() => (req, res) => {
        res.status(200).send('OK');
    }),
    createProbot: jest.fn(() => ({}))
}));

jest.unstable_mockModule('../lib/probot-client.js', () => ({
    probot: {}
}));

// Import after mocking
const { default: handler } = await import('../api/github-webhook.js');

describe('github-webhook handler', () => {
    test('should be a function', () => {
        expect(typeof handler).toBe('function');
    });

    test('should respond OK when called', async () => {
        const req = {};
        const res = {
            status: jest.fn().mockReturnThis(),
            send: jest.fn().mockReturnThis()
        };

        await handler(req, res);
        expect(res.status).toHaveBeenCalledWith(200);
        expect(res.send).toHaveBeenCalledWith('OK');
    });
});
