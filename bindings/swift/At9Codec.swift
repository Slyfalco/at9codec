// at9codec — Swift binding (C interop).
//
// Swift imports the C header directly. Expose it as a module named `CAt9Codec`
// with a module map (see ../README.md), then `import CAt9Codec`. Link the shared
// library (libat9codec.dylib on macOS / .so on Linux; or the static/import lib).
//
//   let at9 = try At9Codec.encodeWav(wavData,
//                                    loopStart: 0,                       // whole-track loop
//                                    maxBytes: At9Codec.vitaBgmMaxBytes,
//                                    progress: { d, t in print("\(d)/\(t)") })
//   let (pcm, channels, rate) = try At9Codec.decode(at9)

import Foundation
import CAt9Codec

public enum At9CodecError: Error { case failed(Int32) }

public enum At9Codec {
    public static let vitaBgmMaxBytes = 5 * 1024 * 1024
    public static let psCrossBgmMaxBytes = 2 * 1024 * 1024

    public static func version() -> String { String(cString: at9_version()) }

    // Holds a Swift progress closure so it can be reached from the C callback
    // through the options' `user` pointer.
    private final class ProgressBox {
        let cb: (UInt32, UInt32) -> Void
        init(_ cb: @escaping (UInt32, UInt32) -> Void) { self.cb = cb }
    }

    private static let trampoline: At9ProgressCb = { user, done, total in
        guard let user = user else { return }
        Unmanaged<ProgressBox>.fromOpaque(user).takeUnretainedValue().cb(done, total)
    }

    /// Decode AT9 data to interleaved little-endian 16-bit PCM.
    public static func decode(_ at9: Data,
                              progress: ((UInt32, UInt32) -> Void)? = nil)
        throws -> (pcm: Data, channels: Int, sampleRate: Int)
    {
        let box = progress.map(ProgressBox.init)
        var opts = At9DecodeOptions()
        opts.struct_size = UInt32(MemoryLayout<At9DecodeOptions>.size)
        if let box = box {
            opts.progress = trampoline
            opts.user = Unmanaged.passUnretained(box).toOpaque()
        }
        var outPcm: UnsafeMutablePointer<Int16>? = nil
        var count: Int = 0
        var ch: UInt16 = 0
        var sr: UInt32 = 0
        let rc = at9.withUnsafeBytes { raw -> Int32 in
            at9_decode(raw.bindMemory(to: UInt8.self).baseAddress, at9.count, &opts,
                       &outPcm, &count, &ch, &sr)
        }
        withExtendedLifetime(box) {}
        guard rc == AT9_OK, let outPcm = outPcm else { throw At9CodecError.failed(rc) }
        defer { at9_free_pcm(outPcm, count) }
        let pcm = Data(bytes: outPcm, count: count * 2)
        return (pcm, Int(ch), Int(sr))
    }

    /// Encode a WAV buffer to AT9. loopStart: <0 none, 0 whole track, >0 from sample.
    /// loopEnd: <=0 to track end, >0 inclusive last sample. maxBytes: 0 uncapped.
    public static func encodeWav(_ wav: Data,
                                 bitrate: UInt32 = 0,
                                 maxBytes: Int = 0,
                                 loopStart: Int32 = -1,
                                 loopEnd: Int32 = 0,
                                 progress: ((UInt32, UInt32) -> Void)? = nil) throws -> Data
    {
        let box = progress.map(ProgressBox.init)
        var opts = At9EncodeOptions()
        opts.struct_size = UInt32(MemoryLayout<At9EncodeOptions>.size)
        opts.bitrate = bitrate
        opts.max_bytes = maxBytes
        opts.loop_start = loopStart
        opts.loop_end = loopEnd
        if let box = box {
            opts.progress = trampoline
            opts.user = Unmanaged.passUnretained(box).toOpaque()
        }
        var out: UnsafeMutablePointer<UInt8>? = nil
        var outLen: Int = 0
        let rc = wav.withUnsafeBytes { raw -> Int32 in
            at9_encode_wav(raw.bindMemory(to: UInt8.self).baseAddress, wav.count, &opts,
                           &out, &outLen)
        }
        withExtendedLifetime(box) {}
        guard rc == AT9_OK, let out = out else { throw At9CodecError.failed(rc) }
        defer { at9_free_at9(out, outLen) }
        return Data(bytes: out, count: outLen)
    }
}
