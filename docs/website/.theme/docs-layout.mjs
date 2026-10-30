const escape = (value) => String(value).replace(/[&<>"']/g,
  (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[char]);
const normalize = (value) => value.replace(/\/$/, "");

export function documentationLayout(navigation) {
  return {
    name: "holon-documentation-layout",
    renderPage(page, _context, next) {
      // Keep section indexes and details in the same shell, using canonical sources.
      const source = page.sourcePath.replace(/^\/?(?:zh-CN\/)?/, "");
      if (!/^(getting-started|concepts|guides|reference)\/.+\.md$/.test(source)) {
        return next(page);
      }
      const zh = page.locale === "zh-CN";
      const labels = zh
        ? { docs: "文档导航", menu: "浏览文档", toc: "本页提纲", skip: "跳到正文" }
        : { docs: "Documentation", menu: "Browse docs", toc: "On this page", skip: "Skip to content" };
      const current = normalize(page.canonicalPath || page.requestPath);
      const groups = navigation[zh ? "zh-CN" : "en"];
      const sidebar = groups.map((group) => `<section class="docs-nav__group">
        <h2>${escape(group.label)}</h2><ul>${group.pages.map((entry) =>
          `<li><a href="${escape(entry.href)}"${normalize(entry.href) === current
            ? ' aria-current="page"' : ""}>${escape(entry.title)}</a></li>`).join("")}</ul></section>`).join("");

      // mdorigin assigns stable IDs before this hook; reuse them rather than re-slugging.
      // Only consume the renderer's H2/H3 elements, not examples inside code blocks.
      const headings = [...page.bodyHtml.matchAll(/<h([23])\b[^>]*\bid="([^"]+)"[^>]*>([\s\S]*?)<\/h\1>/g)];
      const items = headings.map(([, depth, id, content]) =>
        `<li class="docs-toc__level-${depth}"><a href="#${id}">${content.replace(/<[^>]*>/g, "")}</a></li>`).join("");
      const toc = items ? `<details class="docs-toc" open>
        <summary>${labels.toc}</summary><nav aria-label="${labels.toc}"><ul>${items}</ul></nav>
        </details>` : "";
      return next({
        ...page,
        bodyHtml: `<div class="docs-layout${toc ? "" : " docs-layout--no-toc"}">
          <a class="docs-skip" href="#docs-content">${labels.skip}</a>
          <details class="docs-sidebar" open>
            <summary>${labels.menu}</summary><nav aria-label="${labels.docs}">${sidebar}</nav>
          </details>
          ${toc}
          <div class="docs-content" id="docs-content" tabindex="-1">${page.bodyHtml}</div>
        </div><script src="/assets/docs-layout.js" defer></script>`,
      });
    },
  };
}
