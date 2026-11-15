# at9codec

A **clean-room** implementation of the **ATRAC9 ("AT9")** audio codec
in Rust — no proprietary or decompiled code. It builds a small, portable native library
(`.dll` / `.so` / `.dylib`) plus a CLI (`at9`), so applications can decode and
(eventually) **encode** AT9 without the Windows-only the reference tool or a bundled
Wine.

Today the only working AT9 encoder is the Windows-only the reference tool, which
forces non-Windows apps to ship it under Wine. A native, cross-platform codec
removes that dependency and makes AT9 usable from any language on any OS.

## Status

| Part | State |
|------|-------|
| Container parsing (`fmt`/`fact`/`smpl`/`data`, ATRAC9 config, loop points, true length) | ✅ done |
| Decode → PCM (LibAtrac9 basis) | ✅ done — **bit-exact** vs the reference tool/VGAudio |
| Validation harness (round-trip vs the reference tool/VGAudio) | ✅ done (`tools/compare.py`) |
| Encode (from scratch — no open ATRAC9 encoder exists) | ⬜ not started |

Decode is verified **bit-for-bit identical** to the reference the reference tool and VGAudio across
every test file (max sample difference 0). Run `tools/compare.py` against a folder
of `.at9` files with the reference tool/`the reference encoder.exe` available to reproduce.

## Build

```sh
cargo build --release
# library:  target/release/at9codec.{dll,so,dylib}
# CLI:      target/release/at9[.exe]
```

## CLI

```sh
at9 info   input.at9             # print container metadata
at9 decode input.at9 out.wav     # decode to 16-bit PCM WAV (once implemented)
```

## Workspace layout

```
crates/at9codec/   core library (pure Rust, no deps) — builds rlib + cdylib
  src/container.rs  AT9 RIFF/WAVE parsing + ATRAC9 config
  src/wav.rs        16-bit PCM WAV writer
  src/ffi.rs        C ABI for the cdylib
crates/at9cli/      the `at9` command-line tool
tools/compare.py    validation harness (compares our output to reference decoders)
reference/          clean-room reference sources, analysis only (git-ignored)
```

## Clean-room policy

Derived from the open, MIT-licensed clean-room references
[LibAtrac9](https://github.com/Thealexbarney/LibAtrac9) (decode) and
[VGAudio](https://github.com/Thealexbarney/VGAudio) (encode structure), plus
public DSP technique. The reference the reference tool is used **only as a black-box validation
oracle** (known inputs → inspect outputs) — never decompiled into shipped code.

## License

MIT (see [LICENSE](LICENSE)). The codec is clean-room and license-safe to ship as
a standalone library.
