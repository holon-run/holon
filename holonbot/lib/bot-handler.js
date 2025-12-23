/**
 * Holon GitHub App Bot
 *
 * This bot handles various GitHub webhook events for the holon repository.
 *
 * Features:
 * - Passive logging of all events
 * - Publishing Holon execution results (review replies + summary) to PRs
 */


export default async function botHandler(app) {
  // Log when the app is initialized
  app.log.info('Holon Bot is starting up!');

  // Listen to all relevant events for logging
  app.onAny(async (context) => {
    const { name, payload } = context;
    const action = payload.action ? `.${payload.action}` : '';

    app.log.info(`Received event: ${name}${action}`);

    // Detailed logging for debugging if needed, can be removed in production
    // app.log.debug(payload);
  });

  // Listen for push events to PR branches - this indicates Holon may have pushed fixes
  app.on('push', async (context) => {
    const { payload, repository } = context;
    const { ref, pusher, head_commit } = payload;

    // Only process pushes to branches (not tags)
    if (!ref.startsWith('refs/heads/')) {
      return;
    }

    const branch = ref.replace('refs/heads/', '');
    const owner = repository.owner.login;
    const repo = repository.name;

    // Skip if push was by holonbot itself to avoid loops
    if (pusher.name === 'holonbot' || pusher.name === 'holonbot[bot]') {
      app.log.info('Skipping push from holonbot itself to avoid loops');
      return;
    }

    app.log.info(`Push to branch '${branch}' by ${pusher.name}`);

    try {
      // Check if this branch has associated PRs
      const { data: pulls } = await context.octokit.rest.pulls.list({
        owner,
        repo,
        state: 'open',
        head: `${owner}:${branch}`,
        per_page: 1
      });

      if (pulls.length === 0) {
        app.log.info(`No open PR found for branch ${branch}`);
        return;
      }

      const pr = pulls[0];
      app.log.info(`Found PR #${pr.number} for branch ${branch}`);

      // Check if commit message indicates Holon fix
      if (head_commit && head_commit.message) {
        const commitMsg = head_commit.message.toLowerCase();
        const isHolonFix = commitMsg.includes('holon') ||
                           commitMsg.includes('automated fix') ||
                           commitMsg.includes('co-authored-by: claude');

        if (!isHolonFix) {
          app.log.info('Push does not appear to be a Holon fix commit');
          return;
        }
      }

      // Look for Holon output artifacts
      // For now, we'll check if the workflow run created artifacts
      // The actual output files will be downloaded by the workflow
      // We need to listen for workflow completion events

      app.log.info(`Holon fix detected on PR #${pr.number}`);

    } catch (error) {
      app.log.error('Error processing push event:', error);
    }
  });

  // Listen for workflow run completion to publish results
  app.on('workflow_run.completed', async (context) => {
    const { payload } = context;
    const { workflow_run, repository, action } = payload;

    const owner = repository.owner.login;
    const repo = repository.name;

    // Only process holonbot-fix.yml workflow runs
    if (workflow_run.name !== 'Holonbot Fix Workflow') {
      app.log.info(`Skipping non-Holon workflow: ${workflow_run.name}`);
      return;
    }

    app.log.info(`Holonbot workflow ${action} for run #${workflow_run.id}`);

    // Only process completed (successful) runs
    if (workflow_run.conclusion !== 'success') {
      app.log.info(`Workflow run did not succeed: ${workflow_run.conclusion}`);
      return;
    }

    try {
      // Extract PR number from workflow run
      // The workflow_run.event should be 'issue_comment' for our fix workflow
      if (workflow_run.event !== 'issue_comment') {
        app.log.info(`Workflow run was triggered by ${workflow_run.event}, not issue_comment`);
        return;
      }

      // Get the issue number from the workflow run
      const issueNumber = workflow_run.pull_requests?.[0]?.number;

      if (!issueNumber) {
        app.log.info('No PR number associated with this workflow run');
        return;
      }

      app.log.info(`Found associated PR #${issueNumber}`);

      // Download artifacts from the workflow run
      const artifactName = `holon-fix-output-pr-${issueNumber}`;

      // List artifacts for this workflow run
      const { data: artifacts } = await context.octokit.rest.actions.listWorkflowRunArtifacts({
        owner,
        repo,
        run_id: workflow_run.id
      });

      const holonArtifact = artifacts.artifacts.find(a => a.name === artifactName);

      if (!holonArtifact) {
        app.log.warn(`Holon artifact not found: ${artifactName}`);
        return;
      }

      app.log.info(`Found Holon artifact: ${holonArtifact.name} (${holonArtifact.size_in_bytes} bytes)`);

      // Download the artifact
      const { data: zipBuffer } = await context.octokit.rest.actions.downloadArtifact({
        owner,
        repo,
        artifact_id: holonArtifact.id,
        archive_format: 'zip'
      });

      app.log.info(`Downloaded artifact: ${zipBuffer.length} bytes`);

      // Note: In a serverless environment, we need to extract the zip
      // This requires adding a dependency like adm-zip or unzipper
      // For now, we'll log that we found the artifact
      // The actual processing can be done in the workflow itself
      // or we can add extraction logic here

      app.log.info('Holon output artifact found - publisher workflow should process it');

    } catch (error) {
      app.log.error('Error processing workflow_run event:', error);
    }
  });

  // Error handling
  app.onError((error) => {
    app.log.error('Error occurred in the app:', error);
  });

  // Log when app is loaded
  app.log.info('Holon Bot is ready to receive events!');
}