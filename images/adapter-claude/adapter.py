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

    # 2. Context Injection
    context_dir = "/holon/input/context"
    context_header = "\n\n### ADDITIONAL CONTEXT\n"
    context_content = ""
    if os.path.exists(context_dir):
        files = glob.glob(os.path.join(context_dir, "*"))
        for file_path in files:
            if os.path.isfile(file_path):
                file_name = os.path.basename(file_path)
                with open(file_path, 'r') as f:
                    content = f.read()
                    context_content += f"\nFile: {file_name}\n---\n{content}\n---\n"
    
    full_prompt = goal
    if context_content:
        full_prompt += context_header + context_content

    # 3. Preflight: Git Baseline
    workspace_path = "/workspace"
    os.chdir(workspace_path)
    
    has_git = os.path.exists(os.path.join(workspace_path, ".git"))
    if not has_git:
        print("No git repo found in workspace. Initializing temporary baseline...")
        subprocess.run(["git", "init"], check=True)
        subprocess.run(["git", "config", "user.name", "holon-adapter"], check=True)
        subprocess.run(["git", "config", "user.email", "adapter@holon.local"], check=True)
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
            
            print(f"Syncing environment: has_token={bool(auth_token)}, has_base_url={bool(base_url)}")
            
            if auth_token:
                env_section["ANTHROPIC_AUTH_TOKEN"] = auth_token
                env_section["ANTHROPIC_API_KEY"] = auth_token
            if base_url:
                env_section["ANTHROPIC_BASE_URL"] = base_url
            
            settings["env"] = env_section
            
            with open(claude_settings_path, 'w') as f:
                json.dump(settings, f, indent=2)
            print(f"Synced environment to {claude_settings_path}")
        except Exception as e:
            print(f"Warning: Failed to sync Claude settings: {e}")

    from claude_agent_sdk.types import AssistantMessage, TextBlock, ResultMessage
    
    # Options for headless behavior
    options = ClaudeAgentOptions(
        permission_mode="bypassPermissions",
        cwd=workspace_path
    )
    client = ClaudeSDKClient(options=options)
    
    start_time = datetime.now()
    log_file_path = os.path.join(evidence_dir, "execution.log")
    
    success = True
    try:
        print("Connecting to Claude Code...")
        await client.connect()
        print("Session established. Running query...")
        
        # Simple wrapper to capture everything to evidence
        with open(log_file_path, 'w') as log_file:
            # Run the query
            await client.query(full_prompt)
            
            final_output = ""
            async for msg in client.receive_response():
                # print(f"Received message type: {type(msg).__name__}")
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
        
        # Diff Patch: Ensure we see new files
        subprocess.run(["git", "add", "-N", "."], capture_output=True)
        diff_proc = subprocess.run(["git", "diff", "--patch"], capture_output=True, text=True)
        patch_content = diff_proc.stdout
        
        print(f"Generated patch: size={len(patch_content)} characters")
        if len(patch_content) > 0:
            print(f"Patch preview: {patch_content[:200]}")
        
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
            
        with open(os.path.join(output_dir, "summary.md"), 'w') as f:
            f.write(f"# Task Summary\n\nGoal: {goal}\n\nOutcome: {'Success' if success else 'Failure'}\n\n## Actions\n{result}\n")

        print(f"Artifacts written to {output_dir}")
        
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
        sys.exit(1)

if __name__ == "__main__":
    asyncio.run(run_adapter())
