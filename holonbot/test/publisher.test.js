/**
 * Tests for the publisher module
 */

import { jest } from '@jest/globals';

// Mock fs/promises at the module level
const mockReadFile = jest.fn();
jest.unstable_mockModule('fs/promises', () => ({
    readFile: mockReadFile
}));

// Import after mocking
import {
    isHolonbotComment,
    formatReviewReply
} from '../lib/publisher.js';

// Note: hasValidHolonOutput and publishHolonResults are harder to test
// with jest.unstable_mockModule in the current setup, so we'll focus on
// testing the utility functions that don't require complex mocking.

describe('Publisher Module', () => {

    beforeEach(() => {
        jest.clearAllMocks();
    });

    describe('isHolonbotComment', () => {
        test('should return true for holonbot[bot]', () => {
            expect(isHolonbotComment({ user: { login: 'holonbot[bot]' } })).toBe(true);
        });

        test('should return false for regular users', () => {
            expect(isHolonbotComment({ user: { login: 'octocat' } })).toBe(false);
        });

        test('should return false for bots with different names', () => {
            expect(isHolonbotComment({ user: { login: 'otherbot[bot]' } })).toBe(false);
        });

        test('should handle missing user', () => {
            expect(isHolonbotComment({})).toBe(false);
        });

        test('should handle user without login or type', () => {
            expect(isHolonbotComment({ user: {} })).toBe(false);
        });
    });

    describe('formatReviewReply', () => {
        test('should format fixed status with emoji and message', () => {
            const reply = formatReviewReply({
                status: 'fixed',
                message: 'Good catch! Fixed the bug.',
                action_taken: 'Added null check'
            });

            expect(reply).toContain('✅');
            expect(reply).toContain('FIXED');
            expect(reply).toContain('Good catch! Fixed the bug.');
            expect(reply).toContain('**Action taken**: Added null check');
        });

        test('should format wontfix status', () => {
            const reply = formatReviewReply({
                status: 'wontfix',
                message: 'Not applicable',
                action_taken: null
            });

            expect(reply).toContain('⚠️');
            expect(reply).toContain('WONTFIX');
            expect(reply).toContain('Not applicable');
            expect(reply).not.toContain('Action taken');
        });

        test('should format need-info status', () => {
            const reply = formatReviewReply({
                status: 'need-info',
                message: 'Please clarify',
                action_taken: null
            });

            expect(reply).toContain('❓');
            expect(reply).toContain('NEED-INFO');
        });

        test('should handle unknown status with default emoji', () => {
            const reply = formatReviewReply({
                status: 'unknown',
                message: 'Some message',
                action_taken: null
            });

            expect(reply).toContain('📝');
            expect(reply).toContain('UNKNOWN');
        });

        test('should include action_taken when present', () => {
            const reply = formatReviewReply({
                status: 'fixed',
                message: 'Done',
                action_taken: 'Refactored function'
            });

            expect(reply).toContain('**Action taken**: Refactored function');
        });

        test('should not include action_taken when null', () => {
            const reply = formatReviewReply({
                status: 'fixed',
                message: 'Done',
                action_taken: null
            });

            expect(reply).not.toContain('Action taken');
        });
    });

    // Integration-style tests for publishHolonResults would require
    // more complex mocking setup. The functions are tested indirectly
    // through the CLI script and manual testing.
    describe('Publisher API exports', () => {
        test('should export isHolonbotComment', () => {
            expect(typeof isHolonbotComment).toBe('function');
        });

        test('should export formatReviewReply', () => {
            expect(typeof formatReviewReply).toBe('function');
        });
    });
});
