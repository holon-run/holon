const appShell = document.querySelector(".app-shell");
const messageList = document.querySelector("[data-message-list]");
const toolList = document.querySelector("[data-tool-list]");
const composer = document.querySelector(".composer");
const composerInput = document.querySelector("[data-composer-input]");
const pageTitle = document.querySelector("[data-page-title]");
const pageSubtitle = document.querySelector("[data-page-subtitle]");
const placeholderTitle = document.querySelector("[data-placeholder-title]");
const placeholderLabel = document.querySelector("[data-placeholder-label]");
const placeholderCopy = document.querySelector("[data-placeholder-copy]");
const panelTitle = document.querySelector("[data-panel-title]");

const placeholderContent = {
  search: {
    title: "Search",
    label: "Global search",
    subtitle: "cross-agent lookup · messages · briefs · work evidence",
    copy:
      "Search is global because it crosses agent boundaries. It should find messages, briefs, work items, tool executions, and memory records without becoming a second conversation list.",
  },
  settings: {
    title: "Settings",
    label: "Runtime configuration",
    subtitle: "local connection · providers · model defaults",
    copy:
      "Settings owns runtime configuration and local/remote connection details. The bottom-left connection chip is the compact runtime posture.",
  },
};

const messages = [
  {
    role: "assistant",
    id: "turn_915_917_context",
    turn: "915-917",
    time: "10:57",
    text: [
      "Earlier context:",
      "",
      "- turn 915: 讨论 Figma Dev seat / Full seat 是否够 Holon Web GUI 使用。",
      "- turn 916: 判断 code-first HTML prototype 比先上 Figma 更适合第一阶段。",
      "- turn 917: 确认先写轻量 DESIGN.md，避免 agent 输出普通 SaaS dashboard。",
    ].join("\n"),
  },
  {
    role: "operator",
    id: "msg_5028c856a38d06f",
    turn: 918,
    time: "11:18",
    text: "这样吧，我们不建立 issue 了，直接实现 web gui的初始化，",
  },
  {
    role: "assistant",
    id: "brief_06f488c0317adce",
    turn: 918,
    time: "11:18",
    text: "已暂停，不会继续实现。等你下一步指示。",
  },
  {
    role: "operator",
    id: "msg_4e292547aac6a3c",
    turn: 918,
    time: "11:18",
    text: "先等一下",
  },
  {
    role: "operator",
    id: "msg_00c022e3b649fe4",
    turn: 919,
    time: "11:19",
    text: "这样吧，我们不建立 issue 了，直接实现 web gui的初始化，design.md，以及页面的 prototype。你直接拆分拆 workitem，在同一个 worktree 里完成，完成后我本地先看看",
  },
  {
    role: "assistant",
    id: "brief_086335d5e7475e1",
    turn: 919,
    time: "11:26",
    text: [
      "已完成第一版 Web GUI 初始化，放在同一个隔离 worktree 里，方便你本地先看。",
      "",
      "Worktree 路径：",
      "```bash",
      "/Users/jolestar/opensource/src/github.com/holon-run/.holon-worktrees-holon/web-gui-prototype",
      "```",
      "",
      "Prototype 里已经包含 Workspace Dashboard、Agent Detail、Work Item Detail、Settings / local connection，以及 Info / Verbose / Debug display level。",
      "",
      "已验证：git diff --check、HTML 引用资源和导航面板一致性检查、node --check、以及本地 HTTP server curl 首页成功。Work item 也已标记完成。",
    ].join("\n"),
    tools: true,
  },
];

