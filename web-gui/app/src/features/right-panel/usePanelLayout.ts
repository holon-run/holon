import { useCallback, useEffect, useState } from "react";
import { PANEL_DEFAULT, PANEL_MIN, panelLayout } from "./panel-layout";

const WIDTH_KEY = "holon:panelWidth";
export function usePanelLayout(open: boolean, expanded: boolean, navPreference: boolean) {
  const [viewport, setViewport] = useState(() => window.innerWidth);
  const [preferredWidth, setPreferredWidth] = useState(() => {
    try {
      const stored = Number(localStorage.getItem(WIDTH_KEY));
      return Number.isFinite(stored) && stored >= PANEL_MIN ? stored : PANEL_DEFAULT;
    } catch { return PANEL_DEFAULT; }
  });
  useEffect(() => {
    const resize = () => setViewport(window.innerWidth);
    window.addEventListener("resize", resize);
    return () => window.removeEventListener("resize", resize);
  }, []);
  const resizePanel = useCallback((width: number, save = false) => {
    setPreferredWidth(width);
    if (save) { try { localStorage.setItem(WIDTH_KEY, String(width)); } catch { /* storage unavailable */ } }
  }, []);
  return { ...panelLayout(viewport, open, expanded, navPreference, preferredWidth), resizePanel };
}
