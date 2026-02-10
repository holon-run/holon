import { jest, describe, test, expect, beforeEach, afterAll } from '@jest/globals';

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
const { default: handler, resetSecurityCachesForTests } = await import('../api/exchange-token.js');
const { probot } = await import('../lib/probot-client.js');
const { verifyOIDCToken, validateClaims } = await import('../lib/oidc.js');

describe('exchange-token handler', () => {
    let req, res;
    const originalEnv = process.env;
    let appOctokit;
    let installationOctokit;

    function setupDefaultOctokits() {
        appOctokit = {
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
        installationOctokit = {
            rest: {
                repos: {
                    get: jest.fn().mockResolvedValue({
                        data: { id: 42, default_branch: 'main' }
                    }),
                    getCollaboratorPermissionLevel: jest.fn().mockResolvedValue({
                        data: { permission: 'write' }
                    }),
                }
            }
        };
        probot.auth.mockImplementation((installationId) => {
            if (installationId === undefined) {
                return Promise.resolve(appOctokit);
            }
            if (installationId === 123) {
                return Promise.resolve(installationOctokit);
            }
            throw new Error(`unexpected installation id: ${installationId}`);
        });
    }

    beforeEach(() => {
        process.env = {
            ...originalEnv,
            HOLON_OIDC_AUDIENCE: 'holon-broker',
            HOLON_REQUIRE_DEFAULT_BRANCH_REF: 'false',
        };
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
        resetSecurityCachesForTests();
        setupDefaultOctokits();
    });

    afterAll(() => {
        process.env = originalEnv;
    });

    test('should exchange token successfully', async () => {
        // 1. Mock OIDC verification
        verifyOIDCToken.mockResolvedValue({ sub: 'repo:owner/repo' });
        validateClaims.mockReturnValue({
            repository: 'owner/repo',
            owner: 'owner',
            repo: 'repo',
            repositoryId: '42',
            actor: 'jolestar',
            ref: 'refs/heads/main',
            runId: 'run-1',
            jti: 'jti-1',
        });

        await handler(req, res);

        expect(res.status).toHaveBeenCalledWith(200);
        expect(res.json).toHaveBeenCalledWith(expect.objectContaining({
            token: 'gh-installation-token'
        }));

        // Verify correct API namespace usage
        expect(appOctokit.rest.apps.getRepoInstallation).toHaveBeenCalled();
        expect(appOctokit.rest.apps.createInstallationAccessToken).toHaveBeenCalledWith(expect.objectContaining({
            installation_id: 123,
            repository_ids: [42],
            permissions: { contents: 'write', pull_requests: 'write' }
        }));
        expect(verifyOIDCToken).toHaveBeenCalledWith('valid-oidc-token', { audiences: ['holon-broker'] });
    });

    test('should default OIDC audience when HOLON_OIDC_AUDIENCE is not configured', async () => {
        delete process.env.HOLON_OIDC_AUDIENCE;
        verifyOIDCToken.mockResolvedValue({ sub: 'repo:owner/repo' });
        validateClaims.mockReturnValue({
            repository: 'owner/repo',
            owner: 'owner',
            repo: 'repo',
            repositoryId: '42',
            actor: 'jolestar',
            ref: 'refs/heads/main',
            runId: 'run-1',
            jti: 'jti-1',
        });

        await handler(req, res);

        expect(res.status).toHaveBeenCalledWith(200);
        expect(verifyOIDCToken).toHaveBeenCalledWith('valid-oidc-token', { audiences: ['holon-token-broker'] });
    });

    test('should return 401 if auth header is missing', async () => {
        req.headers.authorization = undefined;
        await handler(req, res);
        expect(res.status).toHaveBeenCalledWith(401);
        expect(res.json).toHaveBeenCalledWith({ error: 'Missing or invalid Authorization header' });
    });

    test('should return 404 if app is not installed', async () => {
        verifyOIDCToken.mockResolvedValue({});
        validateClaims.mockReturnValue({
            repository: 'owner/repo',
            owner: 'owner',
            repo: 'repo',
            repositoryId: '42',
            actor: 'jolestar',
            ref: 'refs/heads/main',
            runId: 'run-1',
            jti: 'jti-1',
        });

        appOctokit.rest.apps.getRepoInstallation.mockRejectedValue({ status: 404 });

        await handler(req, res);

        expect(res.status).toHaveBeenCalledWith(404);
        expect(res.json).toHaveBeenCalledWith({ error: 'HolonBot is not installed on this repository' });
    });

    test('should reject when actor is not collaborator', async () => {
        verifyOIDCToken.mockResolvedValue({});
        validateClaims.mockReturnValue({
            repository: 'owner/repo',
            owner: 'owner',
            repo: 'repo',
            repositoryId: '42',
            actor: 'outsider',
            ref: 'refs/heads/main',
            runId: 'run-1',
            jti: 'jti-1',
        });

        installationOctokit.rest.repos.getCollaboratorPermissionLevel.mockRejectedValue({ status: 404 });

        await handler(req, res);

        expect(res.status).toHaveBeenCalledWith(403);
        expect(res.json).toHaveBeenCalledWith(expect.objectContaining({
            error: 'Token request rejected by security policy',
        }));
    });

    test('should reject replayed request by jti', async () => {
        verifyOIDCToken.mockResolvedValue({});
        validateClaims.mockReturnValue({
            repository: 'owner/repo',
            owner: 'owner',
            repo: 'repo',
            repositoryId: '42',
            actor: 'jolestar',
            ref: 'refs/heads/main',
            runId: 'run-1',
            jti: 'same-jti',
        });

        await handler(req, res);
        await handler(req, res);

        expect(res.status).toHaveBeenLastCalledWith(403);
        expect(res.json).toHaveBeenLastCalledWith(expect.objectContaining({
            error: 'Token request rejected by security policy',
            code: 'policy.replay.detected',
            message: 'Replay detected for jti/run_id',
        }));
    });

    test('should reject when rate limit is exceeded', async () => {
        process.env.HOLON_RATE_LIMIT_MAX_REQUESTS = '1';
        process.env.HOLON_ENABLE_REPLAY_PROTECTION = 'false';

        verifyOIDCToken.mockResolvedValue({});
        validateClaims
            .mockReturnValueOnce({
                repository: 'owner/repo',
                owner: 'owner',
                repo: 'repo',
                repositoryId: '42',
                actor: 'jolestar',
                ref: 'refs/heads/main',
                runId: 'run-1',
                jti: 'jti-1',
            })
            .mockReturnValueOnce({
                repository: 'owner/repo',
                owner: 'owner',
                repo: 'repo',
                repositoryId: '42',
                actor: 'jolestar',
                ref: 'refs/heads/main',
                runId: 'run-2',
                jti: 'jti-2',
            });

        await handler(req, res);
        await handler(req, res);

        expect(res.status).toHaveBeenLastCalledWith(403);
        expect(res.json).toHaveBeenLastCalledWith(expect.objectContaining({
            error: 'Token request rejected by security policy',
            code: 'policy.rate_limited',
            message: 'Rate limit exceeded',
        }));
    });

    test('should pass allowPullRequestRef option during default-branch enforcement', async () => {
        process.env.HOLON_REQUIRE_DEFAULT_BRANCH_REF = 'true';
        process.env.HOLON_ALLOW_PULL_REQUEST_REF = 'true';

        verifyOIDCToken.mockResolvedValue({ sub: 'repo:owner/repo:pull_request' });
        validateClaims
            .mockReturnValueOnce({
                repository: 'owner/repo',
                owner: 'owner',
                repo: 'repo',
                repositoryId: '42',
                actor: 'jolestar',
                ref: 'refs/pull/621/merge',
                runId: 'run-1',
                jti: 'jti-1',
            })
            .mockReturnValueOnce({
                repository: 'owner/repo',
                owner: 'owner',
                repo: 'repo',
                repositoryId: '42',
                actor: 'jolestar',
                ref: 'refs/pull/621/merge',
                runId: 'run-1',
                jti: 'jti-1',
            });

        await handler(req, res);

        expect(res.status).toHaveBeenCalledWith(200);
        expect(validateClaims).toHaveBeenLastCalledWith(expect.any(Object), expect.objectContaining({
            requireDefaultBranchRef: true,
            defaultBranch: 'main',
            allowPullRequestRef: true,
        }));
    });
});
