import { useLayoutEffect, useRef, type ReactNode, type RefObject } from "react";

/** Keep file menus out of the browser's clipped, content-sized preview area. */
export function FileBrowserPopover({ anchorRef, className = "", children }: {
  anchorRef: RefObject<HTMLDivElement | null>;
  className?: string;
  children: ReactNode;
}) {
  const contentRef = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const content = contentRef.current;
    const anchor = anchorRef.current;
    if (!content || !anchor) return;
    content.showPopover();
    const position = () => {
      const rect = anchor.getBoundingClientRect();
      const gap = 4;
      const padding = 8;
      const below = Math.max(0, window.innerHeight - rect.bottom - gap - padding);
      const above = Math.max(0, rect.top - gap - padding);
      content.style.maxHeight = `${Math.min(280, Math.max(below, above))}px`;
      const { width, height } = content.getBoundingClientRect();
      const top = below >= height || below >= above ? rect.bottom + gap : rect.top - gap - height;
      content.style.top = `${Math.max(padding, Math.min(top, window.innerHeight - height - padding))}px`;
      content.style.left = `${Math.max(padding, Math.min(rect.right - width, window.innerWidth - width - padding))}px`;
    };
    position();
    const observer = new ResizeObserver(position);
    observer.observe(anchor.closest(".file-browser") ?? anchor);
    observer.observe(content);
    window.addEventListener("resize", position);
    window.addEventListener("scroll", position, true);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", position);
      window.removeEventListener("scroll", position, true);
      content.hidePopover();
    };
  }, [anchorRef]);

  return <div ref={contentRef} popover="manual" className={`file-browser-popover-content ${className}`}>{children}</div>;
}
