import os
import sys

# Set debug logging early so the SDK picks it up upon import
if os.environ.get("LOG_LEVEL", "").lower() == "debug":
    os.environ["ANTHROPIC_LOG"] = "debug"

import yaml
import json
import asyncio
import glob
import time
from datetime import datetime
from pathlib import Path
from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient
from enum import Enum

# Import testable helper modules
from config import HolonConfig
from subprocess_helpers import subprocess_runner
from fs_helpers import fs_operations, permission_fixer
from network_helpers import network_client

class LogLevel(Enum):
    DEBUG = "debug"
    INFO = "info"
    PROGRESS = "progress"
    MINIMAL = "minimal"

class ProgressLogger:
    def __init__(self, log_level="progress"):
        self.log_level = LogLevel(log_level.lower())
        self.tool_use_count = 0

    def _should_log(self, level):
        level_priority = {
            LogLevel.DEBUG: 0,
            LogLevel.INFO: 1,
            LogLevel.PROGRESS: 2,
            LogLevel.MINIMAL: 3
        }
        return level_priority[level] >= level_priority[self.log_level]

    def debug(self, message):
        if self._should_log(LogLevel.DEBUG):
            print(f"[DEBUG] {message}", flush=True)

    def info(self, message):
        if self._should_log(LogLevel.INFO):
            print(f"[INFO] {message}", flush=True)

    def progress(self, message):
        if self._should_log(LogLevel.PROGRESS):
            print(f"[PROGRESS] {message}", flush=True)

    def minimal(self, message):
        if self._should_log(LogLevel.MINIMAL):
            print(f"[PHASE] {message}", flush=True)

    def log_tool_use(self, tool_name, files_touched=None, file_count=None):
        """Safely log tool use without exposing content"""
        self.tool_use_count += 1

        if self._should_log(LogLevel.PROGRESS):
            if files_touched:
                safe_files = [self._safe_filename(f) for f in files_touched if f]
                count_info = f"{len(safe_files)} files"
                if len(safe_files) <= 3:  # Show few files in progress mode
                    safe_list = ", ".join(safe_files)
                    print(f"[TOOL] {tool_name} → {safe_list} ({count_info})")
                else:
                    print(f"[TOOL] {tool_name} → {count_info}")
            elif file_count:
                print(f"[TOOL] {tool_name} → {file_count} items")
            else:
                print(f"[TOOL] {tool_name}")

    def _safe_filename(self, filepath):
        """Return a safe representation of filename without exposing content"""
        if not filepath:
            return "unknown"
        # Just show filename, not full path or content
        return os.path.basename(filepath)

    def log_phase(self, phase_name):
        """Log high-level phase"""
        self.minimal(f"Starting: {phase_name}")

    def log_outcome(self, success, duration, error=None):
        """Log final outcome"""
        outcome = "SUCCESS" if success else "FAILURE"
        self.minimal(f"Outcome: {outcome} (duration: {duration:.1f}s)")
        if error and self._should_log(LogLevel.INFO):
            self.info(f"[ERROR] {error}")

    def log_summary_excerpt(self, summary_path, lines=5):
        """Print first N lines of summary to workflow logs"""
        try:
            if os.path.exists(summary_path):
                with open(summary_path, 'r') as f:
                    summary_lines = f.readlines()
                    self.minimal("=== SUMMARY EXCERPT ===")
                    for i, line in enumerate(summary_lines[:lines]):
                        self.minimal(f"{i+1:2d}: {line.rstrip()}")
                    if len(summary_lines) > lines:
                        self.minimal(f"... and {len(summary_lines) - lines} more lines")
                    self.minimal("=== END SUMMARY ===")
            else:
                self.info("[WARNING] Summary file not found")
        except Exception as e:
            self.info(f"[WARNING] Failed to read summary: {e}")

def generate_fallback_summary(goal: str, success: bool, result: str) -> str:
    """
    Generate a fallback summary when the agent fails to produce one.
    Wraps the execution log in a collapsible details block.
    """
    outcome = 'Success' if success else 'Failure'
    return f"# Task Summary\n\nGoal: {goal}\n\nOutcome: {outcome}\n\n## Actions\n<details><summary>Click to see full execution log</summary>\n\n{result}\n</details>\n"


