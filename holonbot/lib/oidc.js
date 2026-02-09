import { createRemoteJWKSet, jwtVerify } from 'jose';

// GitHub's OIDC JWKS URL
const GITHUB_JWKS_URI = 'https://token.actions.githubusercontent.com/.well-known/jwks';
const GITHUB_ISSUER = 'https://token.actions.githubusercontent.com';

// Cache the JWKS for performance
const JWKS = createRemoteJWKSet(new URL(GITHUB_JWKS_URI));

const REPO_FULL_NAME_RE = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;

/**
 * Verify the OIDC token from GitHub Actions
 * @param {string} token - The raw JWT token
 * @param {Object} options
 * @param {string[]} options.audiences - Allowed audiences
 * @returns {Promise<Object>} - The verified claims
 */
export async function verifyOIDCToken(token, options = {}) {
    const audiences = Array.isArray(options.audiences)
        ? options.audiences.filter((aud) => typeof aud === 'string' && aud.trim() !== '')
        : [];

    if (audiences.length === 0) {
        throw new Error('OIDC audience validation is required (set HOLON_OIDC_AUDIENCE)');
    }

    try {
        const { payload } = await jwtVerify(token, JWKS, {
            issuer: GITHUB_ISSUER,
            audience: audiences,
        });
        return payload;
    } catch (error) {
        throw new Error(`Invalid OIDC Token: ${error.message}`);
    }
}

/**
 * Validate that the claims meet our security policy
 * @param {Object} claims - Verified JWT claims
 * @param {Object} options
 * @param {boolean} options.requireWorkflowRef - Require job_workflow_ref claim
 * @param {string[]} options.allowedWorkflowRefs - Allowed workflow refs
 * @param {boolean} options.requireDefaultBranchRef - Require ref to be default branch
 * @param {string} options.defaultBranch - Default branch name
 * @returns {Object} - Validated info (repository, owner, installationId logic candidates)
 */
export function validateClaims(claims, options = {}) {
    if (!claims || typeof claims !== 'object') {
        throw new Error('Missing OIDC claims');
    }

    const requireWorkflowRef = options.requireWorkflowRef !== false;
    const allowedWorkflowRefs = Array.isArray(options.allowedWorkflowRefs)
        ? options.allowedWorkflowRefs.filter((it) => typeof it === 'string' && it.trim() !== '')
        : [];
    const requireDefaultBranchRef = options.requireDefaultBranchRef === true;
    const defaultBranch = typeof options.defaultBranch === 'string' ? options.defaultBranch.trim() : '';

    if (!claims.repository || !claims.repository_owner) {
        throw new Error('Missing repository information in OIDC token');
    }

    if (typeof claims.repository !== 'string' || !REPO_FULL_NAME_RE.test(claims.repository)) {
        throw new Error('Invalid repository format in OIDC token');
    }

    const [owner, repo] = claims.repository.split('/');
    if (claims.repository_owner !== owner) {
        throw new Error('repository_owner does not match repository owner');
    }

    if (claims.sub !== `repo:${claims.repository}:ref:${claims.ref}` && !String(claims.sub || '').startsWith(`repo:${claims.repository}:`)) {
        throw new Error('sub claim does not match repository');
    }

    if (!claims.repository_id || !/^\d+$/.test(String(claims.repository_id))) {
        throw new Error('Missing or invalid repository_id claim');
    }

    if (!claims.actor || typeof claims.actor !== 'string') {
        throw new Error('Missing actor claim');
    }

    if (!claims.ref || typeof claims.ref !== 'string') {
        throw new Error('Missing ref claim');
    }

    if (requireWorkflowRef && (!claims.job_workflow_ref || typeof claims.job_workflow_ref !== 'string')) {
        throw new Error('Missing job_workflow_ref claim');
    }

    if (allowedWorkflowRefs.length > 0 && !allowedWorkflowRefs.includes(claims.job_workflow_ref)) {
        throw new Error('job_workflow_ref is not allowed');
    }

    if (requireDefaultBranchRef) {
        if (!defaultBranch) {
            throw new Error('default branch is required when HOLON_REQUIRE_DEFAULT_BRANCH_REF=true');
        }
        if (claims.ref !== `refs/heads/${defaultBranch}`) {
            throw new Error(`ref is not default branch: expected refs/heads/${defaultBranch}`);
        }
    }

    return {
        repository: claims.repository,
        owner,
        repo,
        repositoryId: String(claims.repository_id),
        actor: claims.actor,
        ref: claims.ref,
        sub: claims.sub,
        workflowRef: claims.job_workflow_ref || '',
        runId: claims.run_id ? String(claims.run_id) : '',
        jti: claims.jti ? String(claims.jti) : '',
    };
}
