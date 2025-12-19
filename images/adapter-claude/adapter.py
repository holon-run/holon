import os
import sys
import yaml
import json
import asyncio
import subprocess
import glob
from datetime import datetime
from pathlib import Path
from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient

def fix_permissions(directory):
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
        print(f"Fixing permissions for {directory} to {uid}:{gid}")
        
        # Change ownership of the directory itself
        os.chown(directory, uid, gid)
        
        # Recursively change ownership of contents
        for root, dirs, files in os.walk(directory):
            for d in dirs:
                os.chown(os.path.join(root, d), uid, gid)
            for f in files:
                os.chown(os.path.join(root, f), uid, gid)
    except Exception as e:
        print(f"Warning: Failed to fix permissions: {e}")

async def run_adapter():
    print("Holon Claude Adapter Starting...")
    
    # Define output_dir early
    output_dir = "/holon/output"
    os.makedirs(output_dir, exist_ok=True)
    evidence_dir = os.path.join(output_dir, "evidence")
    os.makedirs(evidence_dir, exist_ok=True)
    
    # 1. Load Spec
    spec_path = "/holon/input/spec.yaml"
    if not os.path.exists(spec_path):
        print(f"Error: Spec not found at {spec_path}")
        sys.exit(1)
        
    with open(spec_path, 'r') as f:
        spec = yaml.safe_load(f)
    
    # Handle goal
    goal_val = spec.get('goal', '')
    if isinstance(goal_val, dict):
        goal = goal_val.get('description', '')
    else:
        goal = str(goal_val)
        
    print(f"Task Goal: {goal}")
    
    # Context Processing Removed: handled by Host-side User Prompt compilation

    # CRITICAL: Instruct the agent to stay in /holon/workspace and use relative paths
    
    # Load System Prompt from Host (compiled)
    prompt_path = "/holon/input/prompts/system.md"
    if not os.path.exists(prompt_path):
        print(f"Error: Compiled system prompt not found at {prompt_path}")
        sys.exit(1)
        
    print(f"Loading compiled system prompt from {prompt_path}")
    with open(prompt_path, 'r') as f:
        system_instruction = f.read()

    # Load User Prompt from Host (compiled)
    user_prompt_path = "/holon/input/prompts/user.md"
    if not os.path.exists(user_prompt_path):
        print(f"Error: Compiled user prompt not found at {user_prompt_path}")
        sys.exit(1)
        
    print(f"Loading compiled user prompt from {user_prompt_path}")
    with open(user_prompt_path, 'r') as f:
        user_msg = f.read()

    # 3. Preflight: Git Baseline
    workspace_path = "/holon/workspace"
    os.chdir(workspace_path)
    
    # Force IS_SANDBOX for Claude Code
    os.environ["IS_SANDBOX"] = "1"
    
    # Fix dubious ownership and set global user for git
    subprocess.run(["git", "config", "--global", "--add", "safe.directory", "/holon/workspace"], check=False)
    subprocess.run(["git", "config", "--global", "user.name", "holon-adapter"], check=False)
    subprocess.run(["git", "config", "--global", "user.email", "adapter@holon.local"], check=False)
    
    has_git = os.path.exists(os.path.join(workspace_path, ".git"))
    if not has_git:
        print("No git repo found in workspace. Initializing temporary baseline...")
        subprocess.run(["git", "init"], check=True)
        # Add basic ignores to avoid bloating patches
        with open(".gitignore", "a") as f:
            f.write("\n__pycache__/\n*.pyc\n*.pyo\n.DS_Store\n")
        subprocess.run(["git", "add", "-A"], check=True)
        subprocess.run(["git", "commit", "-m", "holon-baseline"], check=True)
    else:
        print("Existing git repo found. Baseline established.")
    
    # 3.5 Sync Environment to Claude Settings (Wegent style)
    claude_settings_path = os.path.expanduser("~/.claude/settings.json")
    if os.path.exists(claude_settings_path):
        try:
            with open(claude_settings_path, 'r') as f:
                settings = json.load(f)
            
            env_section = settings.get("env", {})
            auth_token = os.environ.get("ANTHROPIC_AUTH_TOKEN") or os.environ.get("ANTHROPIC_API_KEY")
            base_url = os.environ.get("ANTHROPIC_BASE_URL") or os.environ.get("ANTHROPIC_API_URL")
            
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
            print(f"Synced environment to {claude_settings_path}")
        except Exception as e:
            print(f"Warning: Failed to sync Claude settings: {e}")

    from claude_agent_sdk.types import AssistantMessage, TextBlock, ResultMessage, ToolUseBlock
    
    # Append system instructions to Claude Code's default system prompt
    # Using preset="claude_code" preserves Claude's internal tools and instructions
    # append adds our custom rules on top
    options = ClaudeAgentOptions(
        permission_mode="bypassPermissions",
        cwd=workspace_path,
        system_prompt={
            "preset": "claude_code",
            "append": system_instruction
        }
    )
    client = ClaudeSDKClient(options=options)
    
    start_time = datetime.now()
    log_file_path = os.path.join(evidence_dir, "execution.log")
    
    success = True
    result = ""
    try:
        print("Connecting to Claude Code...")
        await client.connect()
        print("Session established. Running query...")
        
        # Simple wrapper to capture everything to evidence
        with open(log_file_path, 'w') as log_file:
            # Run the query with user message only (system prompt is set via options)
            await client.query(user_msg)
            
            final_output = ""
            async for msg in client.receive_response():
                log_file.write(f"Message: {msg}\n")
                
                if isinstance(msg, AssistantMessage):
                    for block in msg.content:
                        if isinstance(block, TextBlock):
                            final_output += block.text
                elif isinstance(msg, ResultMessage):
                    print(f"Task result: {msg.subtype}, is_error: {msg.is_error}")
                    if msg.is_error:
                        success = False
                    break
            
            result = final_output
            log_file.write(f"--- FINAL OUTPUT ---\n{result}\n")

        print(f"Claude Code execution finished. Success: {success}")
        
        # 5. Generate Artifacts
        end_time = datetime.now()
        duration = (end_time - start_time).total_seconds()
        
        # Diff Patch: stage changes so new files are included.
        # We rely on `.gitignore` to exclude transient artifacts (e.g. __pycache__/*.pyc).
        subprocess.run(["git", "add", "-A"], capture_output=True)

        # Generate patch with binary support. `git apply` requires full index lines for binary patches.
        diff_proc = subprocess.run(
            ["git", "diff", "--cached", "--patch", "--binary", "--full-index"],
            capture_output=True,
            text=True,
        )
        patch_content = diff_proc.stdout
            
        print(f"Generated patch: size={len(patch_content)} characters")
        
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
            print("Found user-generated summary.md in /holon/output.")
            with open(summary_out, 'r') as f:
                summary_text = f.read()
        else:
            print("No summary.md found. Falling back to execution log.")
            # Use 'goal' variable from spec parsing, ensuring newlines are handled for display
            display_goal = str(goal).replace('\n', ' ')
            summary_text = f"# Task Summary\n\nGoal: {display_goal}\n\nOutcome: {'Success' if success else 'Failure'}\n\n## Actions\n{result}\n"

        with open(os.path.join(output_dir, "summary.md"), 'w') as f:
            f.write(summary_text)

        print(f"Artifacts written to {output_dir}")
        fix_permissions(output_dir)
        
    except Exception as e:
        print(f"Execution failed: {e}")
        # Write failure manifest
        manifest = {
            "status": "completed",
            "outcome": "failure",
            "error": str(e)
        }
        with open(os.path.join(output_dir, "manifest.json"), 'w') as f:
            json.dump(manifest, f, indent=2)
        fix_permissions(output_dir)
        sys.exit(1)

if __name__ == "__main__":
    asyncio.run(run_adapter())
