// ===== File: _test-resolver.js — resolves the dashboard's root-absolute imports under node =====
// The dashboard imports shared modules by their served path ('/js/lib/sfx.js'),
// which the browser resolves against the origin. Node has no origin, so the
// unit-test runner maps that prefix onto the www directory instead of forcing
// every component to use relative paths that would break once a file moves.
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';

const WWW_ROOT = dirname(dirname(fileURLToPath(import.meta.url)));

export async function resolve(specifier, context, nextResolve) {
  if (specifier.startsWith('/js/')) {
    return nextResolve(pathToFileURL(join(WWW_ROOT, specifier)).href, context);
  }
  return nextResolve(specifier, context);
}
