// at9codec — Kotlin binding (JNA). Works on the JVM and Android (arm64/x86_64).
//
// Dependency: net.java.dev.jna:jna (and jna-platform). On Android add the JNA AAR
// and ship libat9codec.so per ABI under jniLibs/<abi>/; on the desktop JVM put
// at9codec.(dll|so|dylib) on the java.library.path or next to the app.
//
//   val at9 = At9Codec.encodeWav(wavBytes,
//                                loopStart = 0,                       // whole-track loop
//                                sizeProfile = At9Codec.SIZE_VITA_BGM, // codec owns the limit
//                                progress = { d, t -> println("$d/$t") })
//   val (pcm, channels, rate) = At9Codec.decode(at9)

package at9codec

import com.sun.jna.*
import com.sun.jna.ptr.IntByReference
import com.sun.jna.ptr.LongByReference
import com.sun.jna.ptr.PointerByReference

/** size_t, sized correctly for the current platform. */
class SizeT(value: Long = 0) : IntegerType(Native.SIZE_T_SIZE, value, true)

/** An out-param size_t. */
class SizeTByReference(value: Long = 0) : ByReference(Native.SIZE_T_SIZE) {
    init { value.let { setValue(it) } }
    fun setValue(v: Long) = if (Native.SIZE_T_SIZE == 8) pointer.setLong(0, v) else pointer.setInt(0, v.toInt())
    fun getValue(): Long = if (Native.SIZE_T_SIZE == 8) pointer.getLong(0) else pointer.getInt(0).toLong() and 0xffffffffL
}

fun interface ProgressCb : Callback {
    fun invoke(user: Pointer?, done: Int, total: Int)
}

@Structure.FieldOrder("struct_size", "progress", "user")
open class DecodeOptions : Structure() {
    @JvmField var struct_size = 0
    @JvmField var progress: ProgressCb? = null
    @JvmField var user: Pointer? = null
}

@Structure.FieldOrder("struct_size", "bitrate", "max_bytes", "loop_start", "loop_end", "progress", "user", "size_profile")
open class EncodeOptions : Structure() {
    @JvmField var struct_size = 0
    @JvmField var bitrate = 0
    @JvmField var max_bytes = SizeT()
    @JvmField var loop_start = 0
    @JvmField var loop_end = 0
    @JvmField var progress: ProgressCb? = null
    @JvmField var user: Pointer? = null
    @JvmField var size_profile = 0
}

internal interface Lib : Library {
    fun at9_version(): String
    fun at9_decode(at9: ByteArray, len: SizeT, opts: DecodeOptions?,
                   outPcm: PointerByReference, outCount: SizeTByReference,
                   outCh: ShortByReferenceJNA, outRate: IntByReference): Int
    fun at9_free_pcm(pcm: Pointer, count: SizeT)
    fun at9_encode_wav(wav: ByteArray, len: SizeT, opts: EncodeOptions?,
                       outAt9: PointerByReference, outLen: SizeTByReference): Int
    fun at9_free_at9(at9: Pointer, len: SizeT)
    fun at9_profile_max_bytes(sizeProfile: Int): SizeT
    // Streaming (since v2.1).
    fun at9_stream_open(at9: ByteArray, len: SizeT, outStream: PointerByReference): Int
    fun at9_stream_channels(stream: Pointer): Short
    fun at9_stream_sample_rate(stream: Pointer): Int
    fun at9_stream_total_frames(stream: Pointer): Long
    fun at9_stream_position(stream: Pointer): Long
    fun at9_stream_loop(stream: Pointer, start: LongByReference, end: LongByReference): Int
    fun at9_stream_read(stream: Pointer, out: ShortArray, sampleCount: SizeT): SizeT
    fun at9_stream_seek(stream: Pointer, frame: Long)
    fun at9_stream_close(stream: Pointer)
}

// JNA has no built-in ShortByReference; provide a minimal one for out uint16.
class ShortByReferenceJNA(value: Short = 0) : ByReference(2) {
    init { pointer.setShort(0, value) }
    fun getValue(): Short = pointer.getShort(0)
}

object At9Codec {
    // Size profiles: name a console target and the codec applies its own byte cap.
    const val SIZE_NONE = 0
    const val SIZE_VITA_BGM = 1   // PS Vita system/theme BGM (under 5 MB)
    const val SIZE_PS_CROSS = 2   // tighter 2 MiB cap for PS4/PS5 cross-support
    private const val AT9_OK = 0
    private const val AT9_ERR_TOO_LARGE = -6

    internal val lib: Lib = Native.load("at9codec", Lib::class.java)

    fun version(): String = lib.at9_version()

    /** The codec's byte cap for a size profile (SIZE_*); 0 = no cap/unknown. Reads
     *  the limit from the library so you never hard-code the console numbers. */
    fun profileMaxBytes(sizeProfile: Int): Long = lib.at9_profile_max_bytes(sizeProfile).toLong()

