import botHandler from '../lib/bot-handler.js';

describe('Bot Handler', () => {
    test('should export a function', () => {
        expect(typeof botHandler).toBe('function');
    });
});
