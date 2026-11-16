/*
 * at9codec — clean-room ATRAC9 (AT9) decoder/encoder, C API.
 *
 * Link against the shared library (at9codec.dll / libat9codec.so /
 * libat9codec.dylib). No CLI or external tool is required.
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
    uint32_t loop_end;
    uint32_t is_whole_loop;
} At9Info;

/*
 * Progress callback: (user, units_done, units_total). One unit is one superframe.
 * It is invoked from a single thread (a dedicated monitor thread during encode,
 * the decode thread during decode), never concurrently, so it need not be
 * re-entrant. Keep it cheap. Pass NULL to the _ex functions for no reporting
 * (then no monitor thread is spawned — zero overhead).
 */
typedef void (*At9ProgressCb)(void *user, uint32_t done, uint32_t total);

/* Null-terminated version string, valid for the life of the library. */
const char *at9_version(void);

/* Parse an AT9 file's header into *out. Returns AT9_OK or a negative code. */
int32_t at9_parse_info(const uint8_t *data, size_t len, At9Info *out);

/*
 * Decode an AT9 file to interleaved 16-bit PCM.
 * On AT9_OK: *out_pcm points to *out_count int16_t samples (frames*channels),
 * and *out_channels / *out_sample_rate hold the format. Free with at9_free_pcm.
 */
int32_t at9_decode(const uint8_t *at9, size_t at9_len,
                   int16_t **out_pcm, size_t *out_count,
                   uint16_t *out_channels, uint32_t *out_sample_rate);

/*
 * Like at9_decode(), but reports progress via cb/user (one call per superframe).
 * Pass cb = NULL for no reporting (identical to at9_decode()).
 */
int32_t at9_decode_ex(const uint8_t *at9, size_t at9_len,
                      At9ProgressCb cb, void *user,
                      int16_t **out_pcm, size_t *out_count,
                      uint16_t *out_channels, uint32_t *out_sample_rate);

/* Free a buffer returned by at9_decode()/at9_decode_ex(). count = *out_count. */
void at9_free_pcm(int16_t *pcm, size_t count);

/*
 * Encode interleaved 16-bit PCM to an AT9 file (RIFF/WAVE).
 * sample_rate must be 12000, 24000 or 48000. bitrate is in bits/sec; pass 0 for
 * a sensible default. On AT9_OK: *out_at9 points to *out_len bytes; free with
 * at9_free_at9.
 */
int32_t at9_encode(const int16_t *pcm, size_t sample_count,
                   uint16_t channels, uint32_t sample_rate, uint32_t bitrate,
                   uint8_t **out_at9, size_t *out_len);

/*
 * Encode a WAV file buffer to an AT9 file. Accepts 8/16/24/32-bit integer and
 * 32/64-bit float PCM WAV (converted to 16-bit internally, no resampling).
 * Free the result with at9_free_at9.
 */
int32_t at9_encode_wav(const uint8_t *wav, size_t wav_len, uint32_t bitrate,
                       uint8_t **out_at9, size_t *out_len);

/*
 * Like at9_encode()/at9_encode_wav(), with loop points and progress reporting.
 * loop_start: negative = no loop; 0 = whole-track loop; positive = loop from that
 * sample index to the end (decoded-PCM coordinates). cb/user as above (NULL = none).
 */
int32_t at9_encode_ex(const int16_t *pcm, size_t sample_count,
                      uint16_t channels, uint32_t sample_rate, uint32_t bitrate,
                      int32_t loop_start, At9ProgressCb cb, void *user,
                      uint8_t **out_at9, size_t *out_len);

int32_t at9_encode_wav_ex(const uint8_t *wav, size_t wav_len, uint32_t bitrate,
                          int32_t loop_start, At9ProgressCb cb, void *user,
                          uint8_t **out_at9, size_t *out_len);

/* Free a buffer returned by any at9_encode* function. len = *out_len. */
void at9_free_at9(uint8_t *at9, size_t len);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* AT9CODEC_H */
