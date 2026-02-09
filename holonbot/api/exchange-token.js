import { probot } from '../lib/probot-client.js';
import { verifyOIDCToken, validateClaims } from '../lib/oidc.js';

const replayCache = new Map();
const rateLimitCache = new Map();
const permissionRank = {
    none: 0,
    read: 1,
    triage: 2,
    write: 3,
    maintain: 4,
    admin: 5,
};

function parseBool(value, defaultValue) {
    if (value === undefined || value === null || value === '') {
        return defaultValue;
    }
    return String(value).toLowerCase() === 'true';
}

function parseIntEnv(value, defaultValue) {
    const parsed = Number.parseInt(String(value ?? ''), 10);
    if (!Number.isFinite(parsed) || parsed <= 0) {
        return defaultValue;
    }
    return parsed;
}

function parseCSV(value) {
    if (!value) {
        return [];
    }
    return String(value)
        .split(',')
        .map((it) => it.trim())
        .filter((it) => it.length > 0);
}

function getRequiredAudiences(env = process.env) {
    const audiences = parseCSV(env.HOLON_OIDC_AUDIENCE);
    if (audiences.length === 0) {
        throw new Error('Missing HOLON_OIDC_AUDIENCE configuration');
    }
    return audiences;
}

function getInstallationPermissions(env = process.env) {
    const raw = env.HOLON_INSTALLATION_PERMISSIONS_JSON;
    if (!raw) {
        return { contents: 'write', pull_requests: 'write' };
    }
    try {
        const parsed = JSON.parse(raw);
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
            throw new Error('not an object');
        }
        return parsed;
    } catch (error) {
        throw new Error(`Invalid HOLON_INSTALLATION_PERMISSIONS_JSON: ${error.message}`);
    }
}

function cleanExpiredEntries(now) {
    for (const [key, expiresAt] of replayCache.entries()) {
        if (expiresAt <= now) {
            replayCache.delete(key);
        }
    }
    for (const [key, state] of rateLimitCache.entries()) {
        if (!state || state.windowStart + state.windowMs <= now) {
            rateLimitCache.delete(key);
        }
    }
}

function applyReplayProtection(claims, repository, env = process.env) {
    const enabled = parseBool(env.HOLON_ENABLE_REPLAY_PROTECTION, true);
    if (!enabled) {
        return;
    }

    const replayId = claims.jti || claims.runId;
    if (!replayId) {
        throw new Error('Missing jti/run_id claim required for replay protection');
    }

    const windowSeconds = parseIntEnv(env.HOLON_REPLAY_WINDOW_SECONDS, 3600);
    const now = Date.now();
    cleanExpiredEntries(now);

    const key = `${repository}:${replayId}`;
    if (replayCache.has(key)) {
        throw new Error('Replay detected for jti/run_id');
    }
    replayCache.set(key, now + (windowSeconds * 1000));
}

function applyRateLimit(claims, repository, env = process.env) {
    const enabled = parseBool(env.HOLON_ENABLE_RATE_LIMIT, true);
    if (!enabled) {
        return;
    }

    const windowSeconds = parseIntEnv(env.HOLON_RATE_LIMIT_WINDOW_SECONDS, 60);
    const maxRequests = parseIntEnv(env.HOLON_RATE_LIMIT_MAX_REQUESTS, 10);
    const actor = claims.actor || 'unknown';
    const now = Date.now();
    cleanExpiredEntries(now);

    const key = `${repository}:${actor}`;
    const windowMs = windowSeconds * 1000;
    const state = rateLimitCache.get(key);
    if (!state || (state.windowStart + state.windowMs) <= now) {
        rateLimitCache.set(key, { count: 1, windowStart: now, windowMs });
        return;
    }

    if (state.count >= maxRequests) {
        throw new Error('Rate limit exceeded');
    }

    state.count += 1;
    rateLimitCache.set(key, state);
}

async function assertActorPermission(appOctokit, claims, env = process.env) {
    const enabled = parseBool(env.HOLON_REQUIRE_ACTOR_COLLABORATOR, true);
    if (!enabled) {
        return;
    }

    const minPermission = String(env.HOLON_MIN_ACTOR_PERMISSION || 'read').toLowerCase();
    if (!Object.prototype.hasOwnProperty.call(permissionRank, minPermission)) {
        throw new Error(`Invalid HOLON_MIN_ACTOR_PERMISSION: ${minPermission}`);
    }

    let permission = 'none';
    try {
        const response = await appOctokit.rest.repos.getCollaboratorPermissionLevel({
            owner: claims.owner,
            repo: claims.repo,
            username: claims.actor,
        });
        permission = String(response.data.permission || 'none').toLowerCase();
    } catch (error) {
        if (error.status === 404) {
            throw new Error(`Actor ${claims.actor} is not a collaborator/member`);
        }
        throw error;
    }

    const actualRank = permissionRank[permission] ?? permissionRank.none;
    const requiredRank = permissionRank[minPermission];
    if (actualRank < requiredRank) {
        throw new Error(`Insufficient actor permission: ${claims.actor} has ${permission}, requires ${minPermission}`);
    }
}

