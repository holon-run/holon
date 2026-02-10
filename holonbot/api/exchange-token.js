import { probot } from '../lib/probot-client.js';
import { verifyOIDCToken, validateClaims } from '../lib/oidc.js';

const replayCache = new Map();
const rateLimitCache = new Map();
let lastCleanupAtMs = 0;
const defaultOIDCAudiences = ['holon-token-broker'];
const permissionRank = {
    none: 0,
    read: 1,
    triage: 2,
    write: 3,
    maintain: 4,
    admin: 5,
};

class HttpError extends Error {
    constructor(status, code, message) {
        super(message);
        this.name = 'HttpError';
        this.status = status;
        this.code = code;
    }
}

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
        return defaultOIDCAudiences;
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
            throw new HttpError(500, 'config.invalid', 'HOLON_INSTALLATION_PERMISSIONS_JSON must be an object');
        }
        return parsed;
    } catch (error) {
        if (error instanceof HttpError) {
            throw error;
        }
        throw new HttpError(500, 'config.invalid', `Invalid HOLON_INSTALLATION_PERMISSIONS_JSON: ${error.message}`);
    }
}

function sanitizeForLog(value) {
    return String(value ?? '').replace(/[\r\n\t]/g, ' ').replace(/[^\x20-\x7E]/g, '?');
}

