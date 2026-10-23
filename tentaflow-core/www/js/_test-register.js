// ===== File: _test-register.js — installs the root-path resolver before any test module loads =====
import { register } from 'node:module';
import { after } from 'node:test';

register('./_test-resolver.js', import.meta.url);

// An anchored menu left open watches its trigger on animation frames, which would keep a test process
// alive after its last test; closing the leftovers lets the process end the way it does for any other file.
after(() => {
  for (const menu of globalThis.document?.querySelectorAll('tf-menu') ?? []) menu.remove();
});
