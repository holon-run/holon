import { probot } from '../lib/probot-client.js';
import { verifyOIDCToken, validateClaims } from '../lib/oidc.js';


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
        // 2. Verify OIDC Token
        const claims = await verifyOIDCToken(token);
        const { repository, owner } = validateClaims(claims);

        console.log(`Token request for repository: ${repository} by actor: ${claims.actor}`);

        // 3. Find the App Installation for this repository
        // We use probot.auth() to get an authenticated Octokit (as the App itself)
        // This is different from the standard Probot webhook flow where `context` is provided.
        // Here we manually query for the installation.
        const appOctokit = await probot.auth();

        // https://docs.github.com/en/rest/apps/apps?apiVersion=2022-11-28#get-a-repository-installation-for-the-authenticated-app
        let installation;
        try {
            const response = await appOctokit.apps.getRepoInstallation({
                owner,
                repo: repository.split('/')[1] // 'owner/repo' -> 'repo'
            });
            installation = response.data;
        } catch (err) {
            if (err.status === 404) {
                return res.status(404).json({ error: 'HolonBot is not installed on this repository' });
            }
            throw err;
        }

        // 4. Generate Installation Token
        // We can restrict permissions here using the `permissions` arg if we want partial scope.
        // For now, we give the default permissions the App has (or what's requested/needed).
        // Issue #27 suggests implementing scoping logic, but for this first pass, we return a standard token.

        const installationId = installation.id;
        const installationTokenResponse = await appOctokit.apps.createInstallationAccessToken({
            installation_id: installationId,
            // permissions: { contents: 'write' } // Example: restriction
        });

        // 5. Return the Token
        return res.status(200).json({
            token: installationTokenResponse.data.token,
            expires_at: installationTokenResponse.data.expires_at,
            permissions: installationTokenResponse.data.permissions
        });

    } catch (error) {
        console.error('Token Exchange Error:', error);
        return res.status(500).json({
            error: 'Token exchange failed',
            message: error.message
        });
    }
}
