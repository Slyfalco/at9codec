"""at9codec — Python binding (ctypes).

Loads the at9codec shared library directly (no wheel/build step) and wraps the
options-struct C API with idiomatic helpers:

    import at9codec as a9
    pcm, channels, rate = a9.decode(open("in.at9", "rb").read())
    blob = a9.encode_wav(open("in.wav", "rb").read(),
                         loop=(0, None),               # whole-track loop
                         size_profile=a9.SIZE_VITA_BGM,  # codec owns the limit
                         progress=lambda d, t: print(f"{d}/{t}"))
    open("out.at9", "wb").write(blob)

The codec accepts any input sample rate (it resamples to a console rate itself)
and any WAV bit depth. See ../README.md for how the library is located.
"""

import ctypes
import os
import sys

AT9_OK = 0
AT9_ERR_BITRATE = -5    # bitrate is not a standard rate (use 0 = auto)
AT9_ERR_TOO_LARGE = -6  # too long to fit the size limit at any standard bitrate

# Size profiles: name a console target and the codec applies its own byte cap.
SIZE_NONE = 0
SIZE_VITA_BGM = 1       # PS Vita system/theme BGM (under 5 MB)
SIZE_PS_CROSS = 2       # tighter 2 MiB cap for PS4/PS5 cross-support

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
                ("user", ctypes.c_void_p),
                ("size_profile", ctypes.c_uint32)]


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
    lib.at9_profile_max_bytes.restype = ctypes.c_size_t
    lib.at9_profile_max_bytes.argtypes = [ctypes.c_uint32]
    # Streaming (since v2.1).
    lib.at9_stream_open.restype = ctypes.c_int32
    lib.at9_stream_open.argtypes = [ctypes.c_char_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_void_p)]
    for fn, res in (("at9_stream_channels", ctypes.c_uint16), ("at9_stream_sample_rate", ctypes.c_uint32),
                    ("at9_stream_total_frames", ctypes.c_uint64), ("at9_stream_position", ctypes.c_uint64)):
        getattr(lib, fn).restype = res
        getattr(lib, fn).argtypes = [ctypes.c_void_p]
    lib.at9_stream_loop.restype = ctypes.c_int32
    lib.at9_stream_loop.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint64), ctypes.POINTER(ctypes.c_uint64)]
    lib.at9_stream_read.restype = ctypes.c_size_t
    lib.at9_stream_read.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_int16), ctypes.c_size_t]
    lib.at9_stream_seek.restype = None
    lib.at9_stream_seek.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
    lib.at9_stream_close.restype = None
    lib.at9_stream_close.argtypes = [ctypes.c_void_p]
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


def profile_max_bytes(size_profile):
    """The codec's byte cap for a size profile (SIZE_*); 0 = no cap/unknown. Reads
    the limit from the library so you never hard-code the console numbers."""
    return int(_lib_handle().at9_profile_max_bytes(size_profile))


def encode_wav(wav_bytes, bitrate=0, size_profile=SIZE_NONE, max_bytes=0,
               loop=None, progress=None):
    """Encode a WAV file (bytes) to AT9 bytes.

    loop: None = no loop; a (start, end) tuple where start is 0 for a whole-track
    loop or a sample index, and end is None (to track end) or an inclusive last
    sample. size_profile: SIZE_VITA_BGM / SIZE_PS_CROSS to let the codec apply its
    own console size cap; SIZE_NONE (default) uses max_bytes (0 = uncapped)."""
    lib = _lib_handle()
    cb = _wrap(progress)
    if loop is None:
        ls, le = -1, 0
    else:
        start, end = loop
        ls = int(start)
        le = int(end) if end else 0
    opts = _EncodeOptions(struct_size=ctypes.sizeof(_EncodeOptions), bitrate=bitrate,
                          max_bytes=max_bytes, loop_start=ls, loop_end=le, progress=cb,
                          user=None, size_profile=size_profile)
    out = ctypes.POINTER(ctypes.c_uint8)()
    n = ctypes.c_size_t()
    rc = lib.at9_encode_wav(wav_bytes, len(wav_bytes), ctypes.byref(opts),
                            ctypes.byref(out), ctypes.byref(n))
    if rc == AT9_ERR_TOO_LARGE:
        raise RuntimeError("at9_encode_wav: track too long to fit the size limit")
    if rc != AT9_OK:
        raise RuntimeError(f"at9_encode_wav failed: {rc}")
    try:
        return ctypes.string_at(out, n.value)
    finally:
        lib.at9_free_at9(out, n)


class Stream:
    """Pull-based decoder for playback (since v2.1): read PCM in small chunks as
    your audio device needs it, and seek to loop. Output is bit-identical to
    decode(). Use it from one thread at a time; close it (or use `with`).

        with a9.Stream(open("bgm.at9", "rb").read()) as s:
            loop = s.loop                      # (start, end) frames or None
            while chunk := s.read(4096):       # bytes of interleaved 16-bit PCM
                play(chunk)
            if loop:
                s.seek(loop[0])                # jump back to loop
    """

    def __init__(self, at9_bytes):
        self._lib = _lib_handle()
        self._h = ctypes.c_void_p()
        rc = self._lib.at9_stream_open(at9_bytes, len(at9_bytes), ctypes.byref(self._h))
        if rc != AT9_OK:
            raise RuntimeError(f"at9_stream_open failed: {rc}")

    @property
    def channels(self):
        return int(self._lib.at9_stream_channels(self._h))

    @property
    def sample_rate(self):
        return int(self._lib.at9_stream_sample_rate(self._h))

    @property
    def total_frames(self):
        """Un-looped length in frames."""
        return int(self._lib.at9_stream_total_frames(self._h))

    @property
    def position(self):
        return int(self._lib.at9_stream_position(self._h))

    @property
    def loop(self):
        """(start, end) loop region in frames (end inclusive), or None."""
        a, b = ctypes.c_uint64(), ctypes.c_uint64()
        if self._lib.at9_stream_loop(self._h, ctypes.byref(a), ctypes.byref(b)):
            return int(a.value), int(b.value)
        return None

    def read(self, sample_count):
        """Up to `sample_count` interleaved samples as little-endian 16-bit bytes;
        b"" once the stream has ended."""
        buf = (ctypes.c_int16 * sample_count)()
        n = self._lib.at9_stream_read(self._h, buf, sample_count)
        return ctypes.string_at(buf, n * 2)

    def seek(self, frame):
        """Reposition to a frame (sample-accurate)."""
        self._lib.at9_stream_seek(self._h, frame)

    def close(self):
        if self._h:
            self._lib.at9_stream_close(self._h)
            self._h = ctypes.c_void_p()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def __del__(self):
        try:
            self.close()
        except Exception:
            pass
