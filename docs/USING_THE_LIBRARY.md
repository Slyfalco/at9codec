# Using the at9codec shared library

`at9codec` ships as a C-ABI shared library you can link from any language — no CLI
tool, subprocess, or external dependency is required. This document covers building
the library and calling it from C/C++, Python, and C#.

The full API is declared in [`include/at9codec.h`](../include/at9codec.h).

## Building the library

The `at9codec` crate is both an `rlib` and a `cdylib`, so a normal release build
produces the shared library:

```sh
cargo build --release
```

Output (under `target/release/`):

| Platform | Shared library        | Link/import library            |
|----------|-----------------------|--------------------------------|
| Windows  | `at9codec.dll`        | `at9codec.dll.lib` (import lib) |
| Linux    | `libat9codec.so`      | —                              |
| macOS    | `libat9codec.dylib`   | —                              |

On Windows, link against `at9codec.dll.lib` at build time and ship `at9codec.dll`
next to your executable (or on `PATH`). On Linux/macOS link `-lat9codec` with
`-L target/release`, and make sure the `.so`/`.dylib` is on the loader path at run
time (`LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`, an rpath, or a system lib dir).

The header is standalone and depends only on `<stddef.h>` and `<stdint.h>`.

## API overview

| Function           | Purpose                                             |
|--------------------|-----------------------------------------------------|
| `at9_version()`    | Library version string.                             |
| `at9_parse_info()` | Read an AT9 file's header into an `At9Info` struct. |
| `at9_decode()`     | AT9 file → interleaved 16-bit PCM.                  |
| `at9_free_pcm()`   | Free a buffer from `at9_decode()`.                  |
| `at9_encode()`     | Interleaved 16-bit PCM → AT9 file.                  |
| `at9_encode_wav()` | WAV file buffer → AT9 file.                         |
| `at9_free_at9()`   | Free a buffer from the encode functions.            |
| `at9_decode_ex()`  | `at9_decode()` plus a progress callback.            |
| `at9_encode_ex()`  | `at9_encode()` plus loop points and progress.       |
| `at9_encode_wav_ex()` | `at9_encode_wav()` plus loop points and progress. |

**Status codes** (returned by the `int32_t` functions): `AT9_OK` (0), plus negative
`AT9_ERR_NULL`, `AT9_ERR_PARSE`, `AT9_ERR_DECODE`, `AT9_ERR_ENCODE`.

**Supported encode input:** interleaved 16-bit PCM (mono or stereo) at 12000,
24000, or 48000 Hz. `at9_encode_wav()` additionally accepts 8/16/24/32-bit integer
and 32/64-bit float WAV, converting to 16-bit internally (no resampling). Pass
`bitrate = 0` for a sensible default for the channel count.

### Memory ownership

Functions that produce a buffer allocate it inside the library and write a pointer
plus a length to your out-parameters. Release each buffer with its matching
`at9_free_*` function — **never** with your own `free()`/`delete`, and never mix the
two free functions. Pass the exact length the function returned back to the free
function.

### Loop points and progress

The `_ex` encode functions take a `loop_start` argument: negative = no loop, `0` =
loop the whole track, positive = loop from that sample index (decoded-PCM
coordinates) to the end. The loop is written into the AT9 container's `smpl`
chunk and read back by `at9_parse_info()` (`has_loop`/`loop_start`/`loop_end`/
`is_whole_loop`).

They also take a `max_bytes` cap: `0` = no limit (general game AT9, up to the RIFF
container's 4 GiB), or a byte limit that the encoder keeps the output at or under
by choosing the highest standard bitrate that fits. Use `AT9_VITA_BGM_MAX_BYTES`
(~5 MB) for PS Vita theme BGM, or `AT9_PS_CROSS_BGM_MAX_BYTES` (2 MiB) for a theme
that must also work on PS4/PS5. The Vita refuses oversized theme BGM, so this
keeps a default-settings encode usable out of the box.

All `_ex` functions also accept an `At9ProgressCb cb` and a `void *user`. The
callback receives `(user, done, total)` in superframes. During encode it runs on
one dedicated monitor thread (the encode workers never call it), so it adds no
measurable cost; pass `NULL` to skip it entirely (no monitor thread is spawned).

### Thread-safety

All functions are stateless and re-entrant. You may call them concurrently from
multiple threads as long as each call works on its own buffers.

## C / C++

```c
#include <stdio.h>
#include "at9codec.h"

int main(void) {
    /* ... fill `pcm` with sample_count interleaved int16 samples ... */

    uint8_t *at9 = NULL; size_t at9_len = 0;
    if (at9_encode(pcm, sample_count, /*channels*/ 2, /*rate*/ 48000,
                   /*bitrate*/ 0, &at9, &at9_len) != AT9_OK) {
        return 1;
    }

    int16_t *out = NULL; size_t out_count = 0; uint16_t ch; uint32_t sr;
    if (at9_decode(at9, at9_len, &out, &out_count, &ch, &sr) != AT9_OK) {
        at9_free_at9(at9, at9_len);
        return 1;
    }
    printf("decoded %zu samples, %u Hz, %u ch\n", out_count, sr, ch);

    at9_free_pcm(out, out_count);
    at9_free_at9(at9, at9_len);
    return 0;
}
```

Build (Windows, MSVC):

```bat
cl /I include use_lib.c /link target\release\at9codec.dll.lib
```

Build (Linux/macOS):

```sh
cc -I include use_lib.c -L target/release -lat9codec -o use_lib
```

## Python (ctypes)

No wheel needed — load the shared library directly:

```python
import ctypes, os, struct

lib = ctypes.CDLL(os.path.abspath("target/release/at9codec.dll"))  # or .so/.dylib

lib.at9_encode.argtypes = [
    ctypes.POINTER(ctypes.c_int16), ctypes.c_size_t, ctypes.c_uint16,
    ctypes.c_uint32, ctypes.c_uint32,
    ctypes.POINTER(ctypes.POINTER(ctypes.c_uint8)), ctypes.POINTER(ctypes.c_size_t),
]
lib.at9_encode.restype = ctypes.c_int32
lib.at9_free_at9.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t]

def encode(samples, channels, sample_rate, bitrate=0):
    arr = (ctypes.c_int16 * len(samples))(*samples)
    out = ctypes.POINTER(ctypes.c_uint8)()
    out_len = ctypes.c_size_t()
    rc = lib.at9_encode(arr, len(samples), channels, sample_rate, bitrate,
                        ctypes.byref(out), ctypes.byref(out_len))
    if rc != 0:
        raise RuntimeError(f"at9_encode failed: {rc}")
    data = bytes(out[i] for i in range(out_len.value))
    lib.at9_free_at9(out, out_len)
    return data
```

Declare `at9_decode` / `at9_free_pcm` the same way (see the header for signatures).

## C# (P/Invoke)

```csharp
using System.Runtime.InteropServices;

static class At9 {
    [DllImport("at9codec", CallingConvention = CallingConvention.Cdecl)]
    public static extern int at9_encode(
        short[] pcm, nuint sampleCount, ushort channels, uint sampleRate,
        uint bitrate, out nint outAt9, out nuint outLen);

    [DllImport("at9codec", CallingConvention = CallingConvention.Cdecl)]
    public static extern void at9_free_at9(nint at9, nuint len);
}
```

Copy the resulting bytes out of `outAt9` (`Marshal.Copy`) before calling
`at9_free_at9`.

## Verifying the build

The repo's standalone C sample encodes a tone to AT9, decodes it back, and prints a
round-trip SNR (~60 dB for a clean tone), demonstrating that the library works with
no CLI involvement.
