/**
 * Holon Publisher Module
 *
 * This module handles publishing Holon execution results back to GitHub PRs.
 * It reads pr-fix.json and summary.md from Holon output and posts:
 * - Per-thread replies to review comments
 * - A PR-level summary comment
 *
 * The module ensures idempotency by tracking posted replies and avoiding duplicates.
 */

import { readFile } from 'fs/promises';
import { join } from 'path';

const SUMMARY_MARKER = '<!-- holon-summary-marker -->';
const BOT_NAME = 'holonbot[bot]';

/**
 * Reads and parses a JSON file
 * @param {string} filePath - Path to the JSON file
 * @returns {Promise<object|null>} Parsed JSON object or null if file doesn't exist
 */
async function readJsonFile(filePath) {
    try {
        const content = await readFile(filePath, 'utf-8');
        return JSON.parse(content);
    } catch (error) {
        if (error.code === 'ENOENT') {
            return null;
        }
        throw error;
    }
}

/**
 * Reads a markdown file content
 * @param {string} filePath - Path to the markdown file
 * @returns {Promise<string|null>} File content or null if file doesn't exist
 */
async function readMarkdownFile(filePath) {
    try {
        return await readFile(filePath, 'utf-8');
    } catch (error) {
        if (error.code === 'ENOENT') {
            return null;
        }
        throw error;
    }
}

/**
 * Checks if a comment is from holonbot
 * @param {object} comment - GitHub comment object
 * @returns {boolean} True if comment is from holonbot
 */
export function isHolonbotComment(comment) {
    return comment.user?.login === BOT_NAME;
}

/**
 * Finds an existing comment from holonbot with the marker
 * @param {object} octokit - Authenticated Octokit instance
 * @param {string} owner - Repository owner
 * @param {string} repo - Repository name
 * @param {number} prNumber - Pull request number
 * @returns {Promise<object|null>} Existing comment or null
 */
async function findExistingSummaryComment(octokit, owner, repo, prNumber) {
    try {
        const comments = await octokit.paginate(
            octokit.rest.issues.listComments,
            {
                owner,
                repo,
                issue_number: prNumber,
                per_page: 100
            }
        );

        // Find the most recent comment from holonbot with our marker
        return comments
            .filter(c => c.body?.includes(SUMMARY_MARKER))
            .sort((a, b) => b.id - a.id)[0] || null;
    } catch (error) {
        console.error('Error listing comments:', error);
        return null;
    }
}

/**
 * Posts a reply to a review comment
 * @param {object} octokit - Authenticated Octokit instance
 * @param {string} owner - Repository owner
 * @param {string} repo - Repository name
 * @param {number} prNumber - Pull request number
 * @param {number} commentId - Review comment ID to reply to
 * @param {string} message - Reply message
 * @returns {Promise<boolean>} True if successful
 */
async function postReviewReply(octokit, owner, repo, prNumber, commentId, message) {
    try {
        await octokit.rest.pulls.createReplyForReviewComment({
            owner,
            repo,
            pull_number: prNumber,
            comment_id: commentId,
            body: message
        });
        return true;
    } catch (error) {
        console.error(`Error posting reply to comment ${commentId}:`, error);
        return false;
    }
}

/**
 * Posts or updates the PR-level summary comment
 * @param {object} octokit - Authenticated Octokit instance
 * @param {string} owner - Repository owner
 * @param {string} repo - Repository name
 * @param {number} prNumber - Pull request number
 * @param {string} summary - Summary content
 * @returns {Promise<boolean>} True if successful
 */
async function postSummaryComment(octokit, owner, repo, prNumber, summary) {
    try {
        // Check for existing summary comment
        const existing = await findExistingSummaryComment(octokit, owner, repo, prNumber);

        const body = `${SUMMARY_MARKER}\n${summary}`;

        if (existing) {
            // Update existing comment
            await octokit.rest.issues.updateComment({
                owner,
                repo,
                comment_id: existing.id,
                body
            });
            console.log(`Updated existing summary comment: ${existing.id}`);
        } else {
            // Create new comment
            await octokit.rest.issues.createComment({
                owner,
                repo,
                issue_number: prNumber,
                body
            });
            console.log('Created new summary comment');
        }
        return true;
    } catch (error) {
        console.error('Error posting summary comment:', error);
        return false;
    }
}

/**
 * Gets all replies for a specific review comment
 * @param {object} octokit - Authenticated Octokit instance
 * @param {string} owner - Repository owner
 * @param {string} repo - Repository name
 * @param {number} prNumber - Pull request number
 * @param {number} commentId - Review comment ID
 * @returns {Promise<Array>} List of replies
 */
async function getCommentReplies(octokit, owner, repo, prNumber, commentId) {
    try {
        // Get all review comments for this PR
        const comments = await octokit.paginate(
            octokit.rest.pulls.listReviewComments,
            {
                owner,
                repo,
                pull_number: prNumber,
                per_page: 100
            }
        );

        // Filter for replies to the specific comment
        // In GitHub API, replies have a `in_reply_to_id` field
        return comments.filter(c => c.in_reply_to_id === commentId);
    } catch (error) {
        console.error(`Error getting replies for comment ${commentId}:`, error);
        return [];
    }
}

