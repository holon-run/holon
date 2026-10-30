// Navigation and anchors work without JS; enhance responsive controls and reading state.
const layout = document.querySelector(".docs-layout");
if (layout) {
  const sidebar = layout.querySelector(".docs-sidebar");
  const toc = layout.querySelector(".docs-toc");
  const mobile = matchMedia("(max-width: 767px)");
  const syncControls = () => {
    sidebar.open = !mobile.matches;
    if (toc) toc.open = !mobile.matches;
  };
  syncControls();
  mobile.addEventListener("change", syncControls);
  const active = sidebar.querySelector('[aria-current="page"]');
  if (active && !mobile.matches) {
    // Scroll only the sidebar, never the document or an incoming heading fragment.
    sidebar.scrollTop = Math.max(0, active.offsetTop - sidebar.offsetTop - sidebar.clientHeight / 3);
  }
  const header = document.querySelector(".site-header");
  if (header) {
    new ResizeObserver(() => {
      document.documentElement.style.setProperty("--docs-header-height", `${header.offsetHeight}px`);
    }).observe(header);
  }
  if (toc) {
    const links = [...toc.querySelectorAll('a[href^="#"]')];
    const headings = links.map((link) => document.getElementById(
      decodeURIComponent(link.getAttribute("href").slice(1)),
    )).filter(Boolean);
    let scheduled = false;
    const update = () => {
      const boundary = (header?.offsetHeight || 80) + 32;
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
