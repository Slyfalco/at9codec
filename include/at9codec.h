/*
 * at9codec — clean-room ATRAC9 (AT9) decoder/encoder, C API.
 *
 * Link against the shared library (at9codec.dll / libat9codec.so /
 * libat9codec.dylib). No CLI or external tool is required.
 *
 * Optional behaviour is passed through small options structs: zero-initialize one,
 * set its struct_size and only the fields you need, and pass its address — or pass
 * NULL for all defaults. New fields can be appended later without breaking callers.
 *
 * Memory ownership: functions that produce a buffer allocate it inside the
 * library and write a pointer + length to the caller's out-parameters. Release
 * each such buffer with the matching at9_free_* function — never with your own
 * free()/delete, and do not mix the two free functions.
 *
 * Thread-safety: all functions are stateless and re-entrant; you may call them
 * from multiple threads concurrently (on independent buffers).
 */
#ifndef AT9CODEC_H
#define AT9CODEC_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Status codes (returned by the int32_t functions). */
#define AT9_OK          0
#define AT9_ERR_NULL   -1  /* a required pointer argument was NULL            */
#define AT9_ERR_PARSE  -2  /* input is not a valid AT9 container              */
#define AT9_ERR_DECODE -3  /* decoding failed                                 */
#define AT9_ERR_ENCODE -4  /* encoding failed (e.g. unsupported sample rate)  */
#define AT9_ERR_BITRATE -5 /* bitrate is not a standard rate; use 0 for auto  */
#define AT9_ERR_TOO_LARGE -6 /* too long to fit the size limit at any bitrate */

/*
 * Size profiles for At9EncodeOptions.size_profile. Naming a console target makes
 * the codec apply its own byte cap for it (and ignore max_bytes), so callers never
 * hard-code the limits: the Vita won't use a theme AT9 over ~5 MB, and a theme that
 * must also run on PS4/PS5 is capped tighter at 2 MiB. 0 = use max_bytes.
 */
#define AT9_SIZE_NONE     0u
#define AT9_SIZE_VITA_BGM 1u
#define AT9_SIZE_PS_CROSS 2u

/*
 * The raw byte caps, if you need the numbers directly (e.g. to size-check an
 * existing AT9). Prefer size_profile for encoding. at9_profile_max_bytes() returns
 * the same values from the library, so non-C callers need not hard-code them.
 */
#define AT9_VITA_BGM_MAX_BYTES     ((size_t)5u * 1024u * 1024u)
#define AT9_PS_CROSS_BGM_MAX_BYTES ((size_t)2u * 1024u * 1024u)

/*
 * Progress callback: (user, units_done, units_total). One unit is one superframe.
 * Invoked from a single thread (a dedicated monitor thread during encode, the
 * decode thread during decode), never concurrently, so it need not be re-entrant.
 * Keep it cheap. Leave the options' progress field NULL for no reporting (then no
 * monitor thread is spawned — zero overhead).
 */
typedef void (*At9ProgressCb)(void *user, uint32_t done, uint32_t total);

/* Container metadata filled by at9_parse_info(). */
typedef struct {
    uint32_t sample_rate;
    uint16_t channels;
    uint16_t _pad;
    uint32_t total_samples;          /* true decoded length, in frames        */
    uint32_t encoder_delay;          /* priming samples (typically 256)       */
    uint16_t frame_bytes;
    uint16_t frames_per_superframe;
    uint32_t superframe_count;
    uint32_t has_loop;               /* 1 if a loop is defined, else 0        */
    uint32_t loop_start;             /* decoded-WAV sample coords (if has_loop)*/
    uint32_t loop_end;               /* inclusive last sample of the loop      */
    uint32_t is_whole_loop;
} At9Info;

/* Options for at9_decode(). Zero-init, set struct_size, pass NULL for defaults. */
typedef struct {
    uint32_t      struct_size;   /* = sizeof(At9DecodeOptions)                 */
    At9ProgressCb progress;      /* NULL = no progress reporting               */
    void         *user;          /* passed back to progress                    */
} At9DecodeOptions;

/* Options for at9_encode()/at9_encode_wav(). Zero-init, set struct_size. */
typedef struct {
    uint32_t      struct_size;   /* = sizeof(At9EncodeOptions)                 */
    uint32_t      bitrate;       /* bits/sec; 0 = best standard default        */
    size_t        max_bytes;     /* custom cap; 0 = none; ignored if profile set*/
    int32_t       loop_start;    /* <0 none; 0 whole track; >0 from this sample */
    int32_t       loop_end;      /* <=0 to track end; >0 inclusive last sample  */
    At9ProgressCb progress;      /* NULL = no progress reporting               */
    void         *user;
    uint32_t      size_profile;  /* AT9_SIZE_*; non-zero overrides max_bytes    */
} At9EncodeOptions;

/* Null-terminated version string, valid for the life of the library. */
const char *at9_version(void);

/*
 * Byte cap the codec uses for a size profile (AT9_SIZE_*); 0 = no cap/unknown.
 * Lets non-C callers read the console limits instead of hard-coding them.
 */
size_t at9_profile_max_bytes(uint32_t size_profile);

/* Parse an AT9 file's header into *out. Returns AT9_OK or a negative code. */
int32_t at9_parse_info(const uint8_t *data, size_t len, At9Info *out);

/*
 * Decode an AT9 file to interleaved 16-bit PCM.
 * opts may be NULL (defaults). On AT9_OK: *out_pcm points to *out_count int16_t
 * samples (frames*channels), and *out_channels / *out_sample_rate hold the format.
 * Free with at9_free_pcm.
 */
int32_t at9_decode(const uint8_t *at9, size_t at9_len,
                   const At9DecodeOptions *opts,
                   int16_t **out_pcm, size_t *out_count,
                   uint16_t *out_channels, uint32_t *out_sample_rate);

/* Free a buffer returned by at9_decode(). count = *out_count. */
void at9_free_pcm(int16_t *pcm, size_t count);

/*
 * Encode interleaved 16-bit PCM to an AT9 file (RIFF/WAVE).
 * sample_rate must be 12000, 24000 or 48000. opts may be NULL (defaults).
 * On AT9_OK: *out_at9 points to *out_len bytes; free with at9_free_at9.
 */
int32_t at9_encode(const int16_t *pcm, size_t sample_count,
                   uint16_t channels, uint32_t sample_rate,
                   const At9EncodeOptions *opts,
                   uint8_t **out_at9, size_t *out_len);

/*
 * Encode a WAV file buffer to an AT9 file. Accepts 8/16/24/32-bit integer and
 * 32/64-bit float PCM WAV (converted to 16-bit internally, no resampling).
 * opts may be NULL. Free the result with at9_free_at9.
 */
int32_t at9_encode_wav(const uint8_t *wav, size_t wav_len,
                       const At9EncodeOptions *opts,
                       uint8_t **out_at9, size_t *out_len);

/* Free a buffer returned by at9_encode()/at9_encode_wav(). len = *out_len. */
void at9_free_at9(uint8_t *at9, size_t len);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AT9CODEC_H */
