// Progressive enhancement: role links and selectable commands work without JS.
(() => {
  const zh = document.documentElement.lang === "zh-CN";
  for (const button of document.querySelectorAll("[data-template-copy]")) {
    const body = button.closest("[data-template-command]");
    const code = body.querySelector("pre code");
    const status = body.querySelector(".template-copy-status");
    // An unavailable clipboard leaves the selectable command as the fallback.
    if (!navigator.clipboard?.writeText) continue;
    button.hidden = false;
    button.addEventListener("click", async () => {
      button.disabled = true;
      try {
        await navigator.clipboard.writeText(code.textContent);
        status.textContent = zh ? "已复制" : "Copied";
      } catch {
        status.textContent = zh ? "复制失败，请手动选择命令。" : "Could not copy. Select the command manually.";
      } finally {
        button.disabled = false;
      }
    });
  }

  const directory = document.querySelector(".template-directory");
  if (!directory) return;

  const input = directory.querySelector("#template-query");
  const filters = [...directory.querySelectorAll("[data-template-filter]")];
  const groups = [...directory.querySelectorAll("[data-template-group]")];
  const results = directory.querySelector("[data-template-results]");
  const empty = directory.querySelector("[data-template-empty]");
  const entries = [...directory.querySelectorAll(".template-entry")].map((element) => ({
    element,
    category: element.querySelector("[data-template-category]").dataset.templateCategory,
    text: (element.id + " " + element.textContent + " "
      + element.querySelector("[data-template-keywords]").dataset.templateKeywords)
      .normalize("NFKC").toLocaleLowerCase(),
  }));
  let category = "all";

  function filter() {
    const terms = input.value.normalize("NFKC").toLocaleLowerCase().trim().split(/\s+/).filter(Boolean);
    let count = 0;
    for (const entry of entries) {
      entry.element.hidden = !((category === "all" || category === entry.category)
        && terms.every((term) => entry.text.includes(term)));
      if (!entry.element.hidden) count++;
    }
    for (const group of groups) {
      group.hidden = !group.querySelector(".template-entry:not([hidden])");
    }
    for (const link of filters) {
      if (link.dataset.templateFilter === category) link.setAttribute("aria-current", "true");
      else link.removeAttribute("aria-current");
    }
    results.hidden = false;
    results.textContent = zh ? `显示 ${count} / ${entries.length} 个角色` : `Showing ${count} of ${entries.length} roles`;
    empty.hidden = count !== 0;
  }

  input.addEventListener("input", filter);
  for (const link of filters) {
    link.addEventListener("click", (event) => {
      // Preserve normal open-in-new-tab and no-JS anchor behavior.
      if (event.ctrlKey || event.metaKey || event.shiftKey || event.altKey || event.button) return;
      event.preventDefault();
      category = link.dataset.templateFilter;
      filter();
    });
  }

  function revealAnchor() {
    let id;
    try { id = decodeURIComponent(location.hash.slice(1)); } catch { return; }
    const entry = entries.find((item) => item.element.id === id);
    if (!entry) return;
    // Preserve previously shared /templates/#role links.
    location.replace(entry.element.querySelector("h3 a").href);
  }

  window.addEventListener("hashchange", revealAnchor);
  directory.querySelector("[data-template-search]").hidden = false;

  filter();
  revealAnchor();
})();
