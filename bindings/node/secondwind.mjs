// Native, in-process context compression over the C ABI. Binds the shared library through Bun's FFI
// under Bun and koffi under Node; same .dylib/.so/.dll, identical backend interface.
// For a no-build sandboxed option, import "secondwind/wasm" instead.

import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { existsSync } from "node:fs";
import { platform, arch } from "node:process";

// Native lib ships as per-platform optional packages (secondwind-<os>-<arch>); npm's os/cpu guard
// installs only the matching one. This resolves whichever landed.
function libraryPath() {
  if (process.env.SECONDWIND_LIB) return process.env.SECONDWIND_LIB;
  const libName = platform === "win32" ? "secondwind.dll" : platform === "darwin" ? "libsecondwind.dylib" : "libsecondwind.so";
  const require = createRequire(import.meta.url);
  try {
    return require.resolve(`secondwind-${platform}-${arch}/${libName}`);
  } catch {
    // fall through to a bundled or source-checkout copy
  }
  const here = dirname(fileURLToPath(import.meta.url));
  const bundled = join(here, "native", libName);
  if (existsSync(bundled)) return bundled;
  const dev = join(here, "..", "..", "target", "release", libName);
  if (existsSync(dev)) return dev;
  throw new Error(`secondwind native library not found for ${platform}-${arch}; set SECONDWIND_LIB or install secondwind-${platform}-${arch}`);
}

const isBun = typeof globalThis.Bun !== "undefined";
const { createBackend } = await import(isBun ? "./backend_bun.mjs" : "./backend_koffi.mjs");
const backend = await createBackend(libraryPath());

const encoder = new TextEncoder();
const encode = (obj) => encoder.encode(JSON.stringify(obj));

export const abiVersion = () => backend.version();

export function compress(block, model) {
  return JSON.parse(backend.oneShot("sw_compress", encode(model ? { block, model } : { block })));
}

export function verify(wire, hash) {
  return JSON.parse(backend.oneShot("sw_verify", encode({ wire, hash }))).ok === true;
}

export class Session {
  #session;

  // `store`: object with put(id, value) -> bool and get(id) -> string|null; backs offload with any
  // backend. `codec`: object with encode/decode -> string|null; competes in best-of-N, proven
  // per-block, dropped if it ever fails.
  constructor({ resolver, offloadDir, home, store, proposers = true, codec } = {}) {
    const config = { proposers: proposers !== false };
    if (resolver) config.resolver = resolver;
    if (offloadDir) config.offload_dir = offloadDir;
    if (home) config.home = home; // ledger home; `secondwind proof --home <dir>` reads it
    this.#session = backend.openSession(encode(config), store ?? null, codec ?? null);
    this.totals = { requests: 0, blocks_rewritten: 0, blocks_first_seen: 0, tokens_saved: 0 };
  }

  rewrite(request) {
    const out = JSON.parse(backend.rewrite(this.#session, encode(request)));
    const s = out.stats ?? {};
    this.totals.requests += 1;
    for (const k of ["blocks_rewritten", "blocks_first_seen", "tokens_saved"]) this.totals[k] += s[k] ?? 0;
    return out;
  }

  stats() {
    return { ...this.totals };
  }

  close() {
    backend.closeSession(this.#session);
  }
}
