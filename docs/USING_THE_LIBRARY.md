# Using the at9codec shared library

`at9codec` ships as a C-ABI shared library you can link from any language — no CLI
tool, subprocess, or external dependency is required. The full API is declared in
[`include/at9codec.h`](../include/at9codec.h); ready-to-use bindings for several
languages live in [`bindings/`](../bindings/).

## Building the library

The `at9codec` crate is both an `rlib` and a `cdylib`, so a normal release build
produces the shared library:

```sh
cargo build --release
```

Output (under `target/release/`):

| Platform | Shared library      | Link/import library             |
|----------|---------------------|---------------------------------|
| Windows  | `at9codec.dll`      | `at9codec.dll.lib` (import lib)  |
| Linux    | `libat9codec.so`    | —                               |
| macOS    | `libat9codec.dylib` | —                               |

On Windows, link against `at9codec.dll.lib` and ship `at9codec.dll` next to your
executable (or on `PATH`). On Linux/macOS link `-lat9codec -L target/release` and
put the `.so`/`.dylib` on the loader path at run time (`LD_LIBRARY_PATH` /
`DYLD_LIBRARY_PATH`, an rpath, or a system lib dir). Prebuilt libraries for every
OS/arch are published by the repo's CI (see the releases).

The header is standalone — it needs only `<stddef.h>` and `<stdint.h>`.

## API at a glance

| Function           | Purpose                                                   |
|--------------------|-----------------------------------------------------------|
| `at9_version()`    | Library version string.                                   |
| `at9_parse_info()` | Read an AT9 header into an `At9Info` struct.              |
| `at9_decode()`     | AT9 → interleaved 16-bit PCM (optional `At9DecodeOptions`).|
| `at9_free_pcm()`   | Free a buffer from `at9_decode()`.                        |
| `at9_encode()`     | 16-bit PCM → AT9 (optional `At9EncodeOptions`).           |
| `at9_encode_wav()` | WAV buffer → AT9 (optional `At9EncodeOptions`).           |
| `at9_free_at9()`   | Free a buffer from the encode functions.                 |

The `int32_t` functions return `AT9_OK` (0) or a negative code (`AT9_ERR_NULL`,
`AT9_ERR_PARSE`, `AT9_ERR_DECODE`, `AT9_ERR_ENCODE`).

### Options structs (how optional arguments work)

A C ABI has no optional or keyword arguments, so optional behaviour is passed in a
small **options struct**: zero-initialize it, set `struct_size = sizeof(...)` and
only the fields you need, and pass its address — **or pass `NULL` for all
defaults.** The `struct_size` field lets the library add fields later without
breaking existing callers.

```c
typedef struct {
    uint32_t      struct_size;   /* = sizeof(At9EncodeOptions)                 */
    uint32_t      bitrate;       /* bits/sec; 0 = best standard default        */
    size_t        max_bytes;     /* custom cap; 0 = none; ignored if profile set*/
    int32_t       loop_start;    /* <0 none; 0 whole track; >0 from this sample */
    int32_t       loop_end;      /* <=0 to track end; >0 inclusive last sample  */
    At9ProgressCb progress;      /* NULL = none                                */
    void         *user;
    uint32_t      size_profile;  /* AT9_SIZE_*; non-zero overrides max_bytes    */
} At9EncodeOptions;

typedef struct {
    uint32_t      struct_size;
    At9ProgressCb progress;      /* NULL = none */
    void         *user;
} At9DecodeOptions;
```

### Input, bitrate, and automatic resampling

`at9_encode_wav()` accepts 8/16/24/32-bit integer and 32/64-bit float PCM WAV and
converts to 16-bit internally (clamping cleanly on down-conversion). `at9_encode()`
takes interleaved 16-bit PCM directly. **Any input sample rate is accepted** — if
it isn't a console rate (48/24/12 kHz) the codec resamples to the nearest one with
a band-limited (windowed-sinc) filter, scaling loop points to match, so you never
have to resample yourself. `bitrate = 0` picks the best standard rate (the highest
that fits any size limit); a non-zero `bitrate` must be one of the standard AT9
rates for the stream, or the encode fails with `AT9_ERR_BITRATE`.

### Loops

