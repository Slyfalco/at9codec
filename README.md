# at9codec

A **clean-room** implementation of the **ATRAC9 ("AT9")** audio codec
in Rust — no proprietary or decompiled code. It builds a small, portable native library
(`.dll` / `.so` / `.dylib`) plus a CLI (`at9`), so applications can decode and
**encode** AT9 without the existing Windows-only command-line tool or a bundled
Wine.

Until now the only working AT9 encoder was a Windows-only command-line tool,
which forces non-Windows apps to ship it under Wine. A native, cross-platform
codec removes that dependency and makes AT9 usable from any language on any OS.

## Status

| Part | State |
|------|-------|
| Container parsing (`fmt`/`fact`/`smpl`/`data`, ATRAC9 config, loop points, true length) | ✅ done |
| Decode → PCM (LibAtrac9 basis) | ✅ done |
| Validation harness | ✅ done |
| Encode (from scratch — no open ATRAC9 encoder exists) | ✅ done |

Decode is verified **bit-for-bit identical** to the AT9 format across
every test file (max sample difference 0). Run `tools/compare.py` against a folder
of `.at9` files to reproduce.

Encoded output is accepted by the reference ATRAC9 decoder (whole-track loops and
exact length preserved), at quality matching it. Encoding supports loop points,
8/16/24/32-bit integer and 32/64-bit float WAV input (down-converted to 16-bit
with clean clamping), and an optional output size cap for console theme BGM (see
the CLI below).

## Build

```sh
cargo build --release
# library:  target/release/at9codec.{dll,so,dylib}
# CLI:      target/release/at9[.exe]
```

## CLI

```sh
at9 info   input.at9             # print container metadata
at9 decode input.at9 out.wav     # decode to 16-bit PCM WAV
at9 encode input.wav out.at9     # encode 16-bit PCM WAV to AT9 (no size cap)
at9 encode --vita-system-bgm input.wav out.at9   # cap under 5 MB for PS Vita theme BGM
at9 encode --ps-cross        input.wav out.at9   # cap at 2 MiB (PS4/PS5 too)
```

`--vita-system-bgm` keeps the file under the PS Vita's theme-BGM size limit
(~5 MB) by auto-selecting the highest standard bitrate that fits, so a
default-settings encode plays out of the box; `--ps-cross` uses the tighter 2 MiB
cap for a theme that must also run on PS4/PS5. With no flag there is no cap
(general game AT9, up to the RIFF container's 4 GiB). The same control is exposed
to embedders through the library's `max_bytes` argument (see
`docs/USING_THE_LIBRARY.md`).

## Using it from other languages

The shared library exposes a small, stable C ABI driven by options structs (set
only the fields you need, or pass NULL for defaults). Ready-to-use, documented
bindings live in [`bindings/`](bindings) — C/C++, Python (ctypes), C# (P/Invoke),
Go (cgo), Swift (C interop) and Kotlin/Android (JNA) — each wrapping `decode()` and
`encode()` with loop (start + optional end), size caps and a progress callback.
The codec resamples any input rate to a console rate itself, so callers don't have
to. See [`docs/USING_THE_LIBRARY.md`](docs/USING_THE_LIBRARY.md).

## Workspace layout

```
crates/at9codec/   core library (pure Rust, no deps) — builds rlib + cdylib
  src/container.rs  AT9 RIFF/WAVE parsing + ATRAC9 config
  src/ffi.rs        C ABI for the cdylib (options-struct API)
  src/encode/       encoder (MDCT, allocation, resampling, container writer)
crates/at9cli/      the `at9` command-line tool
include/at9codec.h  C header
bindings/           language bindings + examples (C, Python, C#, Go, Swift, Kotlin)
tools/compare.py    validation harness (compares our output to reference decoders)
reference/          clean-room reference sources, analysis only (git-ignored)
```

## Clean-room policy

Derived from the open, MIT-licensed clean-room references
[LibAtrac9](https://github.com/Thealexbarney/LibAtrac9) (decode) and
[VGAudio](https://github.com/Thealexbarney/VGAudio) (encode structure), plus
public DSP technique. A reference encoder is used **only as a black-box validation
oracle** (known inputs → inspect outputs) — never decompiled, touched or changed.

## License

MIT (see [LICENSE](LICENSE)). The codec is clean-room and license-safe to ship as
a standalone library.
