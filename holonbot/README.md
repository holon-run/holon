# HolonBot - Serverless GitHub App

A scalable, serverless GitHub App bot built with Probot and deployed on Vercel. The bot automates repository management tasks, handles pull requests, issues, and provides automated responses to repository events.

## Features

- **Pull Request Management**: Welcome messages, automated comments, and labeling
- **Issue Handling**: Auto-labeling based on content, welcome messages
- **Review Management**: Automatic labeling based on review states
- **Repository Setup**: Initial structure creation for new repositories
- **Serverless Deployment**: No server maintenance required with Vercel
- **Secure Configuration**: Environment variables for all sensitive data

## Architecture

```
holonbot/
├── api/
│   ├── github-webhook.js    # Vercel serverless function (webhook handler)
│   └── exchange-token.js    # Vercel serverless function (OIDC token exchange)
├── lib/
│   ├── bot-handler.js       # Core bot logic
│   ├── oidc.js              # OIDC token verification
│   └── probot-client.js     # Shared Probot instance
├── package.json             # Dependencies and scripts
├── vercel.json              # Vercel configuration
└── README.md                # This file
```

## Prerequisites

- Node.js 18+ (for local development)
- GitHub account with appropriate permissions
- Vercel account (for deployment)
- GitHub App credentials

## Setup Instructions

### 1. Create a GitHub App

1. Go to [GitHub Developer Settings](https://github.com/settings/apps)
2. Click "New GitHub App"
3. Configure the app:
   - **App name**: `HolonBot` (or your preferred name)
   - **Homepage URL**: Your project's homepage
   - **Webhook URL**: `https://your-vercel-domain.vercel.app/api/github-webhook` (placeholder for now)
   - **Webhook secret**: Generate a strong random string (save this!)

### 2. Configure App Permissions

Under **Repository permissions**, set:

| Permission | Access | Purpose |
|------------|--------|---------|
| Issues | Read & Write | Create comments and add labels |
| Pull Requests | Read & Write | Create comments and manage PRs |
| Checks | Read & Write | Handle check runs |
| Metadata | Read | Access repository information |
| Contents | Write | Create initial repository files |

Under **Subscribe to events**, select:
- Pull requests
- Issues
- Pull request reviews
- Repository creation
- Check runs

### 3. Generate and Save App Credentials

1. Save the **App ID** (displayed in the app settings)
2. Generate a **Private Key** and download the `.pem` file
3. Save the **Webhook secret** you generated earlier

### 4. Install the App

1. Install the GitHub App on your target repositories
2. Note which repositories you want the bot to work on

## Local Development

### 1. Clone and Setup

```bash
git clone <your-repo-url>
cd holonbot
npm install
```

### 2. Environment Variables

Create a `.env` file with your GitHub App credentials:

```env
APP_ID=your_app_id_here
PRIVATE_KEY="-----BEGIN RSA PRIVATE KEY-----\n...\n-----END RSA PRIVATE KEY-----\n"
WEBHOOK_SECRET=your_webhook_secret_here
NODE_ENV=development
LOG_LEVEL=debug
```

### 3. Run Locally

```bash
npm run dev
```

The bot will start and be ready to receive webhook events. Use a tool like [ngrok](https://ngrok.com/) to expose your local server to GitHub during development.

```bash
ngrok http 3000
```

Update your GitHub App's webhook URL to use the ngrok URL.

## Deployment to Vercel

### 1. Install Vercel CLI

```bash
npm i -g vercel
```

### 2. Login to Vercel

```bash
vercel login
```

### 3. Deploy

```bash
vercel --prod
```

### 4. Configure Environment Variables in Vercel

Go to your Vercel project dashboard and add these environment variables:

```
APP_ID=your_app_id_here
PRIVATE_KEY=your_private_key_content_here
WEBHOOK_SECRET=your_webhook_secret_here
NODE_ENV=production
LOG_LEVEL=info
```

**Important**: For the `PRIVATE_KEY`, include the entire key content including the `-----BEGIN RSA PRIVATE KEY-----` and `-----END RSA PRIVATE KEY-----` lines, with `\n` characters for line breaks.

### 5. Update GitHub App Webhook URL

1. Go back to your GitHub App settings
2. Update the **Webhook URL** to: `https://your-vercel-domain.vercel.app/api/github-webhook`
3. Click "Save changes"

## Bot Behavior

### Pull Request Events

- **Opened/Reopened**: Bot posts a welcome message with checklist
- **Review Submitted**:
  - Approved: Adds `approved` label
  - Changes Requested: Adds `changes-requested` label

### Issue Events

- **Opened**: Bot posts a welcome message
- **Auto-labeling**: Automatically adds labels based on content:
  - `bug` - for bug reports
  - `enhancement` - for feature requests
  - `question` - for questions

### Repository Events

- **Created**: Sets up initial repository structure with README.md and .gitignore

## Monitoring and Logs

- **Vercel Logs**: Access real-time logs from your Vercel dashboard
- **GitHub App Logs**: Monitor webhook delivery status in GitHub App settings
- **Local Logs**: Use `LOG_LEVEL=debug` for detailed local development logs

## Security Considerations

- ✅ All secrets stored in environment variables
- ✅ Webhook signature verification enabled
- ✅ Minimal permissions requested
- ✅ No sensitive data in codebase
- ✅ HTTPS enforced by Vercel

## Troubleshooting

### Common Issues

1. **403 Forbidden**: Check GitHub App permissions and installation
2. **Webhook delivery failures**: Verify webhook URL and secret
3. **Environment variables missing**: Ensure all required variables are set in Vercel
4. **Function timeouts**: Check Vercel function logs and increase duration if needed

### Debug Commands

```bash
# Test webhook delivery
curl -X POST https://your-app.vercel.app/api/github-webhook \
  -H "Content-Type: application/json" \
  -H "X-GitHub-Event: ping" \
  -H "X-Hub-Signature-256: sha256=..." \
  -d '{"zen": "Non-blocking is better than blocking."}'
```

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Test thoroughly
5. Submit a pull request

## License

MIT License - see LICENSE file for details.

## Support

For issues and questions:
1. Check the troubleshooting section above
2. Review Vercel function logs
3. Check GitHub App delivery logs
4. Open an issue in this repository

---

**Built with ❤️ using [Probot](https://probot.github.io/) and deployed on [Vercel](https://vercel.com/)**