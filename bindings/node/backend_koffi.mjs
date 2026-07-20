// Node FFI backend over the C ABI via koffi. Imported only under Node (Bun uses backend_bun.mjs),
// same interface. koffi passes a Buffer for void* and reads return pointers with koffi.decode.

import koffi from "koffi";

const decoder = new TextDecoder();
const encoder = new TextEncoder();

export async function createBackend(libraryPath) {
  const lib = koffi.load(libraryPath);
  const sw_abi_version = lib.func("uint32_t sw_abi_version()");
  const sw_compress = lib.func("void* sw_compress(void* input, size_t len, void* out_len)");
  const sw_verify = lib.func("void* sw_verify(void* input, size_t len, void* out_len)");
  const sw_free = lib.func("void sw_free(void* ptr, size_t len)");
  const sw_session_new = lib.func("void* sw_session_new(void* config, size_t len)");
  const sw_session_free = lib.func("void sw_session_free(void* session)");
  const sw_rewrite = lib.func("void* sw_rewrite(void* session, void* input, size_t len, void* out_len)");
  const sw_session_new_with_store = lib.func(
    "void* sw_session_new_with_store(void* config, size_t len, void* ctx, void* put, void* get)",
  );
  const sw_session_new_with_codec = lib.func(
    "void* sw_session_new_with_codec(void* config, size_t len, void* ctx, void* encode, void* decode)",
  );
  const funcs = { sw_compress, sw_verify };

  // Host-side signatures of the store callbacks.
  const PutProto = koffi.proto("int sw_put(void* ctx, void* id, size_t id_len, void* val, size_t val_len)");
  const GetProto = koffi.proto("void* sw_get(void* ctx, void* id, size_t id_len, void* out_len)");

  const readBytes = (ptr, len) => Buffer.from(koffi.decode(ptr, koffi.array("uint8_t", len)));

  function readReply(retPtr, outLen) {
    const n = Number(outLen.readBigUInt64LE(0));
    const text = readBytes(retPtr, n).toString("utf8");
    sw_free(retPtr, n);
    return text;
  }

  function oneShot(name, bytes) {
    const input = Buffer.from(bytes);
    const outLen = Buffer.alloc(8);
    const retPtr = funcs[name](input, input.length, outLen);
    return readReply(retPtr, outLen);
  }

  function openSession(configBytes, store, codec) {
    const config = Buffer.from(configBytes);
    if (codec) {
      // Host codec competes in best-of-N, proven per-block. Callbacks use the store-get shape
      // (GetProto): bytes in, bytes out via a returned pointer.
      const session = { encBuf: null, decBuf: null };
      const side = (fn, key) =>
        koffi.register((_ctx, input, inLen, outLen) => {
          let result = null;
          try {
            result = fn(readBytes(input, Number(inLen)).toString("utf8"));
          } catch {
            result = null;
          }
          if (typeof result !== "string") {
            koffi.encode(outLen, "size_t", 0);
            return null;
          }
          session[key] = Buffer.from(encoder.encode(result));
          koffi.encode(outLen, "size_t", session[key].length);
          return session[key];
        }, koffi.pointer(GetProto));
      const enc = side((text) => codec.encode(text), "encBuf");
      const dec = side((wire) => codec.decode(wire), "decBuf");
      session.regs = [enc, dec];
      session.handle = sw_session_new_with_codec(config, config.length, null, enc, dec);
      return session;
    }
    if (!store) {
      return { handle: sw_session_new(config, config.length), regs: [] };
    }
    const session = { lastGet: null };
    const putPtr = koffi.register((_ctx, id, idLen, val, valLen) => {
      try {
        const idStr = readBytes(id, Number(idLen)).toString("utf8");
        const valStr = readBytes(val, Number(valLen)).toString("utf8");
        return store.put(idStr, valStr) ? 1 : 0;
      } catch {
        return 0;
      }
    }, koffi.pointer(PutProto));
    const getPtr = koffi.register((_ctx, id, idLen, outLen) => {
      let val = null;
      try {
        val = store.get(readBytes(id, Number(idLen)).toString("utf8"));
      } catch {
        val = null;
      }
      if (val == null) {
        koffi.encode(outLen, "size_t", 0);
        return null;
      }
      session.lastGet = Buffer.from(encoder.encode(val)); // stay alive until the library copies it
      koffi.encode(outLen, "size_t", session.lastGet.length);
      return session.lastGet;
    }, koffi.pointer(GetProto));
    session.regs = [putPtr, getPtr];
    session.handle = sw_session_new_with_store(config, config.length, null, putPtr, getPtr);
    return session;
  }

  function rewrite(session, bytes) {
    const input = Buffer.from(bytes);
    const outLen = Buffer.alloc(8);
    const retPtr = sw_rewrite(session.handle, input, input.length, outLen);
    return readReply(retPtr, outLen);
  }

  function closeSession(session) {
    if (session.handle) {
      sw_session_free(session.handle);
      session.handle = null;
    }
    for (const reg of session.regs ?? []) koffi.unregister(reg);
  }

  return { version: () => sw_abi_version(), oneShot, openSession, rewrite, closeSession };
}
