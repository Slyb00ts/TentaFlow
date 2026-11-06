// =============================================================================
// File: wasm-fallback.js — start a wasm-bindgen module, or its plain-JS twin
// =============================================================================
// iOS Lockdown Mode removes the WebAssembly global from every WKWebView, and
// an app cannot opt out without Apple's browser entitlement. build.rs therefore
// compiles each browser module a second time with wasm2js (`<name>_asm.js`,
// slower but pure JavaScript). The wasm-bindgen glue is generated and must not
// be edited, so the twin is presented to it as a minimal WebAssembly object for
// the duration of init.

// wasm2js lowers every i64 to an i32 pair: a parameter becomes (low, high) and a
// result returns the low word, with the high word passed to env.setTempRet0.
// The glue speaks BigInt, so this adapter converts at the boundary; `i64Results`
// (written by build.rs from the disassembly) names the exports that return one.
function instantiateTwin(asmFunc, i64Results, imports) {
  let tempRet0 = 0;
  const raw = asmFunc({ ...imports, env: { setTempRet0(v) { tempRet0 = v | 0; } } });
  const returnsI64 = new Set(i64Results);
  const exports = {};
  for (const [name, value] of Object.entries(raw)) {
    if (typeof value !== 'function') {
      exports[name] = value;
      continue;
    }
    exports[name] = (...args) => {
      const lowered = [];
      for (const arg of args) {
        if (typeof arg === 'bigint') {
          lowered.push(Number(BigInt.asIntN(32, arg)), Number(BigInt.asIntN(32, arg >> 32n)));
        } else {
          lowered.push(arg);
        }
      }
      const result = value(...lowered);
      if (!returnsI64.has(name)) return result;
      return BigInt.asIntN(64, (BigInt(tempRet0) << 32n) | BigInt(result >>> 0));
    };
  }
  return { exports };
}

async function initFromTwin(init, twinUrl) {
  const { asmFunc, i64Results } = await import(String(twinUrl));
  const original = globalThis.WebAssembly;
  class Instance {
    constructor(_module, imports) {
      this.exports = instantiateTwin(asmFunc, i64Results, imports).exports;
    }
  }
  globalThis.WebAssembly = {
    Module: class Module {},
    Instance,
    instantiate: async (_bytes, imports) => new Instance(null, imports),
  };
  try {
    // The glue never fetches anything when handed bytes; an empty buffer keeps
    // it from downloading a .wasm this path does not use.
    return await init({ module_or_path: new Uint8Array(0) });
  } finally {
    if (original === undefined) delete globalThis.WebAssembly;
    else globalThis.WebAssembly = original;
  }
}

/**
 * Runs a wasm-bindgen `init` natively when WebAssembly works, otherwise through
 * the wasm2js twin at `twinUrl`.
 *
 * @param {(arg?: object) => Promise<unknown>} init  default export of the glue
 * @param {string | URL} twinUrl  `<name>_asm.js` generated beside the glue
 */
export async function initWithFallback(init, twinUrl) {
  if (typeof WebAssembly === 'object') {
    try {
      return await init();
    } catch (err) {
      console.warn('WebAssembly init failed, falling back to the JS build:', err);
    }
  }
  return initFromTwin(init, twinUrl);
}
