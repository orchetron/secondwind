// Bun FFI backend over the C ABI. Imported only under Bun (Node can't resolve bun:ffi); mirror of
// backend_koffi.mjs, same interface.

import { dlopen, FFIType, JSCallback, ptr, toArrayBuffer } from "bun:ffi";

const decoder = new TextDecoder();
const P = FFIType.ptr;
const U = FFIType.usize;

export async function createBackend(libraryPath) {
  const { symbols } = dlopen(libraryPath, {
    sw_abi_version: { args: [], returns: FFIType.u32 },
    sw_compress: { args: [P, U, P], returns: P },
    sw_verify: { args: [P, U, P], returns: P },
    sw_free: { args: [P, U], returns: FFIType.void },
    sw_session_new: { args: [P, U], returns: P },
    sw_session_new_with_store: { args: [P, U, P, P, P], returns: P },
    sw_session_new_with_codec: { args: [P, U, P, P, P], returns: P },
    sw_session_free: { args: [P], returns: FFIType.void },
    sw_rewrite: { args: [P, P, U, P], returns: P },
  });

  // Decode the library-allocated JSON reply, then free it exactly once.
  function readReply(retPtr, outLen) {
    const len = Number(outLen[0]);
    const text = decoder.decode(toArrayBuffer(retPtr, 0, len));
    symbols.sw_free(retPtr, len);
    return text;
  }

  function oneShot(name, bytes) {
    const outLen = new BigUint64Array(1);
    const retPtr = symbols[name](ptr(bytes), bytes.length, ptr(outLen));
    return readReply(retPtr, outLen);
  }

  function openSession(configBytes, store, codec) {
    if (codec) {
      // Host codec competes in best-of-N; each block is proven per-round-trip so a wrong codec is
      // dropped. Callbacks use the get-callback shape.
      const session = { encBuf: null, decBuf: null };
      const side = (fn, key) =>
        new JSCallback(
          (_ctx, inPtr, inLen, outLenPtr) => {
            let result = null;
            try {
              result = fn(decoder.decode(toArrayBuffer(inPtr, 0, Number(inLen))));
            } catch {
              result = null;
            }
            const outLen = new BigUint64Array(toArrayBuffer(outLenPtr, 0, 8));
            if (typeof result !== "string") {
              outLen[0] = 0n;
              return 0;
            }
            session[key] = new TextEncoder().encode(result); // stay alive until the library copies it
            outLen[0] = BigInt(session[key].length);
            return ptr(session[key]);
          },
          { args: [P, P, U, P], returns: P },
        );
      const enc = side((text) => codec.encode(text), "encBuf");
      const dec = side((wire) => codec.decode(wire), "decBuf");
      session.cbs = [enc, dec];
      session.handle = symbols.sw_session_new_with_codec(ptr(configBytes), configBytes.length, null, enc.ptr, dec.ptr);
      return session;
    }
    if (!store) {
      return { handle: symbols.sw_session_new(ptr(configBytes), configBytes.length), cbs: [] };
    }
    const session = { lastGet: null };
    const putCb = new JSCallback(
      (_ctx, idPtr, idLen, valPtr, valLen) => {
        try {
          const id = decoder.decode(toArrayBuffer(idPtr, 0, Number(idLen)));
          const val = decoder.decode(toArrayBuffer(valPtr, 0, Number(valLen)));
          return store.put(id, val) ? 1 : 0;
        } catch {
          return 0;
        }
      },
      { args: [P, P, U, P, U], returns: FFIType.i32 },
    );
    const getCb = new JSCallback(
      (_ctx, idPtr, idLen, outLenPtr) => {
        let val = null;
        try {
          val = store.get(decoder.decode(toArrayBuffer(idPtr, 0, Number(idLen))));
        } catch {
          val = null;
        }
        const outLen = new BigUint64Array(toArrayBuffer(outLenPtr, 0, 8));
        if (val == null) {
          outLen[0] = 0n;
          return 0;
        }
        session.lastGet = new TextEncoder().encode(val); // stay alive until the library copies it
        outLen[0] = BigInt(session.lastGet.length);
        return ptr(session.lastGet);
      },
      { args: [P, P, U, P], returns: P },
    );
    session.cbs = [putCb, getCb];
    session.handle = symbols.sw_session_new_with_store(ptr(configBytes), configBytes.length, null, putCb.ptr, getCb.ptr);
    return session;
  }

  function rewrite(session, bytes) {
    const outLen = new BigUint64Array(1);
    const retPtr = symbols.sw_rewrite(session.handle, ptr(bytes), bytes.length, ptr(outLen));
    return readReply(retPtr, outLen);
  }

  function closeSession(session) {
    if (session.handle) {
      symbols.sw_session_free(session.handle);
      session.handle = null;
    }
    for (const cb of session.cbs ?? []) cb.close();
  }

  return { version: () => symbols.sw_abi_version(), oneShot, openSession, rewrite, closeSession };
}
