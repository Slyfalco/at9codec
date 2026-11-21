// at9codec — C# binding (P/Invoke).
//
// Put at9codec.dll / libat9codec.so / libat9codec.dylib where the .NET runtime
// can find it (next to the executable, or on the library path). DllImport("at9codec")
// resolves the platform file name automatically on modern .NET.
//
//   byte[] at9 = At9Codec.EncodeWav(File.ReadAllBytes("in.wav"),
//                                   loopStart: 0,                       // whole-track loop
//                                   sizeProfile: At9Codec.SizeVitaBgm,  // codec owns the limit
//                                   progress: (d, t) => Console.Write($"\r{d}/{t}"));
//   File.WriteAllBytes("out.at9", at9);
//   var (pcm, channels, rate) = At9Codec.Decode(at9);

using System;
using System.Runtime.InteropServices;

public static class At9Codec
{
    public const int AT9_OK = 0;
    public const int AT9_ERR_BITRATE = -5;    // bitrate is not a standard rate
    public const int AT9_ERR_TOO_LARGE = -6;  // too long to fit the size limit

    // Size profiles: name a console target and the codec applies its own byte cap.
    public const uint SizeNone = 0;
    public const uint SizeVitaBgm = 1;   // PS Vita system/theme BGM (under 5 MB)
    public const uint SizePsCross = 2;   // tighter 2 MiB cap for PS4/PS5

    public delegate void ProgressCb(IntPtr user, uint done, uint total);

    [StructLayout(LayoutKind.Sequential)]
    private struct DecodeOptions
    {
        public uint StructSize;
        public IntPtr Progress; // function pointer (or IntPtr.Zero)
        public IntPtr User;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct EncodeOptions
    {
        public uint StructSize;
        public uint Bitrate;
        public nuint MaxBytes;
        public int LoopStart;
        public int LoopEnd;
        public IntPtr Progress;
        public IntPtr User;
        public uint SizeProfile;
    }

    [DllImport("at9codec", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr at9_version();

    [DllImport("at9codec", CallingConvention = CallingConvention.Cdecl)]
    private static extern int at9_decode(byte[] at9, nuint len, in DecodeOptions opts,
        out IntPtr outPcm, out nuint outCount, out ushort outChannels, out uint outRate);

    [DllImport("at9codec", CallingConvention = CallingConvention.Cdecl)]
    private static extern void at9_free_pcm(IntPtr pcm, nuint count);

    [DllImport("at9codec", CallingConvention = CallingConvention.Cdecl)]
    private static extern int at9_encode_wav(byte[] wav, nuint len, in EncodeOptions opts,
        out IntPtr outAt9, out nuint outLen);

    [DllImport("at9codec", CallingConvention = CallingConvention.Cdecl)]
    private static extern void at9_free_at9(IntPtr at9, nuint len);

    [DllImport("at9codec", CallingConvention = CallingConvention.Cdecl)]
    private static extern nuint at9_profile_max_bytes(uint sizeProfile);

    public static string Version() => Marshal.PtrToStringAnsi(at9_version()) ?? "";

    /// <summary>The codec's byte cap for a size profile (Size*); 0 = no cap/unknown.</summary>
    public static ulong ProfileMaxBytes(uint sizeProfile) => at9_profile_max_bytes(sizeProfile);

    /// <summary>Decode AT9 bytes to interleaved little-endian 16-bit PCM.</summary>
    public static (byte[] pcm, int channels, int sampleRate) Decode(byte[] at9, ProgressCb? progress = null)
    {
        var cb = progress; // keep alive across the call
        var opts = new DecodeOptions
        {
            StructSize = (uint)Marshal.SizeOf<DecodeOptions>(),
            Progress = cb == null ? IntPtr.Zero : Marshal.GetFunctionPointerForDelegate(cb),
            User = IntPtr.Zero,
        };
        int rc = at9_decode(at9, (nuint)at9.Length, in opts,
            out IntPtr outPcm, out nuint count, out ushort ch, out uint sr);
        if (rc != AT9_OK) throw new Exception($"at9_decode failed: {rc}");
        try
        {
            var pcm = new byte[(int)count * 2];
            Marshal.Copy(outPcm, pcm, 0, pcm.Length);
            GC.KeepAlive(cb);
            return (pcm, ch, (int)sr);
        }
        finally { at9_free_pcm(outPcm, count); }
    }

    /// <summary>
    /// Encode a WAV buffer to AT9. loopStart: &lt;0 none, 0 whole track, &gt;0 from sample.
    /// loopEnd: &lt;=0 to track end, &gt;0 inclusive last sample. sizeProfile: SizeVitaBgm /
    /// SizePsCross to let the codec apply its own cap; SizeNone uses maxBytes (0 = uncapped).
    /// </summary>
    public static byte[] EncodeWav(byte[] wav, uint bitrate = 0, uint sizeProfile = SizeNone,
        nuint maxBytes = 0, int loopStart = -1, int loopEnd = 0, ProgressCb? progress = null)
    {
        var cb = progress;
        var opts = new EncodeOptions
        {
            StructSize = (uint)Marshal.SizeOf<EncodeOptions>(),
            Bitrate = bitrate,
            MaxBytes = maxBytes,
            LoopStart = loopStart,
            LoopEnd = loopEnd,
            Progress = cb == null ? IntPtr.Zero : Marshal.GetFunctionPointerForDelegate(cb),
            User = IntPtr.Zero,
            SizeProfile = sizeProfile,
        };
        int rc = at9_encode_wav(wav, (nuint)wav.Length, in opts, out IntPtr outAt9, out nuint outLen);
        if (rc == AT9_ERR_TOO_LARGE) throw new Exception("at9_encode_wav: track too long to fit the size limit");
        if (rc != AT9_OK) throw new Exception($"at9_encode_wav failed: {rc}");
        try
        {
            var outb = new byte[(int)outLen];
            Marshal.Copy(outAt9, outb, 0, outb.Length);
            GC.KeepAlive(cb);
            return outb;
        }
        finally { at9_free_at9(outAt9, outLen); }
    }
}
