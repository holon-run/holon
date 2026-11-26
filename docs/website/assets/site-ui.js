// Progressive enhancements for code copy, multi-platform install tabs, and weighted search.
(() => {
  const zh = document.documentElement.lang === "zh-CN";

  // 1. Accessible tab groups (Quickstart install tabs and Hero workbench/terminal stage tabs).
  const bindTabGroup = (rootSelector, tablistSelector, tabAttr, panelAttr) => {
    for (const container of document.querySelectorAll(rootSelector)) {
      const tablist = container.querySelector(tablistSelector);
      const tabs = [...container.querySelectorAll(`[${tabAttr}]`)];
      const panels = [...container.querySelectorAll(`[${panelAttr}]`)];
      if (!tablist || tabs.length === 0 || panels.length === 0) continue;
      tablist.hidden = false;
      container.dataset.tabsReady = "true";

      const activate = (id, focusTab = false) => {
        for (const tab of tabs) {
          const selected = tab.getAttribute(tabAttr) === id;
          tab.setAttribute("aria-selected", String(selected));
          tab.tabIndex = selected ? 0 : -1;
          if (selected && focusTab) tab.focus();
        }
        for (const panel of panels) {
          panel.hidden = panel.getAttribute(panelAttr) !== id;
        }
      };
      activate(tabs[0].getAttribute(tabAttr));

      for (let i = 0; i < tabs.length; i++) {
        tabs[i].addEventListener("click", () => activate(tabs[i].getAttribute(tabAttr)));
        tabs[i].addEventListener("keydown", (event) => {
          if (event.key !== "ArrowRight" && event.key !== "ArrowLeft") return;
          event.preventDefault();
          const next = (i + (event.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length;
          activate(tabs[next].getAttribute(tabAttr), true);
        });
      }
    }
  };

  bindTabGroup("[data-install-tabs]", ".home-install-tablist", "data-install-tab", "data-install-panel");
  bindTabGroup("[data-stage-tabs]", ".home-stage-tablist", "data-stage-tab", "data-stage-panel");

  // 2. One-click copy buttons on code blocks (skipping template detail commands that already have one).
  if (navigator.clipboard?.writeText) {
    for (const pre of document.querySelectorAll("pre")) {
      if (pre.closest("[data-template-command]") || pre.parentElement?.classList.contains("code-block-wrap")) continue;
      const code = pre.querySelector("code");
      if (!code || !code.textContent.trim()) continue;
      const wrapper = document.createElement("div");
      wrapper.className = "code-block-wrap";
      pre.parentNode.insertBefore(wrapper, pre);
      wrapper.appendChild(pre);

      const button = document.createElement("button");
      button.type = "button";
      button.className = "code-copy-btn";
      const idleLabel = zh ? "复制" : "Copy";
      const doneLabel = zh ? "已复制 ✓" : "Copied ✓";
      button.textContent = idleLabel;
      button.setAttribute("aria-label", zh ? "复制代码" : "Copy code");
      let timer = null;
      button.addEventListener("click", async () => {
        try {
          await navigator.clipboard.writeText(code.textContent.replace(/\n$/, ""));
          button.textContent = doneLabel;
          button.dataset.copied = "true";
          clearTimeout(timer);
          timer = setTimeout(() => {
            button.textContent = idleLabel;
            delete button.dataset.copied;
          }, 1800);
        } catch {
          button.textContent = zh ? "复制失败" : "Copy failed";
        }
      });
      wrapper.appendChild(button);
    }
  }

  // 3. Search shortcut hint, live debounced search, locale prefixing, and user-first section ranking.
  const searchRoot = document.querySelector("[data-site-search]");
  if (searchRoot) {
    const toggle = searchRoot.querySelector(".site-search__toggle");
    const form = searchRoot.querySelector(".site-search__form");
    const input = searchRoot.querySelector(".site-search__input");
    const results = searchRoot.querySelector("[data-site-search-results]");

    if (toggle && !toggle.querySelector(".site-search__shortcut")) {
      const isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
      const kbd = document.createElement("kbd");
      kbd.className = "site-search__shortcut";
      kbd.textContent = isMac ? "⌘K" : "Ctrl K";
      toggle.appendChild(kbd);
    }

    if (form && input && results) {
      const SECTION_WEIGHTS = {
        "getting-started": 1.25,
        concepts: 1.2,
        guides: 1.15,
        reference: 1.1,
        templates: 1.05,
        blog: 1.0,
        spec: 0.42,
        maintainers: 0.38,
      };
      const SECTION_LABELS = zh
        ? {
            "getting-started": "入门",
            concepts: "概念",
            guides: "指南",
            reference: "参考",
            templates: "模板",
            blog: "博客",
            spec: "内部规格",
            maintainers: "维护者",
          }
        : {
            "getting-started": "START",
            concepts: "CONCEPT",
            guides: "GUIDE",
            reference: "REFERENCE",
            templates: "TEMPLATE",
            blog: "BLOG",
            spec: "SPEC",
            maintainers: "MAINTAINER",
          };

      const escapeHtml = (v) => String(v)
        .replaceAll("&", "&amp;")
        .replaceAll("<", "&lt;")
        .replaceAll(">", "&gt;")
        .replaceAll('"', "&quot;")
        .replaceAll("'", "&#39;");

      const sectionOf = (relPath) => {
        const clean = String(relPath || "").replace(/^zh-CN\//, "");
        const idx = clean.indexOf("/");
        return idx === -1 ? "" : clean.slice(0, idx);
      };

      let controller = null;
      let debounceTimer = null;

      const renderMsg = (msg) => {
        results.innerHTML = `<p class="site-search__message">${escapeHtml(msg)}</p>`;
      };

      const runWeightedSearch = async (rawQuery) => {
        const query = rawQuery.trim();
        if (!query) {
          renderMsg(zh ? "输入关键词搜索文档、指南与模板。" : "Type to search docs, guides, and templates.");
          return;
        }
        if (controller) controller.abort();
        controller = new AbortController();
        renderMsg(zh ? "搜索中…" : "Searching…");
        try {
          const url = new URL("/api/search", window.location.origin);
          url.searchParams.set("q", query);
          url.searchParams.set("topK", "24");
          if (zh) {
            url.searchParams.set("relativePathPrefix", "zh-CN/");
          }
          const response = await fetch(url, { signal: controller.signal });
          if (!response.ok) {
            renderMsg(zh ? `搜索失败 (${response.status})。` : `Search failed (${response.status}).`);
            return;
          }
          const payload = await response.json();
          const rawHits = Array.isArray(payload.hits) ? payload.hits : [];
          const filtered = rawHits
            .filter((hit) => {
              if (typeof hit.relativePath !== "string") return false;
              return zh ? hit.relativePath.startsWith("zh-CN/") : !hit.relativePath.startsWith("zh-CN/");
            })
            .map((hit) => {
              const sec = sectionOf(hit.relativePath);
              const weight = SECTION_WEIGHTS[sec] ?? 1.0;
              return { hit, sec, adjustedScore: (typeof hit.score === "number" ? hit.score : 1) * weight };
            })
            .sort((a, b) => b.adjustedScore - a.adjustedScore)
            .slice(0, 8);

          if (filtered.length === 0) {
            renderMsg(zh ? "没有找到匹配结果。" : "No matching results.");
            return;
          }
          results.innerHTML = filtered.map(({ hit, sec }) => {
            const rawHref = String(hit.citationUrl || hit.canonicalUrl || hit.docId || "#");
            const href = escapeHtml(rawHref.replace(/^https?:\/\/[^/]+/, "") || "/");
            const title = escapeHtml(hit.title || hit.relativePath || (zh ? "无标题" : "Untitled"));
            const badgeLabel = SECTION_LABELS[sec];
            const badge = badgeLabel ? `<span class="site-search__badge">${escapeHtml(badgeLabel)}</span>` : "";
            const summary = typeof hit.summary === "string"
              ? `<span class="site-search__item-summary">${escapeHtml(hit.summary)}</span>`
              : "";
            const cleanExcerpt = hit.bestMatch && typeof hit.bestMatch.excerpt === "string"
              ? hit.bestMatch.excerpt.replace(/\s+/g, " ").trim()
              : "";
            const excerpt = cleanExcerpt
              ? `<span class="site-search__item-excerpt">${escapeHtml(cleanExcerpt)}</span>`
              : "";
            return `<a class="site-search__item" href="${href}"><span class="site-search__item-head"><strong class="site-search__item-title">${title}</strong>${badge}</span>${summary}${excerpt}</a>`;
          }).join("");
        } catch (err) {
          if (err && typeof err === "object" && err.name === "AbortError") return;
          renderMsg(zh ? "搜索失败。" : "Search failed.");
        }
      };

      // Capture submit before mdorigin's inline listener so our weighted search owns rendering.
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        event.stopImmediatePropagation();
        clearTimeout(debounceTimer);
        void runWeightedSearch(input.value);
      }, true);

      input.addEventListener("input", () => {
        clearTimeout(debounceTimer);
        debounceTimer = setTimeout(() => {
          void runWeightedSearch(input.value);
        }, 180);
      });
    }
  }
})();
