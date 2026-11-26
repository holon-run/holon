const escape = (value) => String(value).replace(/[&<>"']/g,
  (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[char]);
const normalize = (value) => value.replace(/\/$/, "");

function extractTocItems(bodyHtml) {
  const headings = [...bodyHtml.matchAll(/<h([23])\b[^>]*\bid="([^"]+)"[^>]*>([\s\S]*?)<\/h\1>/g)];
  return headings.map(([, depth, id, content]) =>
    `<li class="docs-toc__level-${depth}"><a href="#${id}">${content.replace(/<[^>]*>/g, "")}</a></li>`).join("");
}

function formatIsoDate(value) {
  if (!value) return "";
  if (value instanceof Date && !Number.isNaN(value.getTime())) {
    return value.toISOString().slice(0, 10);
  }
  const text = String(value).trim();
  const match = text.match(/^\d{4}-\d{2}-\d{2}/);
  if (match) return match[0];
  const parsed = new Date(text);
  return Number.isNaN(parsed.getTime()) ? "" : parsed.toISOString().slice(0, 10);
}

function estimateReadingMinutes(html, zh) {
  const plain = html.replace(/<pre[\s\S]*?<\/pre>/g, " ").replace(/<[^>]*>/g, " ").trim();
  if (!plain) return 1;
  if (zh) {
    const chars = plain.replace(/\s+/g, "").length;
    return Math.max(1, Math.round(chars / 450));
  }
  const words = plain.split(/\s+/).filter(Boolean).length;
  return Math.max(1, Math.round(words / 220));
}

export function documentationLayout(navigation) {
  return {
    name: "holon-documentation-layout",
    renderPage(page, _context, next) {
      const source = page.sourcePath.replace(/^\/?(?:zh-CN\/)?/, "");
      const zh = page.locale === "zh-CN";

      // Dedicated reading shell for individual Blog articles (not /blog/ index).
      if (page.kind === "page" && /^blog\/(?!README\.md)[^/]+\.md$/i.test(source)) {
        const items = extractTocItems(page.bodyHtml);
        const tocLabel = zh ? "本文提纲" : "On this page";
        const backHref = zh ? "/zh-CN/blog/" : "/blog/";
        const backLabel = zh ? "← 全部文章" : "← All articles";
        const published = formatIsoDate(page.date ?? page.meta?.date);
        const minutes = estimateReadingMinutes(page.bodyHtml, zh);
        const readTime = zh ? `约 ${minutes} 分钟阅读` : `${minutes} min read`;
        const mdChip = page.alternateMarkdownPath
          ? `<a class="docs-md-chip" href="${escape(page.alternateMarkdownPath)}">${zh ? "Markdown 原文" : "View Markdown"}</a>`
          : "";
        const toc = items
          ? `<aside class="blog-article-toc"><details class="docs-toc" open><summary>${tocLabel}</summary><nav aria-label="${tocLabel}"><ul>${items}</ul></nav></details></aside>`
          : "";
        return next({
          ...page,
          bodyHtml: `<div class="blog-article-layout${toc ? "" : " blog-article-layout--no-toc"}">
            <div class="blog-article-main">
              <div class="blog-article-topbar">
                <a class="blog-article-back" href="${backHref}">${backLabel}</a>
                <div class="blog-article-meta">
                  ${published ? `<time datetime="${escape(published)}">${escape(published)}</time><span>·</span>` : ""}
                  <span>${readTime}</span>
                  ${mdChip}
                </div>
              </div>
              <div class="blog-article-body">${page.bodyHtml}</div>
            </div>
            ${toc}
          </div><script src="/assets/docs-layout.js" defer></script>`,
        });
      }

      // Keep section indexes and details in the documentation shell, using canonical sources.
      if (!/^(getting-started|concepts|guides|reference)\/.+\.md$/.test(source)) {
        return next(page);
      }
      const labels = zh
        ? {
            docs: "文档导航",
            menu: "浏览文档",
            toc: "本页提纲",
            skip: "跳到正文",
            home: "首页",
            md: "Markdown 原文",
            prev: "上一篇",
            next: "下一篇",
            pager: "文档翻页",
          }
        : {
            docs: "Documentation",
            menu: "Browse docs",
            toc: "On this page",
            skip: "Skip to content",
            home: "Home",
            md: "View Markdown",
            prev: "Previous",
            next: "Next",
            pager: "Documentation pagination",
          };
      const current = normalize(page.canonicalPath || page.requestPath);
      const groups = navigation[zh ? "zh-CN" : "en"];
      const sidebar = groups.map((group, groupIndex) => {
        const isCurrentGroup = group.pages.some((entry) => normalize(entry.href) === current);
        let lastSubgroup = null;
        const listHtml = group.pages.map((entry) => {
          let subhead = "";
          if (entry.subgroup && entry.subgroup !== lastSubgroup) {
            lastSubgroup = entry.subgroup;
            subhead = `<li class="docs-nav__subhead" role="presentation">${escape(entry.subgroup)}</li>`;
          }
          const isCurrent = normalize(entry.href) === current;
          return `${subhead}<li><a href="${escape(entry.href)}"${isCurrent ? ' aria-current="page"' : ""}>${escape(entry.title)}</a></li>`;
        }).join("");
        return `<section class="docs-nav__group" data-group-active="${isCurrentGroup || groupIndex === 0 ? "true" : "false"}">
        <h2>${escape(group.label)}</h2><ul>${listHtml}</ul></section>`;
      }).join("");

      const items = extractTocItems(page.bodyHtml);
      const toc = items ? `<details class="docs-toc" open>
        <summary>${labels.toc}</summary><nav aria-label="${labels.toc}"><ul>${items}</ul></nav>
        </details>` : "";

      // Render breadcrumbs, Markdown chip, and Prev/Next pagination on real site requests.
      let topbar = "";
      let pager = "";
      if (page.alternateMarkdownPath) {
        const flat = groups.flatMap((group) =>
          group.pages.map((entry) => ({ ...entry, groupLabel: group.label, sectionHref: group.pages[0]?.href }))
        );
        const idx = flat.findIndex((entry) => normalize(entry.href) === current);
        const activeEntry = idx >= 0 ? flat[idx] : null;
        const homeHref = zh ? "/zh-CN/" : "/";
        const crumbs = activeEntry
          ? `<nav class="docs-breadcrumbs" aria-label="Breadcrumb"><a href="${homeHref}">${labels.home}</a><span aria-hidden="true">/</span>${
              normalize(activeEntry.sectionHref || "") !== current
                ? `<a href="${escape(activeEntry.sectionHref)}">${escape(activeEntry.groupLabel)}</a>`
                : `<span>${escape(activeEntry.groupLabel)}</span>`
            }${activeEntry.subgroup ? `<span aria-hidden="true">/</span><span>${escape(activeEntry.subgroup)}</span>` : ""}</nav>`
          : "";
        topbar = `<div class="docs-content__topbar">${crumbs}<a class="docs-md-chip" href="${escape(page.alternateMarkdownPath)}">${labels.md}</a></div>`;

        if (idx >= 0) {
          const prev = idx > 0 ? flat[idx - 1] : null;
          const nextEntry = idx < flat.length - 1 ? flat[idx + 1] : null;
          if (prev || nextEntry) {
            pager = `<nav class="docs-pager" aria-label="${labels.pager}">
              ${prev ? `<a class="docs-pager__link docs-pager__link--prev" href="${escape(prev.href)}"><small>← ${labels.prev}</small><strong>${escape(prev.title)}</strong></a>` : "<span></span>"}
              ${nextEntry ? `<a class="docs-pager__link docs-pager__link--next" href="${escape(nextEntry.href)}"><small>${labels.next} →</small><strong>${escape(nextEntry.title)}</strong></a>` : "<span></span>"}
            </nav>`;
          }
        }
      }

      return next({
        ...page,
        bodyHtml: `<div class="docs-layout${toc ? "" : " docs-layout--no-toc"}">
          <a class="docs-skip" href="#docs-content">${labels.skip}</a>
          <details class="docs-sidebar" open>
            <summary>${labels.menu}</summary><nav aria-label="${labels.docs}">${sidebar}</nav>
          </details>
          ${toc}
          <div class="docs-content" id="docs-content" tabindex="-1">${topbar}${page.bodyHtml}${pager}</div>
        </div><script src="/assets/docs-layout.js" defer></script>`,
      });
    },
  };
}
