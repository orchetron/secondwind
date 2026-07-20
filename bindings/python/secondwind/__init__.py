"""secondwind: lossless, provable, model-free LLM context compression.

Thin ctypes binding over the bundled C ABI (no native extension); the first caller of a C ABI
every language calls the same way.
"""

import ctypes
import json
import os


def _library_path() -> str:
    if os.environ.get("SECONDWIND_LIB"):
        return os.environ["SECONDWIND_LIB"]
    here = os.path.dirname(__file__)
    names = ("libsecondwind.dylib", "libsecondwind.so", "secondwind.dll")
    # Bundled inside the wheel.
    for name in names:
        bundled = os.path.join(here, "_lib", name)
        if os.path.exists(bundled):
            return bundled
    # Dev fallback: source-checkout cargo target dir.
    target = os.path.join(here, "..", "..", "..", "target", "release")
    for name in names:
        dev = os.path.join(target, name)
        if os.path.exists(dev):
            return dev
    raise OSError("secondwind native library not found; build it or set SECONDWIND_LIB")


_lib = ctypes.CDLL(_library_path())
_lib.sw_abi_version.restype = ctypes.c_uint32
for _fn in ("sw_compress", "sw_verify"):
    f = getattr(_lib, _fn)
    f.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
    f.restype = ctypes.POINTER(ctypes.c_ubyte)
_lib.sw_free.argtypes = [ctypes.POINTER(ctypes.c_ubyte), ctypes.c_size_t]
_lib.sw_session_new.argtypes = [ctypes.c_char_p, ctypes.c_size_t]
_lib.sw_session_new.restype = ctypes.c_void_p
_lib.sw_session_free.argtypes = [ctypes.c_void_p]
_lib.sw_rewrite.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
_lib.sw_rewrite.restype = ctypes.POINTER(ctypes.c_ubyte)

# Host offload backend: put(ctx, id, id_len, val, val_len) -> int; get(ctx, id, id_len, out_len) -> bytes*.
_PUT = ctypes.CFUNCTYPE(
    ctypes.c_int, ctypes.c_void_p, ctypes.POINTER(ctypes.c_ubyte), ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_ubyte), ctypes.c_size_t,
)
_GET = ctypes.CFUNCTYPE(
    ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_ubyte), ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_size_t),
)
_lib.sw_session_new_with_store.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_void_p, _PUT, _GET]
_lib.sw_session_new_with_store.restype = ctypes.c_void_p

# Host codec: encode/decode take input bytes, return an output ptr (len via out_len) or null; same
# shape as the store's get callback.
_CODEC = ctypes.CFUNCTYPE(
    ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_ubyte), ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_size_t),
)
_lib.sw_session_new_with_codec.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.c_void_p, _CODEC, _CODEC]
_lib.sw_session_new_with_codec.restype = ctypes.c_void_p


def _call(fn, payload: dict) -> dict:
    data = json.dumps(payload).encode("utf-8")
    out_len = ctypes.c_size_t(0)
    ptr = fn(data, len(data), ctypes.byref(out_len))
    try:
        return json.loads(ctypes.string_at(ptr, out_len.value))
    finally:
        _lib.sw_free(ptr, out_len.value)


def abi_version() -> int:
    return _lib.sw_abi_version()


def compress(block: str, model: str | None = None) -> dict:
    """Losslessly compress one tool-output block. Returns the outcome dict."""
    payload = {"block": block}
    if model:
        payload["model"] = model
    return _call(_lib.sw_compress, payload)


def verify(wire: str, cert_hash: str) -> bool:
    """Independently confirm a wire is lossless against its certificate hash (no trust required)."""
    return _call(_lib.sw_verify, {"wire": wire, "hash": cert_hash}).get("ok", False)


