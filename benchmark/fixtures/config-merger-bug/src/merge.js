export function mergeConfig(defaultConfig, overrides) {
  return {
    ...defaultConfig,
    ...overrides
  };
}
