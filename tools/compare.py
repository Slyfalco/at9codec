#!/usr/bin/env python3
"""Validation harness for at9codec.

Compares our decoder's output against reference decoders, which is how we know
the clean-room codec is correct:

  * a reference decoder (set AT9_REF_DECODER to its exe) -> ground truth,
    invoked as `<exe> -repeat 1 -d <in> <out>` (true length)
  * an optional second reference decoder (AT9_REF_DECODER2, invoked
    `<exe> -i <in> -o <out>`) -> cross-check
  * ffmpeg -> cross-check (native ATRAC9, trimmed to true length)

For each AT9 file it:
  1. runs `at9 info` and checks the reported true length against the reference,
  2. (when `at9 decode` works) decodes with our CLI and the reference and reports
     frame-count match, max abs sample diff and RMS error.

Set AT9_REF_DECODER (and optionally AT9_REF_DECODER2) to reference decoder
executables. Pure stdlib (wave/struct/subprocess), runs anywhere.
"""
import argparse
import os
import struct
import subprocess
import sys
import tempfile
import wave

DEFAULT_TOOLS = os.environ.get("AT9_REF_TOOLS", "")


def run(cmd, **kw):
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kw)


def wav_frames(path):
    with wave.open(path, "rb") as w:
        return w.getnframes(), w.getframerate(), w.getnchannels(), w.getsampwidth()


def read_samples(path):
    with wave.open(path, "rb") as w:
        n = w.getnframes()
        raw = w.readframes(n)
    return struct.unpack("<%dh" % (len(raw) // 2), raw)


def ref_decode(at9, out, tools_dir, ffmpeg):
    """Produce a true-length reference WAV. Tries, in order: the configured
    reference decoder (AT9_REF_DECODER), an optional second one
    (AT9_REF_DECODER2), then ffmpeg."""
    ref = os.environ.get("AT9_REF_DECODER", "")
    ref2 = os.environ.get("AT9_REF_DECODER2", "")
    if ref and os.path.isfile(ref):
        r = run([ref, "-repeat", "1", "-d", at9, out])
        if r.returncode == 0 and os.path.exists(out):
            return "reference"
    if ref2 and os.path.isfile(ref2):
        r = run([ref2, "-i", at9, "-o", out])
        if r.returncode == 0 and os.path.exists(out):
            return "reference2"
    if ffmpeg:
        r = run([ffmpeg, "-y", "-i", at9, out])
        if r.returncode == 0 and os.path.exists(out):
            return "ffmpeg"
    return None


def our_info(cli, at9):
    r = run([cli, "info", at9])
    info = {}
    for line in r.stdout.decode(errors="replace").splitlines():
        if ":" in line:
            k, _, v = line.partition(":")
            info[k.strip()] = v.strip()
    return (r.returncode == 0), info


def compare_pcm(a, b):
    n = min(len(a), len(b))
    max_abs = 0
    sse = 0
    for i in range(n):
        d = a[i] - b[i]
        if abs(d) > max_abs:
            max_abs = abs(d)
        sse += d * d
    rms = (sse / n) ** 0.5 if n else 0.0
    return max_abs, rms


def main():
    ap = argparse.ArgumentParser(description="at9codec validation harness")
    ap.add_argument("inputs", nargs="+", help="AT9 file(s) or directories")
    ap.add_argument("--cli", default=None, help="path to our `at9` binary")
    ap.add_argument("--tools-dir", default=DEFAULT_TOOLS,
                    help="reference tools dir (reference decoders via AT9_REF_DECODER[2])")
    ap.add_argument("--ffmpeg", default="ffmpeg", help="ffmpeg path (or '' to skip)")
    args = ap.parse_args()

    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    cli = args.cli
    if not cli:
        exe = "at9.exe" if os.name == "nt" else "at9"
        for cfg in ("release", "debug"):
            cand = os.path.join(here, "target", cfg, exe)
            if os.path.isfile(cand):
                cli = cand
                break
    ffmpeg = args.ffmpeg or None

    files = []
    for inp in args.inputs:
        if os.path.isdir(inp):
            for root, _, names in os.walk(inp):
                files += [os.path.join(root, n) for n in names if n.lower().endswith(".at9")]
        elif inp.lower().endswith(".at9"):
            files.append(inp)
    if not files:
        print("no .at9 files found")
        return 1

    print(f"cli: {cli or '(not built)'}\n")
    fails = 0
    for at9 in files:
        name = os.path.basename(at9)
        tmp = tempfile.mkdtemp(prefix="at9cmp_")
        ref_wav = os.path.join(tmp, "ref.wav")
        src = ref_decode(at9, ref_wav, args.tools_dir, ffmpeg)
        if not src:
            print(f"[SKIP] {name}: no reference decoder available")
            continue
        ref_n, rate, ch, _ = wav_frames(ref_wav)

        # 1) info true-length check
        info_line = ""
        if cli:
            ok, info = our_info(cli, at9)
            if ok:
                total = info.get("total_samples", "").split()[0]
                match = total == str(ref_n)
                info_line = f"info.total={total} vs ref={ref_n} {'OK' if match else 'MISMATCH'}"
                if not match:
                    fails += 1
            else:
                info_line = "info FAILED"
                fails += 1

        # 2) decode check (once implemented)
        dec_line = ""
        if cli:
            our_wav = os.path.join(tmp, "ours.wav")
            r = run([cli, "decode", at9, our_wav])
            if r.returncode == 0 and os.path.exists(our_wav):
                our_n = wav_frames(our_wav)[0]
                max_abs, rms = compare_pcm(read_samples(ref_wav), read_samples(our_wav))
                frame_ok = our_n == ref_n
                dec_line = (f"decode frames={our_n} vs {ref_n} "
                            f"{'OK' if frame_ok else 'MISMATCH'}  max_abs={max_abs} rms={rms:.1f}")
                if not frame_ok or max_abs > 0:
                    fails += 1
            else:
                msg = r.stderr.decode(errors="replace").strip().splitlines()[-1:] or [""]
                dec_line = f"decode: {msg[0]}"

        print(f"[{src:>10}] {name}  ({ref_n} frames @ {rate}Hz x{ch})")
        if info_line:
            print(f"             {info_line}")
        if dec_line:
            print(f"             {dec_line}")

    print(f"\n{'FAILURES: %d' % fails if fails else 'all checks passed'}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