export function resetSecurityCachesForTests() {
    replayCache.clear();
    rateLimitCache.clear();
}

export default async function handler(req, res) {
    // 1. Basic Setup
    if (req.method !== 'POST') {
        res.setHeader('Allow', 'POST');
        return res.status(405).json({ error: 'Method not allowed' });
    }

    const authHeader = req.headers.authorization;
    if (!authHeader || !authHeader.startsWith('Bearer ')) {
        return res.status(401).json({ error: 'Missing or invalid Authorization header' });
    }

    const token = authHeader.split(' ')[1];

    try {
        const audiences = getRequiredAudiences(process.env);

        // 2. Verify OIDC Token and strict claims policy
        const claimsPayload = await verifyOIDCToken(token, { audiences });
        const appOctokit = await probot.auth();
        const claims = validateClaims(claimsPayload, {
            requireWorkflowRef: parseBool(process.env.HOLON_REQUIRE_JOB_WORKFLOW_REF, true),
            allowedWorkflowRefs: parseCSV(process.env.HOLON_ALLOWED_WORKFLOW_REFS),
        });

        // 3. Resolve repository and bind token issuance to the same repository_id
        const repoResponse = await appOctokit.rest.repos.get({
            owner: claims.owner,
            repo: claims.repo,
        });
        const repository = claims.repository;
        const repositoryId = String(repoResponse.data.id);
        if (claims.repositoryId !== repositoryId) {
            return res.status(403).json({
                error: 'OIDC repository_id does not match target repository',
            });
        }

        const enforceDefaultRef = parseBool(process.env.HOLON_REQUIRE_DEFAULT_BRANCH_REF, true);
        if (enforceDefaultRef) {
            const validated = validateClaims(claimsPayload, {
                requireWorkflowRef: parseBool(process.env.HOLON_REQUIRE_JOB_WORKFLOW_REF, true),
                allowedWorkflowRefs: parseCSV(process.env.HOLON_ALLOWED_WORKFLOW_REFS),
                requireDefaultBranchRef: true,
                defaultBranch: repoResponse.data.default_branch,
            });
            claims.ref = validated.ref;
        }

        console.log(`Token request for repository: ${repository} by actor: ${claims.actor}`);

        // 4. Abuse protections and actor permission gate
        applyReplayProtection(claims, repository, process.env);
        applyRateLimit(claims, repository, process.env);
        await assertActorPermission(appOctokit, claims, process.env);

        // 5. Find the app installation for this repository
        let installation;
        try {
            const response = await appOctokit.rest.apps.getRepoInstallation({
                owner: claims.owner,
                repo: claims.repo,
            });
            installation = response.data;
        } catch (err) {
            if (err.status === 404) {
                return res.status(404).json({ error: 'HolonBot is not installed on this repository' });
            }
            throw err;
        }

        // 6. Generate least-privilege installation token scoped to this repository only
        const installationPermissions = getInstallationPermissions(process.env);
        const installationId = installation.id;
        const installationTokenResponse = await appOctokit.rest.apps.createInstallationAccessToken({
            installation_id: installationId,
            repository_ids: [repoResponse.data.id],
            permissions: installationPermissions,
        });

        // 7. Return the token
        return res.status(200).json({
            token: installationTokenResponse.data.token,
            expires_at: installationTokenResponse.data.expires_at,
            permissions: installationTokenResponse.data.permissions
        });

    } catch (error) {
        console.error('Token Exchange Error:', error);
        if (/^Invalid OIDC Token:/.test(error.message)) {
            return res.status(401).json({
                error: 'Invalid OIDC token',
                message: error.message,
            });
        }
        if (/Missing HOLON_OIDC_AUDIENCE|Missing .* claim|repository_id|job_workflow_ref|sub claim|default branch|repository_owner|repository format/.test(error.message)) {
            return res.status(403).json({
                error: 'OIDC claims validation failed',
                message: error.message,
            });
        }
        if (/Replay detected|Rate limit exceeded|collaborator\/member|Insufficient actor permission/.test(error.message)) {
            return res.status(403).json({
                error: 'Token request rejected by security policy',
                message: error.message,
            });
        }
        return res.status(500).json({
            error: 'Token exchange failed',
            message: error.message
        });
    }
}