const tools = [
  ["work", "CreateWorkItem", "success", "13ms", "tool_28adf31b549485c"],
  ["work", "PickWorkItem", "success", "15ms", "tool_4380deabbfbe8f5"],
  ["workspace", "UseWorkspace", "success", "373ms", "tool_bf2f9a7e9370dd4"],
  ["tool", "ExecCommandBatch", "success", "373ms", "repo inspection · 4/4"],
  ["tool", "ExecCommandBatch", "success", "199ms", "HTTP/static docs · 4/4"],
  ["tool", "ExecCommandBatch", "success", "109ms", "server route probe · 1/2"],
  ["tool", "ExecCommandBatch", "success", "160ms", "repo shape · 3/3"],
  ["work", "UpdateWorkItem", "success", "14ms", "todo -> in_progress"],
  ["tool", "ApplyPatch", "success", "4ms", "patched 5 prototype files"],
  ["work", "UpdateWorkItem", "success", "13ms", "verification in_progress"],
  ["tool", "ExecCommandBatch", "success", "1517ms", "static validation · 5/5"],
  ["tool", "ExecCommandBatch", "success", "230ms", "file preview · 3/3"],
  ["work", "UpdateWorkItem", "success", "16ms", "todo -> completed"],
  ["work", "CompleteWorkItem", "success", "44ms", "work item completed"],
];

function setView(view, options = {}) {
  appShell.dataset.appView = view;
  if (view !== "agent") {
    setPanel(false);
  }

  document.querySelectorAll("[data-nav]").forEach((button) => {
    button.classList.toggle("is-active", button.dataset.nav === (view === "agent" ? "" : options.nav ?? view));
  });

  if (view === "dashboard") {
    pageTitle.textContent = "Dashboard";
    pageSubtitle.textContent = "2 agents · 1 waiting signal";
    return;
  }

  if (view === "agent") {
    const agentId = options.agentId ?? "holon-pm";
    pageTitle.textContent = agentId;
    pageSubtitle.textContent =
      "asleep · workspace holon · /Users/jolestar/opensource/src/github.com/holon-run/.holon-worktrees-holon/web-gui-prototype";
    setSelectedAgent(agentId);
    return;
  }

  const content = placeholderContent[options.nav] ?? placeholderContent.search;
  pageTitle.textContent = content.title;
  pageSubtitle.textContent = content.subtitle;
  placeholderTitle.textContent = content.title;
  placeholderLabel.textContent = content.label;
  placeholderCopy.textContent = content.copy;
}

function setPanel(open, kind = "workitem") {
  appShell.dataset.panel = open ? "open" : "closed";
  if (!open) return;

  const labels = {
    workitem: "WorkItem detail",
    model: "Model settings",
    diff: "Diff preview",
    file: "File preview",
    web: "Web preview",
  };
  panelTitle.textContent = labels[kind] ?? labels.workitem;

  document.querySelectorAll("[data-panel-kind]").forEach((button) => {
    button.classList.toggle("is-active", button.dataset.panelKind === kind);
  });
  document.querySelectorAll("[data-panel-section]").forEach((section) => {
    section.hidden = section.dataset.panelSection !== kind;
  });
}

function setSelectedAgent(agentId) {
  document.querySelectorAll("[data-open-agent]").forEach((element) => {
    element.classList.toggle("is-selected", element.dataset.openAgent === agentId);
  });
}

function renderMessages() {
  messageList.replaceChildren(...messages.map(renderMessage));
}

function renderMessage(message) {
  const article = document.createElement("article");
  article.className = `message ${message.role}`;

  const bubble = document.createElement("div");
  bubble.className = "bubble";
  bubble.innerHTML = renderText(message.text);
  article.append(bubble);

  const meta = document.createElement("div");
  meta.className = "message-meta";
  meta.innerHTML = `<time>${escapeHtml(message.time)}</time><span>turn ${message.turn}</span>`;
  if (message.role === "assistant") {
    const copy = document.createElement("button");
    copy.type = "button";
    copy.className = "copy-action";
    copy.setAttribute("aria-label", "Copy message");
    copy.title = "Copy";
    copy.textContent = "⧉";
    copy.addEventListener("click", () => copyMessageText(copy, message.text));
    meta.append(copy);
  }
  article.append(meta);

  if (message.tools) {
    const strip = document.createElement("div");
    strip.className = "tool-strip";
    tools.slice(0, 6).forEach(([kind, name, status, duration, detail]) => {
      const row = document.createElement("button");
      row.type = "button";
      row.innerHTML = `<span class="tool-kind ${escapeHtml(kind)}">${escapeHtml(kind)}</span><strong>${escapeHtml(name)}</strong><small>${escapeHtml(status)} · ${escapeHtml(duration)} · ${escapeHtml(detail)}</small>`;
      strip.append(row);
    });
    article.append(strip);
  }

  return article;
}

