#ifndef BROWNNOTE_WAV_H
#define BROWNNOTE_WAV_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/*
 * The WAV core: RIFF/WAVE parsing, sample conversion and resampling.
 * Split out of brownnote.c so it can be unit-tested (tests/unit_wav.c,
 * `make unit`) and fuzzed without the CLI around it.
 */

struct wav_info {
	unsigned channels;
	unsigned rate;
	size_t data_off, data_len;
};

/* Little-endian readers/writers for RIFF fields and the output header. */
uint32_t rd32(const unsigned char *p);
uint16_t rd16(const unsigned char *p);
void wr32(unsigned char *p, uint32_t v);
void wr16(unsigned char *p, uint16_t v);

/*
 * Parse RIFF/WAVE: PCM s16le, mono or stereo, any rate 8-192 kHz.
 * Chunks are walked properly, so LIST and friends pass by; a chunk
 * whose size runs past the end of the file is clamped.  Returns false
 * (with a message on stderr) on any malformed input.
 */
bool parse_wav(const unsigned char *p, size_t len, struct wav_info *w);

/*
 * s16 -> float: v/32768, so full scale lands a hair under 1.0.
 * float -> s16: lroundf(clamp(v)*32767).  Asymmetric on purpose —
 * the common convention — and commented here so nobody "fixes" it:
 * round-tripping is not identity, it is bounded by one LSB.
 */
float s16_to_f(int16_t v);
int16_t f_to_s16(float v);

/*
 * Catmull-Rom cubic resampling, per channel, over interleaved frames.
 * Transport plumbing to meet the bench's 48 kHz — not an effect; it
 * is the same interpolation any file converter does.  Decimation
 * (in_rate > out_rate) runs the input through an anti-aliasing
 * low-pass first, so energy above the output Nyquist does not fold
 * back into the passband.
 *
 * Returns the output frame count and sets *out_p to a fresh buffer,
 * or returns in_frames with *out_p = NULL when no resampling is
 * needed (equal rates, or no input frames) — the caller keeps the
 * input buffer.  Returns (size_t)-1 on allocation failure.
 */
size_t resample_frames(const float *in, size_t in_frames,
		       unsigned channels,
		       unsigned in_rate, unsigned out_rate,
		       float **out_p);

#endif /* BROWNNOTE_WAV_H */
