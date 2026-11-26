// Generate navigation from the same frontmatter and ordering as directory indexes,
// with concise sidebar titles and logical workflow subgroups for large sections.
import { readFile, readdir, writeFile } from "node:fs/promises";
import matter from "gray-matter";

const root = new URL("./dist/content/", import.meta.url);
const sections = [
  ["getting-started", "Getting started", "开始使用"],
  ["concepts", "Core concepts", "核心概念"],
  ["guides", "Task guides", "任务指南"],
  ["reference", "Reference", "配置与参考"],
];

const SHORT_TITLES = {
  en: {
    "/getting-started/": "Overview & quickstart",
    "/concepts/": "Overview",
    "/guides/": "All guides",
    "/reference/": "Overview",
    "/guides/customize-agent": "Customize roles & permissions",
    "/guides/use-skills": "Install & verify skills",
    "/guides/work-with-workspaces": "Workspaces & Git worktrees",
    "/guides/run-long-lived-task": "Run long-lived tasks",
    "/guides/delegate-work": "Delegate work to subagents",
    "/guides/build-agent-ecosystem-researcher": "Multi-repo ecosystem researcher",
    "/guides/build-research-steward": "Traceable research briefs",
    "/guides/build-docs-steward": "Build a Docs Steward",
    "/guides/build-code-reviewer": "Build a PR Code Reviewer",
    "/guides/organize-agent-skills-and-roles": "Isolate skills across roles",
    "/guides/run-github-task": "Solve GitHub issues & PRs",
    "/guides/connect-external-events": "Connect external webhooks",
    "/guides/connect-event-inbox": "Unify events with Agent Inbox",
    "/guides/schedule-agent-work": "Schedule recurring checks",
    "/guides/configure-models": "Configure & switch models",
    "/guides/run-offline-private-agent": "Offline & private environments",
    "/guides/manage-agent-memory": "Manage agent memory",
    "/guides/google-workspace-oidc-authentication": "OIDC with Google Workspace",
    "/guides/gitlab-ce-oidc-authentication": "OIDC with GitLab CE",
    "/guides/keycloak-oidc-authentication": "OIDC with Keycloak",
    "/guides/casdoor-oidc-authentication": "OIDC with Casdoor",
    "/guides/glitchtip-error-alerts": "Wake on GlitchTip alerts",
    "/guides/changedetection-webhook-setup": "Wake on web page changes",
    "/guides/searxng-websearch-provider": "Self-hosted SearXNG search",
    "/guides/host-local-apps": "Build & host Local Apps",
    "/guides/observe-agent-work": "Observe & verify progress",
    "/guides/troubleshooting": "Troubleshoot stuck tasks",
  },
  "zh-CN": {
    "/zh-CN/getting-started/": "概览与快速开始",
    "/zh-CN/concepts/": "概念总览",
    "/zh-CN/guides/": "全部指南",
    "/zh-CN/reference/": "参考总览",
    "/zh-CN/guides/create-agent": "从模板创建首个 Agent",
    "/zh-CN/guides/customize-agent": "定制角色与权限边界",
    "/zh-CN/guides/use-skills": "发现、启用与验证技能",
    "/zh-CN/guides/work-with-workspaces": "工作区与 Git Worktree",
    "/zh-CN/guides/run-long-lived-task": "长周期任务与跨会话恢复",
    "/zh-CN/guides/delegate-work": "委派子 Agent 并验收",
    "/zh-CN/guides/build-agent-ecosystem-researcher": "多仓库生态调研员",
    "/zh-CN/guides/build-research-steward": "可溯源调研与决策简报",
    "/zh-CN/guides/build-docs-steward": "搭建本地 Docs Steward",
    "/zh-CN/guides/build-code-reviewer": "搭建持续跟进 PR 的 Reviewer",
    "/zh-CN/guides/organize-agent-skills-and-roles": "按角色隔离 Agent 技能",
    "/zh-CN/guides/run-github-task": "处理 GitHub Issue 与 PR",
    "/zh-CN/guides/connect-external-events": "配置外部 Webhook 事件",
    "/zh-CN/guides/connect-event-inbox": "用 Agent Inbox 收口事件",
    "/zh-CN/guides/schedule-agent-work": "定时巡检与周期任务",
    "/zh-CN/guides/configure-models": "配置与切换大模型",
    "/zh-CN/guides/run-offline-private-agent": "离线与自托管私有部署",
    "/zh-CN/guides/manage-agent-memory": "管理与核验 Agent 记忆",
    "/zh-CN/guides/google-workspace-oidc-authentication": "Google Workspace OIDC",
    "/zh-CN/guides/gitlab-ce-oidc-authentication": "自托管 GitLab CE OIDC",
    "/zh-CN/guides/keycloak-oidc-authentication": "自托管 Keycloak OIDC",
    "/zh-CN/guides/casdoor-oidc-authentication": "自托管 Casdoor OIDC",
    "/zh-CN/guides/changedetection-webhook-setup": "网页变更唤醒 (changedetection)",
    "/zh-CN/guides/glitchtip-error-alerts": "错误告警唤醒 (GlitchTip)",
    "/zh-CN/guides/searxng-websearch-provider": "自托管 SearXNG 搜索后端",
    "/zh-CN/guides/host-local-apps": "搭建与托管 Local App",
    "/zh-CN/guides/observe-agent-work": "观察、追踪与验收进展",
    "/zh-CN/guides/troubleshooting": "排查卡住或无响应任务",
  },
};

