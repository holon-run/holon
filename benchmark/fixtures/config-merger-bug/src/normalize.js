export function normalizeOverrides(overrides) {
  return {
    ui: overrides.ui
      ? {
          theme: overrides.ui.theme,
          density: overrides.ui.density
        }
      : undefined,
    notifications: overrides.notifications
      ? {
          email: overrides.notifications.email !== undefined ? Boolean(overrides.notifications.email) : undefined,
          sms: overrides.notifications.sms !== undefined ? Boolean(overrides.notifications.sms) : undefined
        }
      : undefined
  };
}
