FROM node:20-slim

RUN apt-get update && apt-get install -y \
    git \
    curl \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /usr/share/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | tee /etc/apt/sources.list.d/github-cli.list > /dev/null \
    && apt-get update \
    && apt-get install -y gh \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code@2.0.74

WORKDIR /app

COPY package.json tsconfig.json ./
COPY src ./src
RUN npm install \
    && npm run build \
    && npm prune --omit=dev

RUN mkdir -p /root/.claude
COPY .claude/ /root/.claude/
COPY .claude.json /root/.claude.json

ENV IS_SANDBOX=1

ENTRYPOINT ["node", "/app/dist/adapter.js"]
