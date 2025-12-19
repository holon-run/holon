import { jest, describe, test, expect } from '@jest/globals';

// Use unstable_mockModule for ESM mocking
// This must be done BEFORE importing the modules that use 'jose'
jest.unstable_mockModule('jose', () => ({
    jwtVerify: jest.fn(),
    createRemoteJWKSet: jest.fn(() => () => Promise.resolve({})),
}));

// Dynamically import modules AFTER mocking
const { validateClaims, verifyOIDCToken } = await import('../lib/oidc.js');
const jose = await import('jose');


describe('OIDC Validation', () => {
    test('should validate correct claims', () => {
        const claims = {
            repository: 'jolestar/holon',
            repository_owner: 'jolestar',
            actor: 'jolestar',
            ref: 'refs/heads/main'
        };

        const result = validateClaims(claims);
        expect(result).toEqual({
            repository: 'jolestar/holon',
            owner: 'jolestar',
            actor: 'jolestar',
            ref: 'refs/heads/main'
        });
    });

    test('should throw error if repository is missing', () => {
        const claims = {
            actor: 'jolestar'
        };
        expect(() => validateClaims(claims)).toThrow('Missing repository information');
    });

    test('should throw error if owner is missing', () => {
        const claims = {
            repository: 'jolestar/holon'
        };
        expect(() => validateClaims(claims)).toThrow('Missing repository information');
    });
});

describe('verifyOIDCToken', () => {
    const GITHUB_ISSUER = 'https://token.actions.githubusercontent.com';

    test('should verify a valid token', async () => {
        const mockPayload = { repository: 'owner/repo', repository_owner: 'owner' };
        jose.jwtVerify.mockResolvedValueOnce({ payload: mockPayload });

        const token = 'valid.token.here';
        const result = await verifyOIDCToken(token);

        expect(jose.jwtVerify).toHaveBeenCalledWith(
            token,
            expect.any(Function),
            expect.objectContaining({
                issuer: GITHUB_ISSUER
            })
        );
        expect(result).toEqual(mockPayload);
    });

    test('should throw error if verification fails', async () => {
        jose.jwtVerify.mockRejectedValueOnce(new Error('Invalid signature'));

        await expect(verifyOIDCToken('invalid-token')).rejects.toThrow('Invalid OIDC Token: Invalid signature');
    });
});