def fix_permissions(directory, logger=None):
    """
    Recursively change ownership of the directory and its contents
    to the HOST_UID and HOST_GID provided in environment variables.
    """
    uid_str = os.environ.get("HOST_UID")
    gid_str = os.environ.get("HOST_GID")

    if not uid_str or not gid_str:
        return

    try:
        uid = int(uid_str)
        gid = int(gid_str)
        if logger:
            logger.debug(f"Fixing permissions for {directory} to {uid}:{gid}")

        # Change ownership of the directory itself
        os.chown(directory, uid, gid)

        # Recursively change ownership of contents
        for root, dirs, files in os.walk(directory):
            for d in dirs:
                os.chown(os.path.join(root, d), uid, gid)
            for f in files:
                os.chown(os.path.join(root, f), uid, gid)
    except Exception as e:
        if logger:
            logger.info(f"Warning: Failed to fix permissions: {e}")
        else:
            # Fallback when no logger is available
            import sys
            print(f"Warning: Failed to fix permissions: {e}", file=sys.stderr)

def _int_env(name, default):
    value = os.environ.get(name)
    if value is None or value == "":
        return default
    try:
        return int(value)
    except ValueError:
        return default

async def run_adapter():
    # Initialize configuration
    config = HolonConfig()
    config.ensure_directories()

    # Get log level from configuration
    log_level = config.get_env_log_level()
    logger = ProgressLogger(log_level)

    # Note: ANTHROPIC_LOG set at top of file

    # Early heartbeat to confirm container started
    print("Holon Claude Adapter process started...", flush=True)
    logger.minimal("Holon Claude Adapter Starting...")
    
    # 1. Load Spec
    logger.log_phase("Loading specification")
    if not fs_operations.exists(config.spec_path):
        logger.minimal(f"Error: Spec not found at {config.spec_path}")
        sys.exit(1)

    spec = fs_operations.read_yaml(config.spec_path)

    # Handle goal
    goal_val = spec.get('goal', '')
    if isinstance(goal_val, dict):
        goal = goal_val.get('description', '')
    else:
        goal = str(goal_val)
    logger.info(f"Task Goal: {goal}")

    # Context Processing: handled by Host-side User Prompt compilation

    # CRITICAL: Instruct the agent to stay in /holon/workspace and use relative paths
    
    # Load System Prompt from Host (compiled)
    if not fs_operations.exists(config.system_prompt_path):
        logger.minimal(f"Error: Compiled system prompt not found at {config.system_prompt_path}")
        sys.exit(1)

    logger.info(f"Loading compiled system prompt from {config.system_prompt_path}")
    system_instruction = fs_operations.read_text(config.system_prompt_path)

    # Load User Prompt from Host (compiled)
    if not fs_operations.exists(config.user_prompt_path):
        logger.minimal(f"Error: Compiled user prompt not found at {config.user_prompt_path}")
        sys.exit(1)

    logger.info(f"Loading compiled user prompt from {config.user_prompt_path}")
    user_msg = fs_operations.read_text(config.user_prompt_path)

    # 3. Preflight: Git Baseline
    logger.log_phase("Setting up git workspace")
    workspace_path = str(config.workspace_dir)
    fs_operations.chdir(workspace_path)

    # Force IS_SANDBOX for Claude Code
    os.environ["IS_SANDBOX"] = "1"

    # Fix dubious ownership and set global user for git
    logger.debug("Configuring git")
    subprocess_runner.run_git_config(workspace_path)

    has_git = fs_operations.exists(config.workspace_dir / ".git")
    if not has_git:
        logger.info("No git repo found in workspace. Initializing temporary baseline...")
        subprocess_runner.run_git_init()
        # Add basic ignores to avoid bloating patches
        fs_operations.append_text(Path(".gitignore"), "\n__pycache__/\n*.pyc\n*.pyo\n.DS_Store\n")
        subprocess_runner.run_git_add()
        subprocess_runner.run_git_commit("holon-baseline")
        logger.log_tool_use("GitInit")
    else:
        logger.info("Existing git repo found. Baseline established.")
    
    # 3.5 Sync Environment to Claude Settings (Wegent style)
    logger.log_phase("Configuring Claude environment")
    
    # Extract environment variables once for reuse
    auth_token = os.environ.get("ANTHROPIC_AUTH_TOKEN") or os.environ.get("ANTHROPIC_API_KEY")
    base_url = os.environ.get("ANTHROPIC_BASE_URL") or os.environ.get("ANTHROPIC_API_URL") or "https://api.anthropic.com"
    
    claude_settings_path = os.path.expanduser("~/.claude/settings.json")
    if os.path.exists(claude_settings_path):
        try:
            with open(claude_settings_path, 'r') as f:
                settings = json.load(f)

            env_section = settings.get("env", {})

            if auth_token:
                env_section["ANTHROPIC_AUTH_TOKEN"] = auth_token
                env_section["ANTHROPIC_API_KEY"] = auth_token
            if base_url:
                env_section["ANTHROPIC_BASE_URL"] = base_url
                env_section["ANTHROPIC_API_URL"] = base_url
                env_section["CLAUDE_CODE_API_URL"] = base_url

            env_section["IS_SANDBOX"] = "1"
            settings["env"] = env_section

            with open(claude_settings_path, 'w') as f:
                json.dump(settings, f, indent=2)
            logger.debug("Synced environment to Claude settings")
        except Exception as e:
            logger.debug(f"Failed to sync Claude settings: {e}")
    
    # Diagnostics: Check environment and connectivity early
    logger.minimal(f"Checking environment: ANTHROPIC_API_KEY present: {bool(auth_token)}")
    logger.minimal(f"Testing connectivity to {base_url}...")
    try:
        import urllib.request
        # Simple heartbeat check to base URL
        with urllib.request.urlopen(base_url, timeout=10) as response:
            logger.minimal(f"Connectivity test: HTTP {response.status} (OK)")
    except Exception as net_err:
        logger.minimal(f"Warning: Connectivity test failed/timed out: {net_err}")

    from claude_agent_sdk.types import AssistantMessage, TextBlock, ResultMessage, ToolUseBlock
    
    # Optional model override for deterministic CI.
    model = os.environ.get("HOLON_MODEL")
    fallback_model = os.environ.get("HOLON_FALLBACK_MODEL")

    # Append system instructions to Claude Code's default system prompt
    # Using preset="claude_code" preserves Claude's internal tools and instructions
    # append adds our custom rules on top
    options = ClaudeAgentOptions(
        permission_mode="bypassPermissions",
        cwd=workspace_path,
        system_prompt={
            "preset": "claude_code",
            "append": system_instruction
        },
        model=model,
        fallback_model=fallback_model,
    )
    client = ClaudeSDKClient(options=options)
    
    start_time = datetime.now()
    log_file_path = os.path.join(evidence_dir, "execution.log")

    success = True
    result = ""
    try:
        logger.log_phase("Running AI execution")
        logger.minimal("Connecting to Claude Code...")
        await client.connect()
        logger.minimal("Session established. Running query...")

        # Simple wrapper to capture everything to evidence
        with open(log_file_path, 'w') as log_file:
            logger.minimal("Executing query...")
            # Run the query with user message only (system prompt is set via options)
            query_timeout_s = _int_env("HOLON_QUERY_TIMEOUT_SECONDS", 300)
            if query_timeout_s > 0:
                await asyncio.wait_for(client.query(user_msg), timeout=query_timeout_s)
            else:
                await client.query(user_msg)
            logger.minimal("Query sent. Waiting for response stream...")

            final_output = ""
            msg_count = 0
            response_stream = client.receive_response()
            heartbeat_s = _int_env("HOLON_HEARTBEAT_SECONDS", 60)
            idle_timeout_s = _int_env("HOLON_RESPONSE_IDLE_TIMEOUT_SECONDS", 1800)
            total_timeout_s = _int_env("HOLON_RESPONSE_TOTAL_TIMEOUT_SECONDS", 7200)
            start_time_mono = time.monotonic()
            last_msg_time = start_time_mono

            if heartbeat_s > 0:
                logger.minimal(
                    "Response stream heartbeat enabled: "
                    f"interval={heartbeat_s}s idle_timeout={idle_timeout_s}s total_timeout={total_timeout_s}s"
                )

            while True:
                try:
                    if heartbeat_s > 0:
                        msg = await asyncio.wait_for(response_stream.__anext__(), timeout=heartbeat_s)
                    else:
                        msg = await response_stream.__anext__()
                except StopAsyncIteration:
                    break
                except asyncio.TimeoutError:
                    now = time.monotonic()
                    idle_for = now - last_msg_time
                    total_for = now - start_time_mono
                    logger.minimal(
                        f"No response yet (idle {idle_for:.0f}s, total {total_for:.0f}s)..."
                    )
                    if idle_timeout_s > 0 and idle_for >= idle_timeout_s:
                        raise TimeoutError(
                            f"No response for {idle_for:.0f}s (idle timeout {idle_timeout_s}s)"
                        )
                    if total_timeout_s > 0 and total_for >= total_timeout_s:
                        raise TimeoutError(
                            f"Response stream exceeded {total_timeout_s}s total timeout"
                        )
                    continue

                last_msg_time = time.monotonic()
                msg_count += 1
                # Always log the message type for progress visibility
                msg_type = type(msg).__name__
                if msg_type == "ResultMessage":
                    logger.debug(f"Received message #{msg_count}: {msg_type}")
                
                log_file.write(f"Message: {msg}\n")
                log_file.flush() # Ensure we don't lose logs on hang

                if isinstance(msg, AssistantMessage):
                    for block in msg.content:
                        if isinstance(block, TextBlock):
                            final_output += block.text
                        elif isinstance(block, ToolUseBlock):
                            # Safe tool use logging - only log tool name, not parameters
                            tool_name = getattr(block, 'name', 'UnknownTool')
                            logger.log_tool_use(tool_name)
                elif isinstance(msg, ResultMessage):
                    logger.info(f"Task result received: {msg.subtype}, is_error: {msg.is_error}")
                    if msg.is_error:
                        success = False
                    break

            result = final_output
            log_file.write(f"--- FINAL OUTPUT ---\n{result}\n")

        logger.progress(f"Claude Code execution finished. Success: {success}")
        
        # 5. Generate Artifacts
        logger.log_phase("Generating artifacts")
        end_time = datetime.now()
        duration = (end_time - start_time).total_seconds()

        # Diff Patch: stage changes so new files are included.
        # We rely on `.gitignore` to exclude transient artifacts (e.g. __pycache__/*.pyc).
        logger.progress("Staging changes for diff")
        subprocess.run(["git", "add", "-A"], capture_output=True)

        # Generate patch with binary support. `git apply` requires full index lines for binary patches.
        logger.progress("Generating patch file")
        diff_proc = subprocess.run(
            ["git", "diff", "--cached", "--patch", "--binary", "--full-index"],
            capture_output=True,
            text=True,
        )
        patch_content = diff_proc.stdout

        logger.progress(f"Generated patch: {len(patch_content)} characters")
        
        # Manifest
        manifest = {
            "metadata": {
                "adapter": "claude-code",
                "version": "0.1.0"
            },
            "status": "completed",
            "outcome": "success" if success else "failure",
            "duration": duration,
            "artifacts": [
                {"name": "diff.patch", "path": "diff.patch"},
                {"name": "summary.md", "path": "summary.md"},
                {"name": "evidence", "path": "evidence/"}
            ]
        }
        
        # Write Files
        with open(os.path.join(output_dir, "manifest.json"), 'w') as f:
            json.dump(manifest, f, indent=2)
            
        with open(os.path.join(output_dir, "diff.patch"), 'w') as f:
            f.write(patch_content)
            
        # Check for summary.md in /holon/output (generated by agent)
        summary_out = os.path.join(output_dir, "summary.md")
        
        if os.path.exists(summary_out):
            logger.info("Found user-generated summary.md in /holon/output.")
            with open(summary_out, 'r') as f:
                summary_text = f.read()
        else:
            logger.info("No summary.md found. Falling back to execution log.")
            summary_text = generate_fallback_summary(goal, success, result)

        with open(os.path.join(output_dir, "summary.md"), 'w') as f:
            f.write(summary_text)

        logger.progress(f"Artifacts written to {output_dir}")
        fix_permissions(output_dir, logger)

        # Log summary excerpt for CI visibility
        summary_path = os.path.join(output_dir, "summary.md")
        logger.log_summary_excerpt(summary_path)

        # Log final outcome
        logger.log_outcome(success, duration)
        
    except Exception as e:
        logger.progress(f"Execution failed: {e}")
        logger.debug(f"Exception details: {type(e).__name__}: {e}")

        # Log failure outcome
        end_time = datetime.now()
        duration = (end_time - start_time).total_seconds()
        logger.log_outcome(False, duration, error=str(e))

        # Write failure manifest
        manifest = {
            "status": "completed",
            "outcome": "failure",
            "error": str(e)
        }
        with open(os.path.join(output_dir, "manifest.json"), 'w') as f:
            json.dump(manifest, f, indent=2)
        fix_permissions(output_dir, logger)
        sys.exit(1)

if __name__ == "__main__":
    asyncio.run(run_adapter())
