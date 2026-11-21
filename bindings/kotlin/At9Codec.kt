// at9codec — Kotlin binding (JNA). Works on the JVM and Android (arm64/x86_64).
//
// Dependency: net.java.dev.jna:jna (and jna-platform). On Android add the JNA AAR
// and ship libat9codec.so per ABI under jniLibs/<abi>/; on the desktop JVM put
// at9codec.(dll|so|dylib) on the java.library.path or next to the app.
//
//   val at9 = At9Codec.encodeWav(wavBytes,
//                                loopStart = 0,                       // whole-track loop
//                                maxBytes = At9Codec.VITA_BGM_MAX_BYTES,
//                                progress = { d, t -> println("$d/$t") })
//   val (pcm, channels, rate) = At9Codec.decode(at9)

package at9codec

import com.sun.jna.*
import com.sun.jna.ptr.IntByReference
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

@Structure.FieldOrder("struct_size", "bitrate", "max_bytes", "loop_start", "loop_end", "progress", "user")
open class EncodeOptions : Structure() {
    @JvmField var struct_size = 0
    @JvmField var bitrate = 0
    @JvmField var max_bytes = SizeT()
    @JvmField var loop_start = 0
    @JvmField var loop_end = 0
    @JvmField var progress: ProgressCb? = null
    @JvmField var user: Pointer? = null
}

private interface Lib : Library {
    fun at9_version(): String
    fun at9_decode(at9: ByteArray, len: SizeT, opts: DecodeOptions?,
                   outPcm: PointerByReference, outCount: SizeTByReference,
                   outCh: ShortByReferenceJNA, outRate: IntByReference): Int
    fun at9_free_pcm(pcm: Pointer, count: SizeT)
    fun at9_encode_wav(wav: ByteArray, len: SizeT, opts: EncodeOptions?,
                       outAt9: PointerByReference, outLen: SizeTByReference): Int
    fun at9_free_at9(at9: Pointer, len: SizeT)
}

// JNA has no built-in ShortByReference; provide a minimal one for out uint16.
class ShortByReferenceJNA(value: Short = 0) : ByReference(2) {
    init { pointer.setShort(0, value) }
    fun getValue(): Short = pointer.getShort(0)
}

object At9Codec {
    const val VITA_BGM_MAX_BYTES = 5L * 1024 * 1024
    const val PS_CROSS_BGM_MAX_BYTES = 2L * 1024 * 1024
    private const val AT9_OK = 0

    private val lib: Lib = Native.load("at9codec", Lib::class.java)

    fun version(): String = lib.at9_version()

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
     * loopEnd: <=0 to track end, >0 inclusive last sample. maxBytes: 0 uncapped.
     */
    fun encodeWav(wav: ByteArray, bitrate: Int = 0, maxBytes: Long = 0,
                  loopStart: Int = -1, loopEnd: Int = 0,
                  progress: ((Int, Int) -> Unit)? = null): ByteArray {
        val opts = EncodeOptions().apply {
            struct_size = size()
            this.bitrate = bitrate
            this.max_bytes = SizeT(maxBytes)
            this.loop_start = loopStart
            this.loop_end = loopEnd
            if (progress != null) this.progress = ProgressCb { _, d, t -> progress(d, t) }
        }
        val out = PointerByReference()
        val outLen = SizeTByReference()
        val rc = lib.at9_encode_wav(wav, SizeT(wav.size.toLong()), opts, out, outLen)
        if (rc != AT9_OK) throw RuntimeException("at9_encode_wav failed: $rc")
        try {
            return out.value.getByteArray(0, outLen.getValue().toInt())
        } finally {
            lib.at9_free_at9(out.value, SizeT(outLen.getValue()))
        }
    }
}
