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

/*
 * Little-endian readers/writers for RIFF fields and the output
 * header.  The wav_ prefix keeps these generic names from
 * clashing with anything else linked in (fuzz harnesses).
 */
uint32_t wav_rd32(const unsigned char *p);
uint16_t wav_rd16(const unsigned char *p);
void wav_wr32(unsigned char *p, uint32_t v);
void wav_wr16(unsigned char *p, uint16_t v);

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
 * low-pass first, so energy above the output Nyquist is strongly
 * attenuated rather than folded back into the passband.
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

/* ------------------------------------------------------------------ */
/* Streaming resampler.                                                */
/* ------------------------------------------------------------------ */

/*
 * The same resampler, streamed: input frames are pushed in blocks of
 * any size and output frames arrive through the emit callback.
 * Produces exactly resample_frames' output — same count, same
 * values — so a stream and a batch of the same signal are
 * interchangeable.  Decimation carries half the kernel's tap count
 * of lookahead; the cubic carries two frames; equal rates pass
 * straight through.
 */
struct wav_resample;

typedef void (*wav_emit_fn)(void *ctx, const float *frames, size_t n);

/* Returns NULL on allocation failure. */
struct wav_resample *wav_resample_new(unsigned channels,
				      unsigned in_rate, unsigned out_rate,
				      wav_emit_fn emit, void *emit_ctx);

/* Feed input frames; output comes back through the emit callback.
 * Returns false on allocation failure. */
bool wav_resample_push(struct wav_resample *rs,
		       const float *in, size_t in_frames);

/* End of input: emits the edge-clamped tail, if any. */
void wav_resample_finish(struct wav_resample *rs);

void wav_resample_free(struct wav_resample *rs);

/* ------------------------------------------------------------------ */
/* Streaming RIFF/WAVE chunk walker.                                   */
/* ------------------------------------------------------------------ */

/*
 * parse_wav() needs the whole file in memory.  This is the same
 * chunk walk as a fixed-size state machine: feed the file's bytes
 * as they arrive and it reports when the data chunk's body is
 * reached — everything after is PCM, up to the declared size or
 * EOF, whichever comes first (the same clamp parse_wav applies).
 * Two pathological inputs resolve differently from parse_wav:
 * the first data chunk wins (parse_wav's last-wins needs the whole
 * file), and fmt must precede data.
 */
enum wav_ps {
	WAV_PS_RIFF,	/* collecting the 12-byte RIFF header */
	WAV_PS_HDR,	/* collecting an 8-byte chunk header */
	WAV_PS_FMT,	/* collecting a fmt chunk body */
	WAV_PS_SKIP,	/* skipping a chunk body */
	WAV_PS_DATA,	/* data chunk reached */
	WAV_PS_ERR
};

struct wav_parse {
	unsigned char hdr[12];	/* RIFF header, then chunk headers */
	unsigned hdr_have;
	uint64_t pos;		/* bytes consumed */
	uint64_t skip;		/* body bytes left to skip */
	uint64_t data_off;	/* absolute offset of the data body */
	uint64_t data_decl;	/* declared data chunk size */
	unsigned char fmt[16];
	unsigned fmt_have;
	uint16_t format, bits;
	bool have_fmt;
	struct wav_info w;
	int state;
	const char *err;
};

void wav_parse_init(struct wav_parse *ps);

/*
 * Feed bytes; returns how many were consumed.  Anything less than
 * n means the data chunk was reached (the rest of the block is
 * PCM — the caller takes over) or an error (ps->err).
 */
size_t wav_parse_feed(struct wav_parse *ps, const unsigned char *p,
		      size_t n);

/*
 * Input ended before the data chunk was reached: report the
 * batch-equivalent rejection in ps->err and return false.
 */
bool wav_parse_eof(struct wav_parse *ps);

#endif /* BROWNNOTE_WAV_H */
