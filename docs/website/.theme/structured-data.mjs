// Structured data & chrome refinements:
// 1. schema.org JSON-LD on every page (Organization, WebSite, Article, BreadcrumbList)
// 2. Header GitHub icon link for immediate open-source repository access
// 3. Accurate footer edit-link rewriting/removal for generated template & model pages
// 4. Global progressive UI enhancements (/assets/site-ui.js)
const SITE_URL = "https://holon.run";
const GITHUB_REPO = "https://github.com/holon-run/holon";
const ORGANIZATION = {
  "@type": "Organization",
  name: "Holon",
  url: `${SITE_URL}/`,
  logo: `${SITE_URL}/assets/logo.png`,
  sameAs: [GITHUB_REPO, "https://x.com/holonrun"],
};
const DEFAULT_SOCIAL_IMAGE = `${SITE_URL}/assets/social-card.png`;
const GITHUB_ICON_SVG = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2C6.48 2 2 6.58 2 12.11c0 4.43 2.87 8.18 6.84 9.5.5.1.68-.22.68-.48 0-.24-.01-1.04-.01-1.89-2.78.62-3.37-1.19-3.37-1.19-.46-1.17-1.11-1.48-1.11-1.48-.91-.63.07-.62.07-.62 1 .08 1.53 1.04 1.53 1.04.9 1.54 2.35 1.09 2.92.84.09-.66.35-1.09.63-1.34-2.22-.25-4.55-1.12-4.55-4.97 0-1.1.39-2 1.03-2.71-.1-.26-.45-1.29.1-2.68 0 0 .84-.27 2.75 1.03A9.4 9.4 0 0 1 12 6.84c.85 0 1.71.12 2.51.35 1.91-1.3 2.75-1.03 2.75-1.03.55 1.39.2 2.42.1 2.68.64.71 1.03 1.61 1.03 2.71 0 3.86-2.33 4.72-4.56 4.97.36.31.68.91.68 1.84 0 1.33-.01 2.4-.01 2.73 0 .27.18.58.69.48A10.11 10.11 0 0 0 22 12.11C22 6.58 17.52 2 12 2Z"></path></svg>';

const absoluteUrl = (value) => (/^https?:\/\//.test(value) ? value : `${SITE_URL}${value}`);
const normalize = (value) => value.replace(/\/$/, "");

function normalizeDateString(value) {
  if (!value) return undefined;
  if (value instanceof Date && !Number.isNaN(value.getTime())) {
    return value.toISOString().slice(0, 10);
  }
  const text = String(value).trim();
  const match = text.match(/^\d{4}-\d{2}-\d{2}/);
  return match ? match[0] : (text || undefined);
}

export function structuredDataLayout(navigation) {
  return {
    name: "holon-structured-data",
    transformHtml(html, context) {
      const page = context.page;
      const zh = page.locale === "zh-CN";
      const canonical = normalize(page.canonicalPath || page.requestPath);
      const source = page.sourcePath.replace(/^\/?(?:zh-CN\/)?/, "");
      const graph = [ORGANIZATION];

      if (canonical === "" || canonical === "/zh-CN") {
        graph.push({
          "@type": "WebSite",
          name: "Holon",
          url: `${SITE_URL}${canonical}/`,
          inLanguage: page.locale,
        });
      }

      const isBlogArticle = page.kind === "page" && /^blog\/(?!README\.md)[^/]+\.md$/i.test(source);
      const publishedDate = normalizeDateString(page.date ?? page.meta?.date);
      if (isBlogArticle) {
        const article = {
          "@type": "Article",
          headline: page.title,
          description: page.summary,
          inLanguage: page.locale,
          mainEntityOfPage: `${SITE_URL}${canonical}`,
          image: page.socialImage ? absoluteUrl(page.socialImage) : DEFAULT_SOCIAL_IMAGE,
          author: { "@type": "Organization", name: "Holon", url: `${SITE_URL}/` },
          publisher: ORGANIZATION,
        };
        if (publishedDate) {
          article.datePublished = publishedDate;
        }
        graph.push(article);
      }

      const groups = navigation?.[zh ? "zh-CN" : "en"] ?? [];
      const group = groups.find((entry) => entry.pages.some((item) => normalize(item.href) === canonical));
      if (group && /^(getting-started|concepts|guides|reference)\//.test(source)) {
        const section = source.split("/")[0];
        graph.push({
          "@type": "BreadcrumbList",
          itemListElement: [
            { "@type": "ListItem", position: 1, name: zh ? "首页" : "Home", item: `${SITE_URL}${zh ? "/zh-CN/" : "/"}` },
            { "@type": "ListItem", position: 2, name: group.label, item: `${SITE_URL}${zh ? "/zh-CN/" : "/"}${section}/` },
            { "@type": "ListItem", position: 3, name: page.title },
          ],
        });
      }

      let output = html;
      if (isBlogArticle && output.includes('property="og:type"')) {
        output = output.replace(/<meta property="og:type" content="[^"]*">/, '<meta property="og:type" content="article">');
      }

      // Add GitHub repository icon in top header actions.
      const githubLabel = zh ? "在 GitHub 查看源码" : "View Holon on GitHub";
      const githubButton = `<a class="site-header__github" href="${GITHUB_REPO}" aria-label="${githubLabel}" title="GitHub">${GITHUB_ICON_SVG}</a>`;
      output = output.replace(
        /<div class="site-header__actions">([\s\S]*?)<\/div><\/div><\/header>/,
        `<div class="site-header__actions">$1${githubButton}</div></div></header>`,
      );

      // Rewrite or remove footer edit links on generated pages so they never 404 on GitHub.
      const templateDetailMatch = source.match(/^templates\/([^/]+)\/README\.md$/);
      if (templateDetailMatch) {
        const templateId = templateDetailMatch[1];
        const contractHref = `${GITHUB_REPO}/blob/main/agent_templates/${encodeURIComponent(templateId)}/AGENTS.md`;
        const contractLabel = zh ? "查看角色契约源码" : "View template contract";
        output = output.replace(
          /<a class="site-footer__edit-link" href="[^"]*">[\s\S]*?<\/a>/,
          `<a class="site-footer__edit-link" href="${contractHref}">${contractLabel}</a>`,
        );
      } else if (source === "templates/README.md" || source === "reference/models.md") {
        output = output.replace(/<a class="site-footer__edit-link" href="[^"]*">[\s\S]*?<\/a>/, "");
      }

      const additions = [
        `<script type="application/ld+json">${JSON.stringify({ "@context": "https://schema.org", "@graph": graph }).replace(/</g, "\\u003c")}</script>`,
      ];
      if (isBlogArticle && !output.includes('property="og:type"')) {
        additions.push('<meta property="og:type" content="article">');
      }
      if (isBlogArticle && publishedDate && !output.includes('property="article:published_time"')) {
        additions.push(`<meta property="article:published_time" content="${publishedDate}">`);
      }

      if (output.includes("</head>")) {
        output = output.replace("</head>", `${additions.join("")}</head>`);
      }
      if (output.includes("</body>")) {
        output = output.replace("</body>", '<script src="/assets/site-ui.js" defer></script></body>');
      }
      return output;
    },
  };
}
