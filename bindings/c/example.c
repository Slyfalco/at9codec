/*
 * at9codec — C / C++ example.
 *
 * Builds against the public header and links the shared library:
 *   Windows (MSVC): cl /I ../../include example.c /link ../../target/release/at9codec.dll.lib
 *   Linux/macOS:    cc -I ../../include example.c -L ../../target/release -lat9codec -o example
 *
 * Demonstrates: encode a WAV to Vita theme BGM (whole-track loop, 5 MB cap, with
 * progress), then decode it back. The same code compiles as C or C++ (the header
 * is `extern "C"`).
 */
#include <stdio.h>
#include <stdlib.h>
#include "at9codec.h"

static void on_progress(void *user, uint32_t done, uint32_t total) {
    (void)user;
    if (total) printf("\rencoding %u%%", done * 100u / total);
    fflush(stdout);
}

static unsigned char *read_file(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) return NULL;
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    unsigned char *buf = (unsigned char *)malloc((size_t)n);
    *len = fread(buf, 1, (size_t)n, f);
    fclose(f);
    return buf;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: %s in.wav out.at9\n", argv[0]);
        return 2;
    }
    printf("at9codec %s\n", at9_version());

    size_t wav_len = 0;
    unsigned char *wav = read_file(argv[1], &wav_len);
    if (!wav) { fprintf(stderr, "cannot read %s\n", argv[1]); return 1; }

    /* Encode: whole-track loop, Vita size cap (named so the codec owns the limit),
     * progress. */
    At9EncodeOptions opts = {0};
    opts.struct_size = sizeof opts;
    opts.loop_start  = 0;                       /* 0 = whole-track loop */
    opts.size_profile = AT9_SIZE_VITA_BGM;      /* codec applies its own cap */
    opts.progress    = on_progress;

    uint8_t *at9 = NULL; size_t at9_len = 0;
    int rc = at9_encode_wav(wav, wav_len, &opts, &at9, &at9_len);
    free(wav);
    if (rc == AT9_ERR_TOO_LARGE) {
        fprintf(stderr, "\ntrack too long to fit the size limit\n"); return 1;
    }
    if (rc != AT9_OK) { fprintf(stderr, "\nencode failed: %d\n", rc); return 1; }
    printf("\nencoded %zu bytes\n", at9_len);

    FILE *o = fopen(argv[2], "wb");
    if (o) { fwrite(at9, 1, at9_len, o); fclose(o); }

    /* Decode it back (pass NULL options for defaults). */
    int16_t *pcm = NULL; size_t count = 0; uint16_t ch = 0; uint32_t sr = 0;
    rc = at9_decode(at9, at9_len, NULL, &pcm, &count, &ch, &sr);
    at9_free_at9(at9, at9_len);
    if (rc != AT9_OK) { fprintf(stderr, "decode failed: %d\n", rc); return 1; }
    printf("decoded %zu samples, %u Hz, %u ch\n", count, sr, ch);
    at9_free_pcm(pcm, count);
    return 0;
}
