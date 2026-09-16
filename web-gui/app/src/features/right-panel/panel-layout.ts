export const PANEL_MIN = 320;
export const PANEL_DEFAULT = 380;
export const CONVERSATION_MIN = 640;
export const NAV_WIDTH = 280;

export function panelLayout(width: number, open: boolean, expanded: boolean, navPreference: boolean, preferredWidth: number) {
  const desired = Math.max(PANEL_MIN, preferredWidth);
  const navCollapsed = navPreference || width <= 760 || (open && (expanded || width < NAV_WIDTH + CONVERSATION_MIN + desired));
  const navWidth = navCollapsed ? 72 : NAV_WIDTH;
  const full = open && (expanded || width < navWidth + CONVERSATION_MIN + PANEL_MIN);
  return { navCollapsed, full, width: Math.max(PANEL_MIN, Math.min(desired, width - navWidth - CONVERSATION_MIN)) };
}