    data class Decoded(val pcm: ByteArray, val channels: Int, val sampleRate: Int)

    /** Decode AT9 bytes to interleaved little-endian 16-bit PCM. */
    fun decode(at9: ByteArray, progress: ((Int, Int) -> Unit)? = null): Decoded {
        val opts = DecodeOptions().apply {
            struct_size = size()
            if (progress != null) { this.progress = ProgressCb { _, d, t -> progress(d, t) }; }
        }
        val outPcm = PointerByReference()
        val count = SizeTByReference()
        val ch = ShortByReferenceJNA()
        val sr = IntByReference()
        val rc = lib.at9_decode(at9, SizeT(at9.size.toLong()), opts, outPcm, count, ch, sr)
        if (rc != AT9_OK) throw RuntimeException("at9_decode failed: $rc")
        try {
            val n = count.getValue().toInt()
            val pcm = outPcm.value.getByteArray(0, n * 2)
            return Decoded(pcm, ch.getValue().toInt() and 0xffff, sr.value)
        } finally {
            lib.at9_free_pcm(outPcm.value, SizeT(count.getValue()))
        }
    }

    /**
     * Encode a WAV buffer to AT9. loopStart: <0 none, 0 whole track, >0 from sample.
     * loopEnd: <=0 to track end, >0 inclusive last sample. sizeProfile: SIZE_VITA_BGM /
     * SIZE_PS_CROSS to let the codec apply its own cap; SIZE_NONE uses maxBytes (0 = uncapped).
     */
    fun encodeWav(wav: ByteArray, bitrate: Int = 0, sizeProfile: Int = SIZE_NONE,
                  maxBytes: Long = 0, loopStart: Int = -1, loopEnd: Int = 0,
                  progress: ((Int, Int) -> Unit)? = null): ByteArray {
        val opts = EncodeOptions().apply {
            struct_size = size()
            this.bitrate = bitrate
            this.max_bytes = SizeT(maxBytes)
            this.loop_start = loopStart
            this.loop_end = loopEnd
            this.size_profile = sizeProfile
            if (progress != null) this.progress = ProgressCb { _, d, t -> progress(d, t) }
        }
        val out = PointerByReference()
        val outLen = SizeTByReference()
        val rc = lib.at9_encode_wav(wav, SizeT(wav.size.toLong()), opts, out, outLen)
        if (rc == AT9_ERR_TOO_LARGE) throw RuntimeException("track too long to fit the size limit")
        if (rc != AT9_OK) throw RuntimeException("at9_encode_wav failed: $rc")
        try {
            return out.value.getByteArray(0, outLen.getValue().toInt())
        } finally {
            lib.at9_free_at9(out.value, SizeT(outLen.getValue()))
        }
    }
}

/**
 * Pull-based AT9 decoder for playback (since v2.1): read PCM in small chunks as
 * your audio device needs it, and seek to loop. Bit-identical to decode(). Use
 * one stream from one thread at a time and close it (or use `use {}`).
 *
 *   At9Stream(at9Bytes).use { s ->
 *       val buf = ShortArray(4096)
 *       while (true) { val n = s.read(buf); if (n == 0) break; play(buf, n) }
 *       s.loop?.let { (start, _) -> s.seek(start) }
 *   }
 */
class At9Stream(at9: ByteArray) : AutoCloseable {
    private var h: Pointer?

    init {
        val out = PointerByReference()
        val rc = At9Codec.lib.at9_stream_open(at9, SizeT(at9.size.toLong()), out)
        if (rc != 0) throw RuntimeException("at9_stream_open failed: $rc")
        h = out.value
    }

    private fun handle(): Pointer = h ?: throw IllegalStateException("stream is closed")

    val channels: Int get() = At9Codec.lib.at9_stream_channels(handle()).toInt() and 0xffff
    val sampleRate: Int get() = At9Codec.lib.at9_stream_sample_rate(handle())
    /** Un-looped length in frames. */
    val totalFrames: Long get() = At9Codec.lib.at9_stream_total_frames(handle())
    val position: Long get() = At9Codec.lib.at9_stream_position(handle())

    /** Loop region in frames (end inclusive), or null if the file doesn't loop. */
    val loop: Pair<Long, Long>?
        get() {
            val a = LongByReference(); val b = LongByReference()
            return if (At9Codec.lib.at9_stream_loop(handle(), a, b) != 0) Pair(a.value, b.value) else null
        }

    /** Fill [buffer] with interleaved samples; returns how many (0 at the end). */
    fun read(buffer: ShortArray): Int =
        At9Codec.lib.at9_stream_read(handle(), buffer, SizeT(buffer.size.toLong())).toInt()

    /** Reposition to a frame (sample-accurate). */
    fun seek(frame: Long) = At9Codec.lib.at9_stream_seek(handle(), frame)

    override fun close() {
        h?.let { At9Codec.lib.at9_stream_close(it) }
        h = null
    }
}
