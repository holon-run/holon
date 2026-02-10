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
            repository: 'holon-run/holon',
            repository_id: '123456',
            repository_owner: 'holon-run',
            actor: 'jolestar',
            ref: 'refs/heads/main',
            sub: 'repo:holon-run/holon:ref:refs/heads/main',
            job_workflow_ref: 'holon-run/holon/.github/workflows/holon-trigger.yml@refs/heads/main',
            run_id: '1',
            jti: 'abc'
        };

        const result = validateClaims(claims);
        expect(result).toEqual({
            repository: 'holon-run/holon',
            owner: 'holon-run',
            repo: 'holon',
            repositoryId: '123456',
            actor: 'jolestar',
            ref: 'refs/heads/main',
            sub: 'repo:holon-run/holon:ref:refs/heads/main',
            workflowRef: 'holon-run/holon/.github/workflows/holon-trigger.yml@refs/heads/main',
            runId: '1',
            jti: 'abc'
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
            repository: 'holon-run/holon'
        };
        expect(() => validateClaims(claims)).toThrow('Missing repository information');
    });

    test('should enforce workflow allowlist when configured', () => {
        const claims = {
            repository: 'holon-run/holon',
            repository_id: '123456',
            repository_owner: 'holon-run',
            actor: 'jolestar',
            ref: 'refs/heads/main',
            sub: 'repo:holon-run/holon:ref:refs/heads/main',
            job_workflow_ref: 'holon-run/holon/.github/workflows/other.yml@refs/heads/main',
        };

        expect(() => validateClaims(claims, {
            allowedWorkflowRefs: ['holon-run/holon/.github/workflows/holon-trigger.yml@refs/heads/main']
        })).toThrow('job_workflow_ref is not allowed');
    });

    test('should allow pull request refs when default-branch enforcement is enabled', () => {
        const claims = {
            repository: 'holon-run/holon',
            repository_id: '123456',
            repository_owner: 'holon-run',
            actor: 'jolestar',
            ref: 'refs/pull/621/merge',
            sub: 'repo:holon-run/holon:pull_request',
            job_workflow_ref: 'holon-run/holon/.github/workflows/holon-trigger.yml@refs/pull/621/merge',
        };

        const result = validateClaims(claims, {
            requireDefaultBranchRef: true,
            defaultBranch: 'main',
        });

        expect(result.ref).toBe('refs/pull/621/merge');
    });

    test('should reject pull request refs when explicitly disabled', () => {
        const claims = {
            repository: 'holon-run/holon',
            repository_id: '123456',
            repository_owner: 'holon-run',
            actor: 'jolestar',
            ref: 'refs/pull/621/merge',
            sub: 'repo:holon-run/holon:pull_request',
            job_workflow_ref: 'holon-run/holon/.github/workflows/holon-trigger.yml@refs/pull/621/merge',
        };

        expect(() => validateClaims(claims, {
            requireDefaultBranchRef: true,
            defaultBranch: 'main',
            allowPullRequestRef: false,
        })).toThrow('ref is not default branch: expected refs/heads/main');
    });
});

describe('verifyOIDCToken', () => {
    const GITHUB_ISSUER = 'https://token.actions.githubusercontent.com';

    test('should verify a valid token', async () => {
        const mockPayload = { repository: 'owner/repo', repository_owner: 'owner' };
        jose.jwtVerify.mockResolvedValueOnce({ payload: mockPayload });

        const token = 'valid.token.here';
        const result = await verifyOIDCToken(token, { audiences: ['holon-broker'] });

        expect(jose.jwtVerify).toHaveBeenCalledWith(
            token,
            expect.any(Function),
            expect.objectContaining({
                issuer: GITHUB_ISSUER,
                audience: ['holon-broker']
            })
        );
        expect(result).toEqual(mockPayload);
    });

    test('should fail when audiences are missing', async () => {
        await expect(verifyOIDCToken('valid.token.here')).rejects.toThrow('OIDC audience validation is required');
    });

    test('should throw error if verification fails', async () => {
        jose.jwtVerify.mockRejectedValueOnce(new Error('Invalid signature'));

        await expect(verifyOIDCToken('invalid-token', { audiences: ['holon-broker'] })).rejects.toThrow('Invalid OIDC Token: Invalid signature');
    });
});