class Session:
    """Per-conversation handle: freeze memory keeps resends byte-identical so only aged blocks
    offload. `home` books ledger events for `secondwind proof --home <dir>`."""

    def __init__(self, resolver=None, offload_dir=None, home=None, store=None, proposers=True, codec=None):
        """`store`: any object with put(id, value) -> bool and get(id) -> str | None, so offload can
        be backed by Redis/S3/a database. `proposers` toggles the best-of-N codec search (on; off is a
        cost/latency choice, output is proven lossless either way). `codec`: any object with
        encode(text) and decode(wire); it competes in the search and is dropped unless
        decode(encode(x)) == x per block."""
        config: dict = {"proposers": bool(proposers)}
        if resolver:
            config["resolver"] = resolver
        if offload_dir:
            config["offload_dir"] = offload_dir
        if home:
            config["home"] = home
        data = json.dumps(config).encode("utf-8")
        if codec is not None:
            self._ptr = self._open_with_codec(data, codec)
        elif store is None:
            self._ptr = _lib.sw_session_new(data, len(data))
        else:
            self._ptr = self._open_with_store(data, store)
        self._totals = {"requests": 0, "blocks_rewritten": 0, "blocks_first_seen": 0, "tokens_saved": 0}

    def _open_with_codec(self, config: bytes, codec):
        self._codec = codec
        self._enc_buf = None  # keep each result alive until secondwind copies it
        self._dec_buf = None

        def side(method, hold):
            def cb(_ctx, in_ptr, in_len, out_len):
                try:
                    result = method(ctypes.string_at(in_ptr, in_len).decode("utf-8"))
                except Exception:
                    result = None
                if not isinstance(result, str):
                    out_len[0] = 0
                    return None
                raw = result.encode("utf-8")
                buf = (ctypes.c_ubyte * len(raw)).from_buffer_copy(raw)
                setattr(self, hold, buf)
                out_len[0] = len(raw)
                return ctypes.addressof(buf)

            return cb

        self._enc_cb = _CODEC(side(codec.encode, "_enc_buf"))
        self._dec_cb = _CODEC(side(codec.decode, "_dec_buf"))
        return _lib.sw_session_new_with_codec(config, len(config), None, self._enc_cb, self._dec_cb)

    def _open_with_store(self, config: bytes, store):
        self._store = store
        self._get_buf = None  # keeps the last get result alive until the next call

        def put_cb(_ctx, id_ptr, id_len, val_ptr, val_len):
            try:
                key = ctypes.string_at(id_ptr, id_len).decode("utf-8")
                val = ctypes.string_at(val_ptr, val_len).decode("utf-8")
                return 1 if store.put(key, val) else 0
            except Exception:
                return 0

        def get_cb(_ctx, id_ptr, id_len, out_len):
            try:
                key = ctypes.string_at(id_ptr, id_len).decode("utf-8")
                val = store.get(key)
            except Exception:
                val = None
            if val is None:
                out_len[0] = 0
                return None
            raw = val.encode("utf-8")
            self._get_buf = (ctypes.c_ubyte * len(raw)).from_buffer_copy(raw)
            out_len[0] = len(raw)
            return ctypes.addressof(self._get_buf)

        # Keep the callback objects alive on self, or they are garbage-collected and the FFI crashes.
        self._put_cb = _PUT(put_cb)
        self._get_cb = _GET(get_cb)
        return _lib.sw_session_new_with_store(config, len(config), None, self._put_cb, self._get_cb)

    def rewrite(self, request: dict) -> dict:
        """Compress every tool-output block in a whole LLM request. Returns {"request", "stats"}."""
        data = json.dumps(request).encode("utf-8")
        out_len = ctypes.c_size_t(0)
        ptr = _lib.sw_rewrite(self._ptr, data, len(data), ctypes.byref(out_len))
        try:
            out = json.loads(ctypes.string_at(ptr, out_len.value))
        finally:
            _lib.sw_free(ptr, out_len.value)
        stats = out.get("stats", {})
        self._totals["requests"] += 1
        for key in ("blocks_rewritten", "blocks_first_seen", "tokens_saved"):
            self._totals[key] += stats.get(key, 0)
        return out

    def stats(self) -> dict:
        """Running totals for this session (no proxy needed)."""
        return dict(self._totals)

    def close(self):
        if self._ptr:
            _lib.sw_session_free(self._ptr)
            self._ptr = None

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()
