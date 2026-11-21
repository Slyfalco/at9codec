"""at9codec — Python binding (ctypes).

Loads the at9codec shared library directly (no wheel/build step) and wraps the
options-struct C API with idiomatic helpers:

    import at9codec as a9
    pcm, channels, rate = a9.decode(open("in.at9", "rb").read())
    blob = a9.encode_wav(open("in.wav", "rb").read(),
                         loop=(0, None),               # whole-track loop
                         max_bytes=a9.VITA_BGM_MAX_BYTES,
                         progress=lambda d, t: print(f"{d}/{t}"))
    open("out.at9", "wb").write(blob)

The codec accepts any input sample rate (it resamples to a console rate itself)
and any WAV bit depth. See ../README.md for how the library is located.
"""

import ctypes
import os
import sys

AT9_OK = 0
VITA_BGM_MAX_BYTES = 5 * 1024 * 1024
PS_CROSS_BGM_MAX_BYTES = 2 * 1024 * 1024

_ProgressCb = ctypes.CFUNCTYPE(None, ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32)


class _DecodeOptions(ctypes.Structure):
    _fields_ = [("struct_size", ctypes.c_uint32),
                ("progress", _ProgressCb),
                ("user", ctypes.c_void_p)]


class _EncodeOptions(ctypes.Structure):
    _fields_ = [("struct_size", ctypes.c_uint32),
                ("bitrate", ctypes.c_uint32),
                ("max_bytes", ctypes.c_size_t),
                ("loop_start", ctypes.c_int32),
                ("loop_end", ctypes.c_int32),
                ("progress", _ProgressCb),
                ("user", ctypes.c_void_p)]


def _default_lib_name():
    if sys.platform == "win32":
        return "at9codec.dll"
    if sys.platform == "darwin":
        return "libat9codec.dylib"
    return "libat9codec.so"


def _load(path=None):
    """Load the library. `path` may be a file or a directory; by default we look
    beside this file and then let the OS resolver search its usual paths."""
    name = _default_lib_name()
    candidates = []
    if path:
        candidates.append(path if os.path.isfile(path) else os.path.join(path, name))
    candidates.append(os.path.join(os.path.dirname(os.path.abspath(__file__)), name))
    candidates.append(name)  # system search path
    last = None
    for c in candidates:
        try:
            lib = ctypes.CDLL(c)
            break
        except OSError as e:
            last = e
    else:
        raise OSError(f"could not load {name}: {last}")

    lib.at9_version.restype = ctypes.c_char_p
    lib.at9_decode.restype = ctypes.c_int32
    lib.at9_decode.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(_DecodeOptions),
                               ctypes.POINTER(ctypes.POINTER(ctypes.c_int16)), ctypes.POINTER(ctypes.c_size_t),
                               ctypes.POINTER(ctypes.c_uint16), ctypes.POINTER(ctypes.c_uint32)]
    lib.at9_free_pcm.argtypes = [ctypes.POINTER(ctypes.c_int16), ctypes.c_size_t]
    lib.at9_encode_wav.restype = ctypes.c_int32
    lib.at9_encode_wav.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(_EncodeOptions),
                                   ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)), ctypes.POINTER(ctypes.c_size_t)]
    lib.at9_free_at9.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]
    return lib


_lib = None


def _lib_handle():
    global _lib
    if _lib is None:
        _lib = _load(os.environ.get("AT9CODEC_LIB"))
    return _lib


def version():
    v = _lib_handle().at9_version()
    return v.decode("ascii", "replace") if v else None


def _wrap(progress):
    if progress is None:
        return ctypes.cast(None, _ProgressCb)
    return _ProgressCb(lambda _u, d, t: progress(int(d), int(t)))


def decode(at9_bytes, progress=None):
    """Decode an AT9 file (bytes) to (pcm_bytes, channels, sample_rate), where
    pcm_bytes is interleaved little-endian 16-bit PCM."""
    lib = _lib_handle()
    cb = _wrap(progress)
    opts = _DecodeOptions(struct_size=ctypes.sizeof(_DecodeOptions), progress=cb, user=None)
    out = ctypes.POINTER(ctypes.c_int16)()
    n = ctypes.c_size_t()
    ch = ctypes.c_uint16()
    sr = ctypes.c_uint32()
    rc = lib.at9_decode(at9_bytes, len(at9_bytes), ctypes.byref(opts),
                        ctypes.byref(out), ctypes.byref(n), ctypes.byref(ch), ctypes.byref(sr))
    if rc != AT9_OK:
        raise RuntimeError(f"at9_decode failed: {rc}")
    try:
        pcm = ctypes.string_at(out, n.value * 2)
    finally:
        lib.at9_free_pcm(out, n)
    return pcm, int(ch.value), int(sr.value)


def encode_wav(wav_bytes, bitrate=0, max_bytes=0, loop=None, progress=None):
    """Encode a WAV file (bytes) to AT9 bytes.

    loop: None = no loop; a (start, end) tuple where start is 0 for a whole-track
    loop or a sample index, and end is None (to track end) or an inclusive last
    sample. max_bytes: 0 = uncapped; VITA_BGM_MAX_BYTES / PS_CROSS_BGM_MAX_BYTES
    for theme BGM."""
    lib = _lib_handle()
    cb = _wrap(progress)
    if loop is None:
        ls, le = -1, 0
    else:
        start, end = loop
        ls = int(start)
        le = int(end) if end else 0
    opts = _EncodeOptions(struct_size=ctypes.sizeof(_EncodeOptions), bitrate=bitrate,
                          max_bytes=max_bytes, loop_start=ls, loop_end=le, progress=cb, user=None)
    out = ctypes.POINTER(ctypes.c_uint8)()
    n = ctypes.c_size_t()
    rc = lib.at9_encode_wav(wav_bytes, len(wav_bytes), ctypes.byref(opts),
                            ctypes.byref(out), ctypes.byref(n))
    if rc != AT9_OK:
        raise RuntimeError(f"at9_encode_wav failed: {rc}")
    try:
        return ctypes.string_at(out, n.value)
    finally:
        lib.at9_free_at9(out, n)
