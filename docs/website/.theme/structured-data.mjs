// Structured data for search and answer engines (search-visibility OP-001):
// schema.org JSON-LD on every page — Organization globally, WebSite on locale
// homes, Article on blog posts, BreadcrumbList on documentation pages — plus
// og:type=article and article:published_time for blog posts. Only facts
// already present in page metadata are mirrored; nothing new is asserted.
const SITE_URL = "https://holon.run";
const ORGANIZATION = {
  "@type": "Organization",
  name: "Holon",
  url: `${SITE_URL}/`,
  logo: `${SITE_URL}/assets/logo.png`,
  sameAs: ["https://github.com/holon-run/holon", "https://x.com/holonrun"],
};
const DEFAULT_SOCIAL_IMAGE = `${SITE_URL}/assets/social-card.png`;

const absoluteUrl = (value) => (/^https?:\/\//.test(value) ? value : `${SITE_URL}${value}`);
const normalize = (value) => value.replace(/\/$/, "");

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

      const isBlogArticle = page.kind === "page" && /^blog\/[^/]+\.md$/.test(source);
      if (isBlogArticle) {
        const article = {
          "@type": "Article",
          headline: page.title,
          description: page.summary,
          inLanguage: page.locale,
          mainEntityOfPage: `${SITE_URL}${canonical}/`,
          image: page.socialImage ? absoluteUrl(page.socialImage) : DEFAULT_SOCIAL_IMAGE,
          author: { "@type": "Organization", name: "Holon", url: `${SITE_URL}/` },
          publisher: ORGANIZATION,
        };
        if (page.date) {
          article.datePublished = page.date;
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
        // Older mdorigin builds emit og:type=website for every page.
        output = output.replace(/<meta property="og:type" content="[^"]*">/, '<meta property="og:type" content="article">');
      }

      const additions = [
        `<script type="application/ld+json">${JSON.stringify({ "@context": "https://schema.org", "@graph": graph }).replace(/</g, "\\u003c")}</script>`,
      ];
      if (isBlogArticle && !output.includes('property="og:type"')) {
        additions.push('<meta property="og:type" content="article">');
      }
      if (isBlogArticle && page.date) {
        additions.push(`<meta property="article:published_time" content="${page.date}">`);
      }
      // transformHtml runs after the document is assembled, so </head> is stable.
      return output.includes("</head>") ? output.replace("</head>", `${additions.join("")}</head>`) : output;
    },
  };
}
