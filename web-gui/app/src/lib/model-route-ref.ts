export function compactModelRouteDisplay(modelRouteRef: string): string {
  const slashIndex = modelRouteRef.indexOf("/");
  if (slashIndex < 0) return modelRouteRef;

  const route = modelRouteRef.slice(0, slashIndex);
  const model = modelRouteRef.slice(slashIndex + 1);
  const defaultSuffix = "@default";
  if (!route.endsWith(defaultSuffix) || model.length === 0) return modelRouteRef;

  return `${route.slice(0, -defaultSuffix.length)}/${model}`;
}
