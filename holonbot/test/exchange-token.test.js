import { jest, describe, test, expect, beforeEach } from '@jest/globals';

// Mock dependencies
jest.unstable_mockModule('../lib/probot-client.js', () => ({
    probot: {
        auth: jest.fn()
    }
}));

jest.unstable_mockModule('../lib/oidc.js', () => ({
    verifyOIDCToken: jest.fn(),
    validateClaims: jest.fn()
}));

// Import after mocking
const { default: handler } = await import('../api/exchange-token.js');
const { probot } = await import('../lib/probot-client.js');
const { verifyOIDCToken, validateClaims } = await import('../lib/oidc.js');

describe('exchange-token handler', () => {
    let req, res;

    beforeEach(() => {
        req = {
            method: 'POST',
            headers: {
                authorization: 'Bearer valid-oidc-token'
            }
        };
        res = {
            status: jest.fn().mockReturnThis(),
            json: jest.fn().mockReturnThis(),
            setHeader: jest.fn()
        };
        jest.clearAllMocks();
    });

    test('should exchange token successfully', async () => {
        // 1. Mock OIDC verification
        verifyOIDCToken.mockResolvedValue({ sub: 'repo:owner/repo' });
        validateClaims.mockReturnValue({ repository: 'owner/repo', owner: 'owner' });

        // 2. Mock Probot and Octokit
        const mockOctokit = {
            rest: {
                apps: {
                    getRepoInstallation: jest.fn().mockResolvedValue({
                        data: { id: 123 }
                    }),
                    createInstallationAccessToken: jest.fn().mockResolvedValue({
                        data: { token: 'gh-installation-token', expires_at: '2025-01-01T00:00:00Z', permissions: {} }
                    })
                }
            }
        };
        probot.auth.mockResolvedValue(mockOctokit);

        await handler(req, res);

        expect(res.status).toHaveBeenCalledWith(200);
        expect(res.json).toHaveBeenCalledWith(expect.objectContaining({
            token: 'gh-installation-token'
        }));

        // Verify correct API namespace usage
        expect(mockOctokit.rest.apps.getRepoInstallation).toHaveBeenCalled();
        expect(mockOctokit.rest.apps.createInstallationAccessToken).toHaveBeenCalled();
    });

    test('should return 401 if auth header is missing', async () => {
        req.headers.authorization = undefined;
        await handler(req, res);
        expect(res.status).toHaveBeenCalledWith(401);
        expect(res.json).toHaveBeenCalledWith({ error: 'Missing or invalid Authorization header' });
    });

    test('should return 404 if app is not installed', async () => {
        verifyOIDCToken.mockResolvedValue({});
        validateClaims.mockReturnValue({ repository: 'owner/repo', owner: 'owner' });

        const mockOctokit = {
            rest: {
                apps: {
                    getRepoInstallation: jest.fn().mockRejectedValue({ status: 404 })
                }
            }
        };
        probot.auth.mockResolvedValue(mockOctokit);

        await handler(req, res);

        expect(res.status).toHaveBeenCalledWith(404);
        expect(res.json).toHaveBeenCalledWith({ error: 'HolonBot is not installed on this repository' });
    });
});
