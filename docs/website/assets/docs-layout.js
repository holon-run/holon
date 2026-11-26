// Navigation and anchors work without JS; enhance responsive controls, collapsible groups, and TOC scroll state.
(() => {
  const header = document.querySelector(".site-header");
  if (header) {
    const syncHeaderHeight = () => {
      document.documentElement.style.setProperty("--docs-header-height", `${header.offsetHeight}px`);
    };
    syncHeaderHeight();
    new ResizeObserver(syncHeaderHeight).observe(header);
  }

  const layout = document.querySelector(".docs-layout");
  if (layout) {
    const sidebar = layout.querySelector(".docs-sidebar");
    const toc = layout.querySelector(".docs-toc");
    const mobile = matchMedia("(max-width: 767px)");

    // Progressive collapsible section groups so inactive 40+ item sections don't overwhelm the sidebar.
    for (const group of sidebar.querySelectorAll(".docs-nav__group")) {
      const heading = group.querySelector("h2");
      const list = group.querySelector("ul");
      if (!heading || !list) continue;
      const count = list.querySelectorAll("a").length;
      const initiallyOpen = group.dataset.groupActive === "true";
      const button = document.createElement("button");
      button.type = "button";
      button.className = "docs-nav__group-toggle";
      button.setAttribute("aria-expanded", String(initiallyOpen));
      button.innerHTML = `<span>${heading.textContent}</span><small>${count}</small>`;
      heading.replaceChildren(button);
      list.hidden = !initiallyOpen;
      button.addEventListener("click", () => {
        const nextOpen = list.hidden;
        list.hidden = !nextOpen;
        button.setAttribute("aria-expanded", String(nextOpen));
      });
    }

    const syncControls = () => {
      sidebar.open = !mobile.matches;
      if (toc) toc.open = !mobile.matches;
    };
    syncControls();
    mobile.addEventListener("change", syncControls);

    const active = sidebar.querySelector('[aria-current="page"]');
    if (active && !mobile.matches) {
      sidebar.scrollTop = Math.max(0, active.offsetTop - sidebar.offsetTop - sidebar.clientHeight / 3);
    }
  }

  const toc = document.querySelector(".docs-toc");
  if (toc) {
    const links = [...toc.querySelectorAll('a[href^="#"]')];
    const headings = links.map((link) => document.getElementById(
      decodeURIComponent(link.getAttribute("href").slice(1)),
    )).filter(Boolean);
    if (headings.length > 0) {
      let scheduled = false;
      const update = () => {
        const boundary = (header?.offsetHeight || 80) + 36;
        let current = headings[0];
        for (const heading of headings) {
          if (heading.getBoundingClientRect().top <= boundary) current = heading;
          else break;
        }
        links.forEach((link) => {
          if (decodeURIComponent(link.hash.slice(1)) === current?.id) link.setAttribute("aria-current", "location");
          else link.removeAttribute("aria-current");
        });
        scheduled = false;
      };
      addEventListener("scroll", () => {
        if (!scheduled) {
          scheduled = true;
          requestAnimationFrame(update);
        }
      }, { passive: true });
      addEventListener("hashchange", update);
      update();
    }
  }
})();
