type RouteHandler = (params: Record<string, string>) => void;

interface RouteEntry {
  pattern: RegExp;
  paramNames: string[];
  handler: RouteHandler;
}

const routeEntries: RouteEntry[] = [];
let notFoundHandler: RouteHandler = () => {};

export function defineRoute(path: string, handler: RouteHandler): void {
  const paramNames: string[] = [];
  const regexStr = path.replace(/:(\w+)/g, (_, name: string) => {
    paramNames.push(name);
    return "([^/]+)";
  });
  routeEntries.push({ pattern: new RegExp(`^${regexStr}$`), paramNames, handler });
}

export function onNotFound(handler: RouteHandler): void {
  notFoundHandler = handler;
}

export function navigate(hash: string): void {
  window.location.hash = hash;
}

export function currentPath(): string {
  return window.location.hash.slice(1) || "/";
}

export function startRouter(): void {
  window.addEventListener("hashchange", () => resolve());
  resolve();
}

function resolve(): void {
  const path = currentPath();

  for (const route of routeEntries) {
    const match = path.match(route.pattern);
    if (!match) continue;

    const params: Record<string, string> = {};
    route.paramNames.forEach((name, i) => {
      params[name] = decodeURIComponent(match[i + 1]!);
    });
    route.handler(params);
    return;
  }

  notFoundHandler({});
}