function enforceCacheLimit(map, maxSize) {
    while (map.size > maxSize) {
        const firstKey = map.keys().next().value;
        if (firstKey === undefined) {
            break;
        }
        map.delete(firstKey);
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

function maybeCleanupCaches(now, env = process.env) {
    const cleanupIntervalMs = parseIntEnv(env.HOLON_CACHE_CLEANUP_INTERVAL_MS, 5000);
    if ((now - lastCleanupAtMs) < cleanupIntervalMs) {
        return;
    }
    cleanExpiredEntries(now);
    lastCleanupAtMs = now;
}

function applyReplayProtection(claims, repository, env = process.env) {
    const enabled = parseBool(env.HOLON_ENABLE_REPLAY_PROTECTION, true);
    if (!enabled) {
        return;
    }

    const replayId = claims.jti || claims.runId;
    if (!replayId) {
        throw new HttpError(403, 'policy.replay.invalid_claim', 'Missing jti/run_id claim required for replay protection');
    }

    const windowSeconds = parseIntEnv(env.HOLON_REPLAY_WINDOW_SECONDS, 3600);
    const maxCacheSize = parseIntEnv(env.HOLON_REPLAY_CACHE_MAX_SIZE, 10000);
    const now = Date.now();
    maybeCleanupCaches(now, env);

    const key = `${repository}:${replayId}`;
    if (replayCache.has(key)) {
        throw new HttpError(403, 'policy.replay.detected', 'Replay detected for jti/run_id');
    }
    replayCache.delete(key);
    replayCache.set(key, now + (windowSeconds * 1000));
    enforceCacheLimit(replayCache, maxCacheSize);
}

function applyRateLimit(claims, repository, env = process.env) {
    const enabled = parseBool(env.HOLON_ENABLE_RATE_LIMIT, true);
    if (!enabled) {
        return;
    }

    const windowSeconds = parseIntEnv(env.HOLON_RATE_LIMIT_WINDOW_SECONDS, 60);
    const maxRequests = parseIntEnv(env.HOLON_RATE_LIMIT_MAX_REQUESTS, 10);
    const maxCacheSize = parseIntEnv(env.HOLON_RATE_LIMIT_CACHE_MAX_SIZE, 10000);
    const actor = claims.actor || 'unknown';
    const now = Date.now();
    maybeCleanupCaches(now, env);

    const key = `${repository}:${actor}`;
    const windowMs = windowSeconds * 1000;
    const state = rateLimitCache.get(key);
    if (!state || (state.windowStart + state.windowMs) <= now) {
        rateLimitCache.set(key, { count: 1, windowStart: now, windowMs });
        return;
    }

    if (state.count >= maxRequests) {
        throw new HttpError(403, 'policy.rate_limited', 'Rate limit exceeded');
    }

    const updated = { ...state, count: state.count + 1 };
    rateLimitCache.delete(key);
    rateLimitCache.set(key, updated);
    enforceCacheLimit(rateLimitCache, maxCacheSize);
}

async function assertActorPermission(installationOctokit, claims, env = process.env) {
    const enabled = parseBool(env.HOLON_REQUIRE_ACTOR_COLLABORATOR, true);
    if (!enabled) {
        return;
    }

    const minPermission = String(env.HOLON_MIN_ACTOR_PERMISSION || 'read').toLowerCase();
    if (!Object.prototype.hasOwnProperty.call(permissionRank, minPermission)) {
        throw new HttpError(500, 'config.invalid', `Invalid HOLON_MIN_ACTOR_PERMISSION: ${minPermission}`);
    }

    let permission;
    try {
        const response = await installationOctokit.rest.repos.getCollaboratorPermissionLevel({
            owner: claims.owner,
            repo: claims.repo,
            username: claims.actor,
        });
        permission = String(response.data.permission || 'none').toLowerCase();
    } catch (error) {
        if (error.status === 404) {
            throw new HttpError(403, 'policy.actor_not_collaborator', `Actor ${claims.actor} is not a collaborator/member`);
        }
        if (error.status === 401 || error.status === 403) {
            throw new HttpError(500, 'github.auth_failed', `Failed to verify collaborator permission: ${error.message}`);
        }
        if (error.status === 429) {
            throw new HttpError(503, 'github.rate_limited', 'GitHub API rate limit while verifying collaborator permission');
        }
        throw error;
    }

    const actualRank = permissionRank[permission] ?? permissionRank.none;
    const requiredRank = permissionRank[minPermission];
    if (actualRank < requiredRank) {
        throw new HttpError(
            403,
            'policy.actor_permission_insufficient',
            `Insufficient actor permission: ${claims.actor} has ${permission}, requires ${minPermission}`
        );
    }
}

export function resetSecurityCachesForTests() {
    replayCache.clear();
    rateLimitCache.clear();
    lastCleanupAtMs = 0;
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
        let claimsPayload;
        try {
            claimsPayload = await verifyOIDCToken(token, { audiences });
        } catch (error) {
            if (String(error.message || '').startsWith('Invalid OIDC Token:')) {
                throw new HttpError(401, 'oidc.invalid_token', error.message);
            }
            throw error;
        }

        let claims;
        try {
            claims = validateClaims(claimsPayload, {
                requireWorkflowRef: parseBool(process.env.HOLON_REQUIRE_JOB_WORKFLOW_REF, true),
                allowedWorkflowRefs: parseCSV(process.env.HOLON_ALLOWED_WORKFLOW_REFS),
            });
        } catch (error) {
            throw new HttpError(403, 'oidc.invalid_claims', error.message);
        }

        const appOctokit = await probot.auth();

        // 3. Find the app installation for this repository using app-authenticated client.
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
            if (err.status === 401 || err.status === 403) {
                throw new HttpError(500, 'github.auth_failed', `GitHub app authentication failed: ${err.message}`);
            }
            if (err.status === 429) {
                throw new HttpError(503, 'github.rate_limited', 'GitHub API rate limit while resolving repository installation');
            }
            throw err;
        }

        const installationOctokit = await probot.auth(installation.id);

        // 4. Resolve repository metadata and bind token issuance to the same repository_id.
        let repoResponse;
        try {
            repoResponse = await installationOctokit.rest.repos.get({
                owner: claims.owner,
                repo: claims.repo,
            });
        } catch (error) {
            if (error.status === 404) {
                throw new HttpError(403, 'oidc.invalid_claims', 'Repository in OIDC token does not exist or is not accessible');
            }
            if (error.status === 401 || error.status === 403) {
                throw new HttpError(500, 'github.auth_failed', `Failed to query repository metadata: ${error.message}`);
            }
            if (error.status === 429) {
                throw new HttpError(503, 'github.rate_limited', 'GitHub API rate limit while reading repository metadata');
            }
            throw error;
        }

        const repository = claims.repository;
        const repositoryId = String(repoResponse.data.id);
        if (claims.repositoryId !== repositoryId) {
            return res.status(403).json({
                error: 'OIDC repository_id does not match target repository',
            });
        }

        const enforceDefaultRef = parseBool(process.env.HOLON_REQUIRE_DEFAULT_BRANCH_REF, true);
        if (enforceDefaultRef) {
            let validated;
            try {
                validated = validateClaims(claimsPayload, {
                    requireWorkflowRef: parseBool(process.env.HOLON_REQUIRE_JOB_WORKFLOW_REF, true),
                    allowedWorkflowRefs: parseCSV(process.env.HOLON_ALLOWED_WORKFLOW_REFS),
                    requireDefaultBranchRef: true,
                    defaultBranch: repoResponse.data.default_branch,
                });
            } catch (error) {
                throw new HttpError(403, 'oidc.invalid_claims', error.message);
            }
            claims.ref = validated.ref;
        }

        console.log(
            `Token request for repository: ${sanitizeForLog(repository)} by actor: ${sanitizeForLog(claims.actor)}`
        );

        // 5. Abuse protections and actor permission gate
        applyReplayProtection(claims, repository, process.env);
        applyRateLimit(claims, repository, process.env);
        await assertActorPermission(installationOctokit, claims, process.env);

        // 6. Generate least-privilege installation token scoped to this repository only.
        const installationPermissions = getInstallationPermissions(process.env);
        const installationTokenResponse = await appOctokit.rest.apps.createInstallationAccessToken({
            installation_id: installation.id,
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
        if (error instanceof HttpError) {
            const response = {
                error: 'Token exchange failed',
                code: error.code,
                message: error.message,
            };
            if (error.status === 401) {
                response.error = 'Invalid OIDC token';
            } else if (error.status === 403 && error.code.startsWith('oidc.')) {
                response.error = 'OIDC claims validation failed';
            } else if (error.status === 403 && error.code.startsWith('policy.')) {
                response.error = 'Token request rejected by security policy';
            } else if (error.status >= 500 && error.code.startsWith('github.')) {
                response.error = 'GitHub API error';
            } else if (error.status >= 500 && error.code.startsWith('config.')) {
                response.error = 'Token broker misconfiguration';
            }
            return res.status(error.status).json(response);
        }
        return res.status(500).json({
            error: 'Token exchange failed',
            message: error.message
        });
    }
}