const GUIDE_SUBGROUPS = [
  {
    en: "Roles & skills",
    zh: "角色与技能",
    slugs: new Set([
      "create-agent",
      "customize-agent",
      "create-agent-template",
      "use-skills",
      "write-skill",
      "organize-agent-skills-and-roles",
    ]),
  },
  {
    en: "Workspaces & workflows",
    zh: "工作区与实战工作流",
    slugs: new Set([
      "work-with-workspaces",
      "run-long-lived-task",
      "delegate-work",
      "run-github-task",
      "build-code-reviewer",
      "build-docs-steward",
      "build-research-steward",
      "build-agent-ecosystem-researcher",
    ]),
  },
  {
    en: "Events & automation",
    zh: "事件、定时与自动化",
    slugs: new Set([
      "connect-external-events",
      "connect-event-inbox",
      "schedule-agent-work",
      "automate-over-http",
      "glitchtip-error-alerts",
      "changedetection-webhook-setup",
    ]),
  },
  {
    en: "Models, memory & privacy",
    zh: "模型、记忆与私有化",
    slugs: new Set([
      "configure-models",
      "run-offline-private-agent",
      "configure-decision-subsystem",
      "manage-agent-memory",
    ]),
  },
  {
    en: "Clients, team & OIDC",
    zh: "客户端、团队与认证",
    slugs: new Set([
      "use-web-gui",
      "connect-remote-runtime",
      "connect-ios-client",
      "connect-android-client",
      "share-with-your-team",
      "configure-oidc-authentication",
      "google-workspace-oidc-authentication",
      "gitlab-ce-oidc-authentication",
      "keycloak-oidc-authentication",
      "casdoor-oidc-authentication",
    ]),
  },
  {
    en: "Tools, apps & ops",
    zh: "工具、本地应用与运维",
    slugs: new Set([
      "use-web-tools",
      "searxng-websearch-provider",
      "generate-image",
      "inspect-image",
      "host-local-apps",
      "observe-agent-work",
      "troubleshooting",
      "configure-observability",
    ]),
  },
];

const REFERENCE_SUBGROUPS = [
  {
    en: "CLI & configuration",
    zh: "CLI 与配置",
    slugs: new Set([
      "cli",
      "cli-contract-inventory",
      "cli-stability-policy",
      "cli-exit-codes",
      "configuration",
      "tui",
    ]),
  },
  {
    en: "Control plane & schemas",
    zh: "控制平面与契约清单",
    slugs: new Set([
      "http-control-plane",
      "api-contract-inventory",
      "model-tool-schema-inventory",
      "runtime-status-enum-inventory",
    ]),
  },
  {
    en: "Runtime & built-in tools",
    zh: "运行时对象与内置工具",
    slugs: new Set([
      "workspaces",
      "agent-templates",
      "skills",
      "work-items",
      "events-and-timers",
      "local-apps",
      "models",
      "web-tools",
      "view-image",
      "image-generation",
      "observability",
    ]),
  },
];

function resolveSubgroup(section, slug, locale) {
  const table = section === "guides" ? GUIDE_SUBGROUPS : section === "reference" ? REFERENCE_SUBGROUPS : null;
  if (!table || !slug) return { subgroup: undefined, subgroupOrder: 0 };
  const idx = table.findIndex((g) => g.slugs.has(slug));
  if (idx === -1) return { subgroup: locale === "zh-CN" ? "更多" : "More", subgroupOrder: 99 };
  return { subgroup: locale === "zh-CN" ? table[idx].zh : table[idx].en, subgroupOrder: idx + 1 };
}

const navigation = {};
for (const locale of ["en", "zh-CN"]) {
  const prefix = locale === "en" ? "" : "zh-CN/";
  navigation[locale] = [];
  for (const [section, en, zh] of sections) {
    const directory = `${prefix}${section}/`;
    const pages = [];
    for (const name of await readdir(new URL(directory, root))) {
      if (!name.endsWith(".md")) continue;
      const { data } = matter(await readFile(new URL(directory + name, root), "utf8"));
      if (data.draft) continue;
      if (typeof data.title !== "string" || !data.title.trim()) {
        throw new Error(`${directory}${name}: navigation needs a title`);
      }
      const overview = name === "README.md";
      const slug = overview ? "" : name.slice(0, -3);
      const href = `/${directory}${slug}`;
      const navTitle = (typeof data.navTitle === "string" && data.navTitle.trim())
        || SHORT_TITLES[locale]?.[href]
        || data.title.trim();
      const { subgroup, subgroupOrder } = overview
        ? { subgroup: undefined, subgroupOrder: 0 }
        : resolveSubgroup(section, slug, locale);
      pages.push({
        title: navTitle,
        href,
        subgroup,
        subgroupOrder,
        order: typeof data.order === "number" ? data.order : Number.MAX_SAFE_INTEGER,
        overview,
      });
    }
    pages.sort((a, b) => Number(b.overview) - Number(a.overview)
      || a.subgroupOrder - b.subgroupOrder
      || a.order - b.order
      || a.title.localeCompare(b.title, locale));
    navigation[locale].push({
      label: locale === "en" ? en : zh,
      pages: pages.map(({ title, href, subgroup }) => (subgroup ? { title, href, subgroup } : { title, href })),
    });
  }
}
await writeFile(new URL("assets/docs-navigation.json", root), JSON.stringify(navigation));
console.log("Built documentation navigation for en and zh-CN.");
