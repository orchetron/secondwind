// Same core as secondwind.mjs, loaded as a sandboxed WASM module instead of a native library. Binds
// the identical C ABI. Key property: the module imports nothing (WebAssembly.Module.imports is empty),
// so it can only take bytes in and give bytes back. Runs anywhere WASM does, no build step. Only
// extra primitive beyond the native ABI is sw_alloc, to place input bytes into linear memory.

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { existsSync } from "node:fs";

function wasmPath() {
  if (process.env.SECONDWIND_WASM) return process.env.SECONDWIND_WASM;
  const here = dirname(fileURLToPath(import.meta.url));
  const bundled = join(here, "native", "secondwind.wasm"); // shipped inside the npm package
  if (existsSync(bundled)) return bundled;
  const dev = join(here, "..", "..", "target", "wasm32-unknown-unknown", "release", "secondwind.wasm");
  if (existsSync(dev)) return dev;
  throw new Error("secondwind.wasm not found; build it or set SECONDWIND_WASM");
}

// Instantiate from raw bytes so one factory serves Node (file) and browser (fetch). Empty import
// object is the point: the module asks the host for nothing.
export async function load(source) {
  const bytes = source ?? (await readFile(wasmPath()));
  const { instance } = await WebAssembly.instantiate(bytes, {});
  return new Secondwind(instance);
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

class Secondwind {
  #x; // the wasm exports

  constructor(instance) {
    this.#x = instance.exports;
  }

  // A fresh view every time: any sw_alloc can grow linear memory and detach the old ArrayBuffer.
  #mem() {
    return new Uint8Array(this.#x.memory.buffer);
  }

  #writeBytes(bytes) {
    const ptr = this.#x.sw_alloc(bytes.length);
    this.#mem().set(bytes, ptr);
    return ptr;
  }

  // Marshal one (in_ptr, in_len, out_len_ptr) -> out_ptr call, freeing every buffer exactly once.
  // `lead` prepends a session handle for sw_rewrite; sw_compress/sw_verify take none.
  #call(fn, obj, lead) {
    const input = encoder.encode(JSON.stringify(obj));
    const inPtr = this.#writeBytes(input);
    const outLenPtr = this.#x.sw_alloc(4);
    const args = lead === undefined ? [inPtr, input.length, outLenPtr] : [lead, inPtr, input.length, outLenPtr];
    const retPtr = fn(...args);
    const outLen = new DataView(this.#x.memory.buffer).getUint32(outLenPtr, true);
    const text = decoder.decode(this.#mem().subarray(retPtr, retPtr + outLen));
    this.#x.sw_free(retPtr, outLen);
    this.#x.sw_free(inPtr, input.length);
    this.#x.sw_free(outLenPtr, 4);
    return JSON.parse(text);
  }

  abiVersion() {
    return this.#x.sw_abi_version();
  }

  compress(block, model) {
    return this.#call(this.#x.sw_compress, model ? { block, model } : { block });
  }

  verify(wire, hash) {
    return this.#call(this.#x.sw_verify, { wire, hash }).ok === true;
  }

  // Per-conversation session with the same cross-request freeze as native (resend re-emits
  // byte-identical bytes, cache prefix holds). Offload is in-memory only (sandbox has no disk); only
  // `resolver` is honored from config.
  session({ resolver } = {}) {
    return new Session(this.#x, resolver);
  }
}

class Session {
  #x;
  #ptr;
  totals = { requests: 0, blocks_rewritten: 0, blocks_first_seen: 0, tokens_saved: 0 };

  constructor(x, resolver) {
    this.#x = x;
    const cfg = encoder.encode(JSON.stringify(resolver ? { resolver } : {}));
    const cfgPtr = x.sw_alloc(cfg.length);
    new Uint8Array(x.memory.buffer).set(cfg, cfgPtr);
    this.#ptr = x.sw_session_new(cfgPtr, cfg.length);
    x.sw_free(cfgPtr, cfg.length);
  }

  rewrite(request) {
    const input = encoder.encode(JSON.stringify(request));
    const inPtr = this.#x.sw_alloc(input.length);
    new Uint8Array(this.#x.memory.buffer).set(input, inPtr);
    const outLenPtr = this.#x.sw_alloc(4);
    const retPtr = this.#x.sw_rewrite(this.#ptr, inPtr, input.length, outLenPtr);
    const outLen = new DataView(this.#x.memory.buffer).getUint32(outLenPtr, true);
    const out = JSON.parse(decoder.decode(new Uint8Array(this.#x.memory.buffer).subarray(retPtr, retPtr + outLen)));
    this.#x.sw_free(retPtr, outLen);
    this.#x.sw_free(inPtr, input.length);
    this.#x.sw_free(outLenPtr, 4);
    const s = out.stats ?? {};
    this.totals.requests += 1;
    for (const k of ["blocks_rewritten", "blocks_first_seen", "tokens_saved"]) this.totals[k] += s[k] ?? 0;
    return out;
  }

  stats() {
    return { ...this.totals };
  }

  close() {
    if (this.#ptr) {
      this.#x.sw_session_free(this.#ptr);
      this.#ptr = 0;
    }
  }
}
