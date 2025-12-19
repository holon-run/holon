import { createRemoteJWKSet, jwtVerify } from 'jose';

// GitHub's OIDC JWKS URL
const GITHUB_JWKS_URI = 'https://token.actions.githubusercontent.com/.well-known/jwks';
const GITHUB_ISSUER = 'https://token.actions.githubusercontent.com';

// Cache the JWKS for performance
const JWKS = createRemoteJWKSet(new URL(GITHUB_JWKS_URI));

/**
 * Verify the OIDC token from GitHub Actions
 * @param {string} token - The raw JWT token
 * @returns {Promise<Object>} - The verified claims
 */
export async function verifyOIDCToken(token) {
    try {
        const { payload } = await jwtVerify(token, JWKS, {
            issuer: GITHUB_ISSUER,
            // Remove audience validation to be permissive
        });
        return payload;
    } catch (error) {
        throw new Error(`Invalid OIDC Token: ${error.message}`);
    }
}

/**
 * Validate that the claims meet our security policy
 * @param {Object} claims - Verified JWT claims
 * @returns {Object} - Validated info (repository, owner, installationId logic candidates)
 */
export function validateClaims(claims) {
    // 1. Must have a repository
    if (!claims.repository || !claims.repository_owner) {
        throw new Error('Missing repository information in OIDC token');
    }

    // 2. (Optional) Check specific actor or branch policies here
    // For now, we allow any valid workflow in the repo to request a token, 
    // relying on the fact that they need to install the App on that repo first.

    return {
        repository: claims.repository,
        owner: claims.repository_owner,
        actor: claims.actor,
        ref: claims.ref
    };
}