`loop_start`/`loop_end` are decoded-PCM sample indices. `loop_start < 0` = no loop;
`0` = loop the whole track; `> 0` = loop from that sample. `loop_end <= 0` = loop to
the track end; `> 0` = an explicit inclusive last sample. They are written to the
AT9 `smpl` chunk and read back by `at9_parse_info()`.

### Size limits for theme BGM

The codec owns the console limits, so name the target with `size_profile` instead of
passing a byte count: `AT9_SIZE_VITA_BGM` (under ~5 MB) for PS Vita theme BGM, or
`AT9_SIZE_PS_CROSS` (2 MiB) for a theme that must also run on PS4/PS5. The encoder
picks the highest standard bitrate whose file fits, and returns `AT9_ERR_TOO_LARGE`
if the track is too long to fit even at the lowest bitrate (rather than writing an
oversized, unusable file). For a non-console budget, leave `size_profile = 0` and set
`max_bytes` to a custom cap. `at9_profile_max_bytes(profile)` returns a profile's byte
cap, so non-C callers can read the limits instead of hard-coding them.

### Progress

Set `progress` (and optional `user`) to receive `(user, done, total)` in
superframes. During encode it runs on one dedicated monitor thread — the encode
workers never call it — so it adds no measurable cost; leave it `NULL` to skip it
(no monitor thread is spawned).

### Memory ownership & thread-safety

Functions that produce a buffer allocate it inside the library and write a pointer
plus length to your out-parameters. Release each buffer with its matching
`at9_free_*` — **never** your own `free()`/`delete`, and never mix the two. All
functions are stateless and re-entrant; call them concurrently on independent
buffers.

## C / C++ by use case

```c
#include <stdio.h>
#include "at9codec.h"

/* 1. Decode an AT9 file (buffer `at9`/`at9_len` already read from disk). */
int16_t *pcm = NULL; size_t count = 0; uint16_t ch; uint32_t sr;
if (at9_decode(at9, at9_len, NULL, &pcm, &count, &ch, &sr) == AT9_OK) {
    /* ... use `count` int16 samples ... */
    at9_free_pcm(pcm, count);
}

/* 2. Encode a WAV buffer with default settings. */
uint8_t *out = NULL; size_t out_len = 0;
at9_encode_wav(wav, wav_len, NULL, &out, &out_len);
at9_free_at9(out, out_len);

/* 3. Encode Vita theme BGM: whole-track loop, capped under 5 MB, with progress. */
void on_progress(void *user, uint32_t done, uint32_t total) {
    printf("\r%u%%", total ? done * 100 / total : 0);
}
At9EncodeOptions opts = {0};
opts.struct_size  = sizeof opts;
opts.loop_start   = 0;                     /* whole-track loop            */
opts.size_profile = AT9_SIZE_VITA_BGM;     /* codec applies its own cap   */
opts.progress     = on_progress;
at9_encode_wav(wav, wav_len, &opts, &out, &out_len);
at9_free_at9(out, out_len);

/* 4. Encode with an explicit loop from sample 100000 to 200000. */
At9EncodeOptions lp = {0};
lp.struct_size = sizeof lp;
lp.loop_start  = 100000;
lp.loop_end    = 200000;   /* inclusive last sample; <=0 would mean "to end" */
at9_encode_wav(wav, wav_len, &lp, &out, &out_len);
at9_free_at9(out, out_len);
```

Build (Windows MSVC): `cl /I include app.c /link target\release\at9codec.dll.lib`
Build (Linux/macOS): `cc -I include app.c -L target/release -lat9codec -o app`

## Other languages

Full, documented bindings are in [`bindings/`](../bindings/), one folder per
language, each wrapping the options-struct API with an idiomatic `decode()` /
`encode()` (loop as a `(start, end)` pair) and a progress callback:

| Language        | Folder                                       | Mechanism        |
|-----------------|----------------------------------------------|------------------|
| C / C++         | [`bindings/c`](../bindings/c)                | header + example |
| Python          | [`bindings/python`](../bindings/python)      | `ctypes`         |
| C#              | [`bindings/csharp`](../bindings/csharp)      | P/Invoke         |
| Go              | [`bindings/go`](../bindings/go)              | cgo              |
| Swift           | [`bindings/swift`](../bindings/swift)        | C interop        |
| Kotlin / Android| [`bindings/kotlin`](../bindings/kotlin)      | JNA              |

See [`bindings/README.md`](../bindings/README.md) for how each one locates and
loads the shared library.
