/**
 * Redaction utility for tool parameters in logs.
 *
 * This module provides functions to redact sensitive information from tool
 * parameters before they are displayed in logs, preventing secret leakage.
 */

/**
 * Sensitive field patterns that should be redacted from tool parameters.
 * These match common names for sensitive fields in tool inputs.
 */
const SENSITIVE_PATTERNS = [
  // Direct keys
  /^token$/i,
  /^access_token$/i,
  /^api_key$/i,
  /^apikey$/i,
  /^secret$/i,
  /^password$/i,
  /^passwd$/i,
  /^authorization$/i,
  /^auth$/i,
  /^bearer$/i,
  /^credential$/i,
  /^cookie$/i,
  /^session(id)?$/i,
  /^session_token$/i,
  /^refresh_token$/i,
  /^auth_token$/i,
  /^private_key$/i,
  /^passphrase$/i,
  // Header patterns (e.g., headers.authorization)
  /^headers\.(authorization|cookie|set-cookie)$/i,
  /^headers\.[xX]-[aA]pi-[kK]ey$/i,
  /^headers\.[xX]-[aA]uth-[tT]oken$/i,
];

/**
 * Patterns for detecting high-entropy strings that might be secrets.
 */
const HIGH_ENTROPY_PREFIXES = [
  "ghp_", "gho_", "ghu_", "ghs_", "ghr_", // GitHub
  "sk_live_", "sk_test_", "sk-", // Stripe
  "anthropic_", // Anthropic
  "hf_", // HuggingFace
  "AKIA", // AWS
  "xoxb-", "xoxp-", // Slack
  "ya29.", // Google OAuth
];

/**
 * Replaces sensitive values with a placeholder string.
 * @param value - The value to potentially redact
 * @param key - The key name (used for pattern matching)
 * @param replacement - The replacement string (default: "***")
 * @returns The original value or replacement string
 */
export function redactValue(value: unknown, key: string, replacement = "***"): unknown {
  // Only redact strings
  if (typeof value !== "string") {
    return value;
  }

  // Check if the key matches a sensitive pattern
  for (const pattern of SENSITIVE_PATTERNS) {
    if (pattern.test(key)) {
      return replacement;
    }
  }

  // Check for known API key/token prefixes
  const lowerValue = value.toLowerCase();
  for (const prefix of HIGH_ENTROPY_PREFIXES) {
    if (value.startsWith(prefix) || lowerValue.startsWith(prefix.toLowerCase())) {
      return replacement;
    }
  }

  // Check for high-entropy strings (heuristic)
  if (looksLikeHighEntropySecret(value)) {
    return replacement;
  }

  return value;
}

/**
 * Redacts sensitive values from an object recursively.
 * @param obj - The object to sanitize
 * @param replacement - The replacement string (default: "***")
 * @returns A new object with sensitive values redacted
 */
export function sanitizeToolInput<T extends Record<string, unknown>>(
  obj: T | undefined,
  replacement = "***"
): Record<string, unknown> | undefined {
  if (!obj || typeof obj !== "object") {
    return undefined;
  }

  const result: Record<string, unknown> = {};

  for (const [key, value] of Object.entries(obj)) {
    if (value === null || value === undefined) {
      continue;
    }

    // Recursively handle nested objects
    if (typeof value === "object" && !Array.isArray(value)) {
      const sanitized = sanitizeToolInput(value as Record<string, unknown>, replacement);
      if (sanitized && Object.keys(sanitized).length > 0) {
        result[key] = sanitized;
      }
      continue;
    }

    // Handle arrays (recursively sanitize objects inside)
    if (Array.isArray(value)) {
      const sanitizedArray: unknown[] = [];
      for (const item of value) {
        if (typeof item === "object" && item !== null) {
          const sanitized = sanitizeToolInput(item as Record<string, unknown>, replacement);
          if (sanitized) {
            sanitizedArray.push(sanitized);
          }
        } else {
          sanitizedArray.push(redactValue(item, key, replacement));
        }
      }
      if (sanitizedArray.length > 0) {
        result[key] = sanitizedArray;
      }
      continue;
    }

    // Redact sensitive values
    const redacted = redactValue(value, key, replacement);
    if (redacted !== null && redacted !== undefined) {
      result[key] = redacted;
    }
  }

  return result;
}

/**
 * Formats tool input parameters for logging in a compact, readable way.
 * @param toolName - Name of the tool being executed
 * @param toolInput - The input parameters to the tool
 * @returns A formatted string representation of the tool input
 */
