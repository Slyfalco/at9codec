# at9codec language bindings

Ready-to-use wrappers around the at9codec C ABI, one folder per language. Each
exposes the same two operations over the options-struct API:

- **decode** AT9 → interleaved little-endian 16-bit PCM (+ channels, sample rate)
- **encode** a WAV buffer → AT9, with optional **bitrate**, **size profile**
  (`SIZE_VITA_BGM` / `SIZE_PS_CROSS` — the codec owns the byte limits), **loop**
  (start + optional end), and a **progress callback**.
- **stream** (since v2.1) — a stream object for playback: read PCM in small
  chunks, read the loop region, and seek (sample-accurate) to loop.

The codec accepts any input sample rate (it resamples to a console rate itself)
and any WAV bit depth, so callers don't resample or convert. See
[`../docs/USING_THE_LIBRARY.md`](../docs/USING_THE_LIBRARY.md) for the API concepts
and [`../include/at9codec.h`](../include/at9codec.h) for the C declarations.

First build (or download from the releases) the shared library:

```sh
cargo build --release   # -> target/release/at9codec.(dll|so|dylib) [+ at9codec.dll.lib]
```

| Language | Folder | How it finds/loads the library |
|----------|--------|--------------------------------|
| C / C++  | [`c/`](c) | Compile against `include/at9codec.h`, link the import/shared lib (see the comment header in `c/example.c`). |
| Python   | [`python/`](python) | `ctypes` loads `at9codec.*` from beside `at9codec.py`, the `AT9CODEC_LIB` env var, or the system path. Pure stdlib, no wheel. |
| C#       | [`csharp/`](csharp) | `DllImport("at9codec")`; put the library next to the executable or on the library path. Works on .NET (Framework/Core/5+). |
| Go       | [`go/`](go) | cgo; the `#cgo` lines point at `../../include` and `../../target/release`. The shared lib must be on the loader path at run time. |
| Swift    | [`swift/`](swift) | Imports a C module `CAt9Codec` (module map below), links `-lat9codec`. Good for iOS/macOS. |
| Kotlin   | [`kotlin/`](kotlin) | JNA (`Native.load("at9codec", ...)`). JVM + Android (ship `libat9codec.so` per ABI in `jniLibs/`). |

## Per-language notes

### C / C++
`c/example.c` is a complete program (encode a WAV to Vita BGM, decode it back,
then stream it in chunks as a player would).
Build lines are in its header comment. The header is `extern "C"`, so it compiles
as C or C++.

### Python
Copy `python/at9codec.py` into your project. Point it at the library with the
`AT9CODEC_LIB` environment variable (a file or directory) if it isn't next to the
module or on the system path.

### C#
Add `csharp/At9Codec.cs` to your project. On modern .NET, `DllImport("at9codec")`
maps to `at9codec.dll` / `libat9codec.so` / `libat9codec.dylib` automatically.

### Go
`go/at9codec.go` uses cgo. Either keep the relative `#cgo` paths (build from a
checkout that has `include/` and `target/release/`), or install the header and
library system-wide and simplify the directives. At run time the shared library
must be locatable (`LD_LIBRARY_PATH`/`DYLD_LIBRARY_PATH`, an rpath, or next to the
binary on Windows).

### Swift
Expose the C header as a module with a `module.modulemap`, e.g.:

```
module CAt9Codec {
    header "at9codec.h"
    link "at9codec"
    export *
}
```

Put it on the import path (a SwiftPM system-library target, or `-I` the folder) and
`import CAt9Codec`. `swift/At9Codec.swift` wraps it in a Swift-friendly API.

### Kotlin / Android
`kotlin/At9Codec.kt` uses JNA (`net.java.dev.jna:jna`). On Android add the JNA AAR
and place `libat9codec.so` under `src/main/jniLibs/<abi>/` (e.g. `arm64-v8a`); on
the desktop JVM put the library on `java.library.path`. The `SizeT` helper sizes
`size_t` per platform (64-bit is assumed for the out-params, which is the norm on
mobile and desktop).