async function copyMessageText(button, text) {
  const previous = button.textContent;
  try {
    await navigator.clipboard.writeText(text);
    button.textContent = "✓";
  } catch {
    button.textContent = "!";
  }
  window.setTimeout(() => {
    button.textContent = previous;
  }, 1200);
}

function renderTools() {
  toolList.replaceChildren(
    ...tools.slice(0, 8).map(([kind, name, status, duration, detail]) => {
      const item = document.createElement("div");
      item.className = `tool-item ${escapeHtml(kind)}`;
      item.innerHTML = `<strong>${escapeHtml(name)}</strong><small>${escapeHtml(kind)} · ${escapeHtml(status)} · ${escapeHtml(duration)} · ${escapeHtml(detail)}</small>`;
      return item;
    }),
  );
}

function renderText(text) {
  const lines = text.split(/\r?\n/);
  const html = [];
  let code = [];
  let inCode = false;
  let paragraph = [];

  function flushParagraph() {
    if (!paragraph.length) return;
    html.push(`<p>${inline(paragraph.join(" "))}</p>`);
    paragraph = [];
  }

  function flushCode() {
    html.push(`<pre><code>${escapeHtml(code.join("\n"))}</code></pre>`);
    code = [];
  }

  lines.forEach((line) => {
    if (line.startsWith("```")) {
      flushParagraph();
      if (inCode) flushCode();
      inCode = !inCode;
      return;
    }
    if (inCode) {
      code.push(line);
      return;
    }
    if (!line.trim()) {
      flushParagraph();
      return;
    }
    paragraph.push(line.trim());
  });
  flushParagraph();
  if (inCode) flushCode();
  return html.join("");
}

function inline(value) {
  return escapeHtml(value).replace(/(https?:\/\/[^\s<]+)/g, '<a href="$1" target="_blank" rel="noreferrer">$1</a>');
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

document.querySelectorAll("[data-nav]").forEach((button) => {
  button.addEventListener("click", () => {
    const nav = button.dataset.nav;
    if (nav === "dashboard") {
      setView("dashboard", { nav });
      return;
    }
    setView("placeholder", { nav });
  });
});

document.querySelectorAll("[data-open-agent]").forEach((element) => {
  element.addEventListener("click", (event) => {
    event.stopPropagation();
    setView("agent", { agentId: element.dataset.openAgent });
  });

  element.addEventListener("keydown", (event) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    setView("agent", { agentId: element.dataset.openAgent });
  });
});

document.querySelector("[data-toggle-nav]").addEventListener("click", () => {
  const collapsed = appShell.dataset.navCollapsed === "true";
  appShell.dataset.navCollapsed = collapsed ? "false" : "true";
});

document.querySelector("[data-toggle-panel]").addEventListener("click", () => {
  setPanel(appShell.dataset.panel !== "open", "workitem");
});

document.querySelector("[data-close-panel]").addEventListener("click", () => {
  setPanel(false);
});

document.querySelectorAll("[data-open-panel]").forEach((button) => {
  button.addEventListener("click", () => {
    setPanel(true, button.dataset.openPanel);
  });
});

document.querySelectorAll("[data-panel-kind]").forEach((button) => {
  button.addEventListener("click", () => {
    setPanel(true, button.dataset.panelKind);
  });
});

composer.addEventListener("submit", (event) => {
  event.preventDefault();
  const text = composerInput.value.trim();
  if (!text) return;
  messages.push({
    role: "operator",
    id: `local_${Date.now()}`,
    turn: "draft",
    time: new Intl.DateTimeFormat("en", { hour: "2-digit", minute: "2-digit", hour12: false }).format(new Date()),
    text,
  });
  composerInput.value = "";
  renderMessages();
  document.querySelector(".agent-page").scrollTop = document.querySelector(".agent-page").scrollHeight;
});

renderMessages();
renderTools();
if (location.hash === "#thread") {
  setView("agent", { agentId: "holon-pm" });
} else {
  setView("dashboard", { nav: "dashboard" });
}
