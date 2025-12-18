# Use a base image with both Node and Python
FROM node:20-slim

# Install Python, GitHub CLI and essential build tools
RUN apt-get update && apt-get install -y \
    python3 \
    python3-pip \
    python3-venv \
    git \
    curl \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | tee /etc/apt/sources.list.d/github-cli.list > /dev/null \
    && apt-get update \
    && apt-get install -y gh \
    && rm -rf /var/lib/apt/lists/*

# Install Claude Code CLI
RUN npm install -g @anthropic-ai/claude-code@2.0.72

# Set up workspace
WORKDIR /app

# Copy and install Python dependencies
COPY requirements.txt .
RUN pip3 install --no-cache-dir -r requirements.txt --break-system-packages

# Copy the bridge script
COPY adapter.py .

# Pre-seed Claude Code configuration for non-interactive mode
RUN mkdir -p /root/.claude
COPY .claude/ /root/.claude/
COPY .claude.json /root/.claude.json

# Default environment variables
ENV IS_SANDBOX=1
ENV PYTHONUNBUFFERED=1

# Run the adapter
ENTRYPOINT ["python3", "adapter.py"]