export function formatToolInputForLog(toolName: string, toolInput: Record<string, unknown> | undefined): string {
  if (!toolInput || Object.keys(toolInput).length === 0) {
    return toolName;
  }

  // Sanitize the input first
  const sanitized = sanitizeToolInput(toolInput);

  // Format based on tool type
  const parts: string[] = [toolName];

  // Tool-specific formatting
  switch (toolName) {
    case "Bash": {
      const cmd = sanitized?.command;
      if (typeof cmd === "string") {
        // Show command, truncated if too long
        parts.push(formatValue("command", cmd, 100));
      }
      break;
    }
    case "Read":
    case "Write":
    case "Edit": {
      if (typeof sanitized?.file_path === "string") {
        parts.push(formatValue("file_path", sanitized.file_path, 80));
      }
      break;
    }
    case "Grep": {
      if (typeof sanitized?.pattern === "string") {
        parts.push(formatValue("pattern", sanitized.pattern, 40));
      }
      if (typeof sanitized?.path === "string") {
        parts.push(formatValue("path", sanitized.path, 60));
      }
      break;
    }
    case "Glob": {
      if (typeof sanitized?.pattern === "string") {
        parts.push(formatValue("pattern", sanitized.pattern, 60));
      }
      break;
    }
    case "HTTP": {
      if (typeof sanitized?.method === "string") {
        parts.push(sanitized.method.toUpperCase());
      }
      if (typeof sanitized?.url === "string") {
        parts.push(formatValue("url", sanitized.url, 80));
      }
      break;
    }
    case "Skill": {
      if (typeof sanitized?.skill === "string") {
        parts.push(formatValue("skill", sanitized.skill, 40));
      }
      if (typeof sanitized?.args === "string") {
        parts.push(formatValue("args", sanitized.args, 60));
      }
      break;
    }
    default: {
      // Generic formatting: show up to 2 key parameters
      if (sanitized) {
        let count = 0;
        const priorityKeys = ["file_path", "pattern", "command", "query", "description", "prompt", "url"];
        for (const key of priorityKeys) {
          if (key in sanitized && count < 2) {
            const value = (sanitized as Record<string, unknown>)[key];
            if (value !== undefined) {
              parts.push(formatValue(key, String(value), 60));
              count++;
            }
          }
        }
      }
    }
  }

  return parts.join(" ");
}

/**
 * Formats a key-value pair for logging.
 * @param key - The parameter name
 * @param value - The parameter value
 * @param maxLength - Maximum length for the value (will be truncated if longer)
 * @returns A formatted string like 'key="value"'
 */
function formatValue(key: string, value: string, maxLength = 80): string {
  const truncated = value.length > maxLength ? value.slice(0, maxLength - 3) + "..." : value;
  // Escape quotes and backslashes
  const escaped = truncated.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  return `${key}="${escaped}"`;
}

/**
 * Checks if a string looks like a high-entropy secret.
 * This is a heuristic to catch things like random API keys.
 */
function looksLikeHighEntropySecret(str: string): boolean {
  // Skip short strings
  if (str.length < 20) {
    return false;
  }

  // Skip things that look like paths or URLs
  if (str.includes("/") || str.includes("\\") || str.startsWith("http")) {
    return false;
  }

  // Calculate Shannon entropy (simplified)
  const freq: Record<string, number> = {};
  for (const ch of str) {
    freq[ch] = (freq[ch] || 0) + 1;
  }

  let entropy = 0;
  const len = str.length;
  for (const count of Object.values(freq)) {
    const p = count / len;
    if (p > 0) {
      entropy -= p * Math.log2(p);
    }
  }

  // High entropy threshold (random-looking strings have entropy > 4.0)
  return entropy > 4.0;
}

/**
 * Checks if a value should be completely omitted from logs (not just redacted).
 * This is for values that are too large or not useful to show.
 */
export function shouldOmitFromLogs(key: string, value: unknown): boolean {
  // Omit very long strings
  if (typeof value === "string" && value.length > 500) {
    return true;
  }

  // Omit large arrays or objects
  if (Array.isArray(value) && value.length > 10) {
    return true;
  }

  // Omit certain keys entirely
  const omitKeys = ["content", "text", "body", "data", "output"];
  return omitKeys.some((omit) => key.toLowerCase() === omit);
}
