/**
 * Holon GitHub App Bot
 *
 * This bot handles various GitHub webhook events for the holon repository
 * including pull requests, issues, and other repository activities.
 */

export default async function app(app) {
  // Log when the app is initialized
  app.log.info('Holon Bot is starting up!');

  // Handle pull request events
  app.on(['pull_request.opened', 'pull_request.reopened'], async (context) => {
    const { payload, repository } = context;
    const pr = payload.pull_request;

    app.log.info(`PR #${pr.number} opened: ${pr.title}`);

    // Welcome message for new PRs
    try {
      await context.octokit.issues.createComment({
        owner: repository.owner.login,
        repo: repository.name,
        issue_number: pr.number,
        body: `👋 Thanks for opening this pull request, @${pr.user.login}!

The Holon team will review it soon. Please make sure:
- [ ] Tests are passing
- [ ] Documentation is updated if needed
- [ ] Code follows the project's style guidelines

Feel free to ask any questions! 🚀`
      });
    } catch (error) {
      app.log.error('Error commenting on PR:', error);
    }
  });

  // Handle issue creation
  app.on(['issues.opened'], async (context) => {
    const { payload, repository } = context;
    const issue = payload.issue;

    app.log.info(`Issue #${issue.number} opened: ${issue.title}`);

    // Auto-assign labels based on issue content
    const labels = [];
    const issueBody = (issue.body || '').toLowerCase();

    if (issueBody.includes('bug') || issue.title.toLowerCase().includes('bug')) {
      labels.push('bug');
    }
    if (issueBody.includes('enhancement') || issueBody.includes('feature')) {
      labels.push('enhancement');
    }
    if (issueBody.includes('question') || issue.title.toLowerCase().includes('?')) {
      labels.push('question');
    }

    if (labels.length > 0) {
      try {
        await context.octokit.issues.addLabels({
          owner: repository.owner.login,
          repo: repository.name,
          issue_number: issue.number,
          labels
        });
        app.log.info(`Added labels to issue #${issue.number}: ${labels.join(', ')}`);
      } catch (error) {
        app.log.error('Error adding labels to issue:', error);
      }
    }

    // Welcome message for new issues
    try {
      await context.octokit.issues.createComment({
        owner: repository.owner.login,
        repo: repository.name,
        issue_number: issue.number,
        body: `👋 Thanks for opening this issue, @${issue.user.login}!

The Holon team will take a look. Please provide as much detail as possible to help us understand and address your concern. 💬`
      });
    } catch (error) {
      app.log.error('Error commenting on issue:', error);
    }
  });

  // Handle pull request reviews
  app.on(['pull_request_review.submitted'], async (context) => {
    const { payload } = context;
    const review = payload.review;

    app.log.info(`Review submitted on PR #${payload.pull_request.number} by @${review.user.login}`);

    // Check for approval and add appropriate label
    if (review.state === 'approved') {
      try {
        await context.octokit.issues.addLabels({
          owner: payload.repository.owner.login,
          repo: payload.repository.name,
          issue_number: payload.pull_request.number,
          labels: ['approved']
        });
      } catch (error) {
        app.log.error('Error adding approved label:', error);
      }
    } else if (review.state === 'changes_requested') {
      try {
        await context.octokit.issues.addLabels({
          owner: payload.repository.owner.login,
          repo: payload.repository.name,
          issue_number: payload.pull_request.number,
          labels: ['changes-requested']
        });
      } catch (error) {
        app.log.error('Error adding changes-requested label:', error);
      }
    }
  });

  // Handle repository events
  app.on(['repository.created'], async (context) => {
    const { repository } = context;

    app.log.info(`New repository created: ${repository.full_name}`);

    // Create initial repository structure
    try {
      const files = {
        'README.md': `# ${repository.name}

Welcome to ${repository.name}! This repository is managed by the Holon team.

## Getting Started

Add your project description and setup instructions here.

## Contributing

Please read our contributing guidelines before submitting pull requests.

## License

Specify your project's license here.
`,

        '.gitignore': `# Dependencies
node_modules/

# Build outputs
dist/
build/

# Environment variables
.env
.env.local
.env.*.local

# IDE files
.vscode/
.idea/
*.swp
*.swo
`
      };

      // Create initial files (this would require appropriate permissions)
      const repo = context.payload.repository;
      const owner =
        repo.owner && (repo.owner.login || repo.owner.name)
          ? (repo.owner.login || repo.owner.name)
          : repo.owner;
      const repoName = repo.name;

      for (const [path, content] of Object.entries(files)) {
        await context.octokit.repos.createOrUpdateFileContents({
          owner,
          repo: repoName,
          path,
          message: `chore: add initial ${path}`,
          content: Buffer.from(content, 'utf8').toString('base64'),
        });
      }
      app.log.info('Repository structure setup complete');
    } catch (error) {
      app.log.error('Error setting up repository:', error);
    }
  });

  // Health check endpoint
  app.on(['check_run.created'], async (context) => {
    const { payload } = context;
    app.log.info(`Check run created: ${payload.check_run.name}`);
  });

  // Error handling
  app.onError((error) => {
    app.log.error('Error occurred in the app:', error);
  });

  // Log when app is loaded
  app.log.info('Holon Bot is ready to receive events!');
}