/**
 * Checks if holonbot has already replied to a review comment
 * @param {Array} replies - List of existing replies
 * @returns {boolean} True if holonbot has already replied
 */
function hasHolonbotReplied(replies) {
    return replies.some(reply => isHolonbotComment(reply));
}

/**
 * Formats a review reply from pr-fix.json data
 * @param {object} replyData - Reply data from pr-fix.json
 * @returns {string} Formatted reply message
 */
export function formatReviewReply(replyData) {
    const { status, message, action_taken } = replyData;

    const statusEmoji = {
        fixed: '✅',
        wontfix: '⚠️',
        'need-info': '❓'
    }[status] || '📝';

    let reply = `${statusEmoji} **${status.toUpperCase()}**: ${message}`;

    if (action_taken) {
        reply += `\n\n**Action taken**: ${action_taken}`;
    }

    return reply;
}

/**
 * Processes review replies from pr-fix.json
 * @param {object} octokit - Authenticated Octokit instance
 * @param {string} owner - Repository owner
 * @param {string} repo - Repository name
 * @param {number} prNumber - Pull request number
 * @param {Array} reviewReplies - Review replies from pr-fix.json
 * @returns {Promise<object>} Results object with success/failure counts
 */
async function processReviewReplies(octokit, owner, repo, prNumber, reviewReplies) {
    const results = {
        total: reviewReplies.length,
        posted: 0,
        skipped: 0,
        failed: 0,
        details: []
    };

    for (const replyData of reviewReplies) {
        const { comment_id } = replyData;

        // Get existing replies for this comment
        const existingReplies = await getCommentReplies(octokit, owner, repo, prNumber, comment_id);

        // Check if holonbot has already replied
        if (hasHolonbotReplied(existingReplies)) {
            results.skipped++;
            results.details.push({
                comment_id,
                status: 'skipped',
                reason: 'Already replied'
            });
            continue;
        }

        // Format and post the reply
        const message = formatReviewReply(replyData);
        const success = await postReviewReply(octokit, owner, repo, prNumber, comment_id, message);

        if (success) {
            results.posted++;
            results.details.push({
                comment_id,
                status: 'posted'
            });
        } else {
            results.failed++;
            results.details.push({
                comment_id,
                status: 'failed',
                reason: 'API error'
            });
        }
    }

    return results;
}

/**
 * Main function to publish Holon execution results
 * @param {object} context - Probot context object
 * @param {object} options - Configuration options
 * @param {string} options.outputDir - Path to Holon output directory
 * @param {number} options.prNumber - Pull request number
 * @returns {Promise<object>} Results object
 */
export async function publishHolonResults(context, options) {
    const { outputDir, prNumber } = options;
    const { octokit, payload } = context;

    const owner = payload.repository.owner.login;
    const repo = payload.repository.name;

    console.log(`Publishing Holon results for PR #${prNumber} in ${owner}/${repo}`);
    console.log(`Output directory: ${outputDir}`);

    const results = {
        prNumber,
        owner,
        repo,
        summary: { posted: false, error: null },
        reviewReplies: { total: 0, posted: 0, skipped: 0, failed: 0 }
    };

    // Step 1: Read and process pr-fix.json
    const prFixPath = join(outputDir, 'pr-fix.json');
    const prFixData = await readJsonFile(prFixPath);

    if (prFixData && prFixData.review_replies && prFixData.review_replies.length > 0) {
        console.log(`Found ${prFixData.review_replies.length} review replies in pr-fix.json`);

        const replyResults = await processReviewReplies(
            octokit,
            owner,
            repo,
            prNumber,
            prFixData.review_replies
        );

        results.reviewReplies = replyResults;
        console.log(`Review reply results: ${replyResults.posted} posted, ${replyResults.skipped} skipped, ${replyResults.failed} failed`);
    } else {
        console.log('No review replies found in pr-fix.json or file does not exist');
    }

    // Step 2: Read and post summary.md
    const summaryPath = join(outputDir, 'summary.md');
    const summaryContent = await readMarkdownFile(summaryPath);

    if (summaryContent) {
        console.log('Found summary.md, posting to PR...');
        const success = await postSummaryComment(octokit, owner, repo, prNumber, summaryContent);
        results.summary.posted = success;
        if (!success) {
            results.summary.error = 'Failed to post summary comment';
        }
    } else {
        console.log('No summary.md found');
        results.summary.error = 'File not found';
    }

    return results;
}

/**
 * Checks if Holon output exists and is valid
 * @param {string} outputDir - Path to Holon output directory
 * @returns {Promise<boolean>} True if output is valid
 */
export async function hasValidHolonOutput(outputDir) {
    const prFixPath = join(outputDir, 'pr-fix.json');
    const summaryPath = join(outputDir, 'summary.md');

    const prFixExists = await readJsonFile(prFixPath);
    const summaryExists = await readMarkdownFile(summaryPath);

    // At least summary.md should exist for a valid output
    return !!summaryExists || !!prFixExists;
}

export default {
    publishHolonResults,
    hasValidHolonOutput,
    formatReviewReply,
    isHolonbotComment
};
