/*
 * Unit tests for the WAV core (src/wav.c): the RIFF/WAVE parser, the
 * sample conversions and the resampler.  Plain checks, no framework;
 * build and run with `make unit` (also part of `make test`).
 *
 * parse_wav() reports its rejections on stderr — one line per negative
 * case below; that chatter is expected.
 */

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "wav.h"

static int checks, failures;

#define CHECK(cond) do {						\
	checks++;							\
	if (!(cond)) {							\
		failures++;						\
		printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond);	\
	}								\
} while (0)

/* ------------------------------------------------------------------ */
/* WAV construction helpers.                                           */
/* ------------------------------------------------------------------ */

struct blob {
	unsigned char *p;
	size_t len, cap;
};

static void put(struct blob *b, const void *src, size_t n)
{
	if (!n)
		return;
	if (b->len + n > b->cap) {
		size_t cap = b->cap ? b->cap : 256;

		while (cap < b->len + n)
			cap *= 2;
		b->p = realloc(b->p, cap);
		if (!b->p) {
			fprintf(stderr, "unit_wav: out of memory\n");
			exit(1);
		}
		b->cap = cap;
	}
	memcpy(b->p + b->len, src, n);
	b->len += n;
}

static void put_id(struct blob *b, const char *id) { put(b, id, 4); }

static void put32(struct blob *b, uint32_t v)
{
	unsigned char t[4];

	wr32(t, v);
	put(b, t, 4);
}

/* Append one chunk: id + size + body + pad byte when the body is odd. */
static void chunk(struct blob *b, const char *id, const void *body, size_t n)
{
	put_id(b, id);
	put32(b, (uint32_t)n);
	put(b, body, n);
	if (n & 1)
		put(b, "\0", 1);
}

static void fmt_chunk(struct blob *b, uint16_t format, uint16_t channels,
		      uint32_t rate, uint16_t bits)
{
	unsigned char f[16];

	wr16(f + 0, format);
	wr16(f + 2, channels);
	wr32(f + 4, rate);
	wr32(f + 8, rate * channels * 2);	/* byte rate, unchecked */
	wr16(f + 12, (uint16_t)(channels * 2));	/* block align, unchecked */
	wr16(f + 14, bits);
	chunk(b, "fmt ", f, sizeof(f));
}

/* RIFF/WAVE + fmt chunk; wav_end() patches the RIFF size. */
static void wav_begin(struct blob *b, uint16_t format, uint16_t channels,
		      uint32_t rate, uint16_t bits)
{
	memset(b, 0, sizeof(*b));
	put_id(b, "RIFF");
	put32(b, 0);
	put_id(b, "WAVE");
	fmt_chunk(b, format, channels, rate, bits);
}

static void wav_end(struct blob *b)
{
	wr32(b->p + 4, (uint32_t)(b->len - 8));
}

/* ------------------------------------------------------------------ */
/* parse_wav.                                                          */
/* ------------------------------------------------------------------ */

static void test_parse_valid(void)
{
	struct blob b;
	struct wav_info w;
	int16_t pcm[4] = { 1000, -1000, 2000, -2000 };

	wav_begin(&b, 1, 2, 48000, 16);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(parse_wav(b.p, b.len, &w));
	CHECK(w.channels == 2);
	CHECK(w.rate == 48000);
	CHECK(w.data_off == 44);
	CHECK(w.data_len == sizeof(pcm));
	CHECK(rd16(b.p + w.data_off) == 1000);
	CHECK((int16_t)rd16(b.p + w.data_off + 6) == -2000);
	free(b.p);
}

static void test_parse_chunk_walking(void)
{
	struct blob b;
	struct wav_info w;
	int16_t pcm[2] = { 111, -222 };
	const char junk[] = "INFOj";	/* odd body: 5 bytes + pad */

	/* a LIST chunk with an odd body must be skipped whole, pad and
	 * all, so the data chunk after it is found where it belongs */
	wav_begin(&b, 1, 1, 44100, 16);
	chunk(&b, "LIST", junk, sizeof(junk) - 1);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(parse_wav(b.p, b.len, &w));
	CHECK(w.channels == 1);
	CHECK(w.rate == 44100);
	CHECK(w.data_off == 12 + 24 + (8 + 5 + 1) + 8);
	CHECK(w.data_len == sizeof(pcm));
	free(b.p);
}

static void test_parse_clamps_oversized_data(void)
{
	struct blob b;
	struct wav_info w;
	int16_t pcm[2] = { 5, -5 };

	/* the 0xFFFFFFFF of streaming writers: runs to EOF, clamped */
	wav_begin(&b, 1, 1, 44100, 16);
	put_id(&b, "data");
	put32(&b, 0xFFFFFFFF);
	put(&b, pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(parse_wav(b.p, b.len, &w));
	CHECK(w.data_len == sizeof(pcm));
	free(b.p);
}

static void test_parse_size_zero_data(void)
{
	struct blob b;
	struct wav_info w;

	/* a declared size of 0 is an empty chunk: the chunks after it
	 * are metadata, never audio (#8) */
	wav_begin(&b, 1, 1, 44100, 16);
	put_id(&b, "data");
	put32(&b, 0);
	chunk(&b, "LIST", "INFO-pad-pad-pad-pad-pad", 24);
	wav_end(&b);
	CHECK(parse_wav(b.p, b.len, &w));
	CHECK(w.data_len == 0);
	free(b.p);
}

static void test_parse_rejections(void)
{
	struct blob b;
	struct wav_info w;
	int16_t pcm[2] = { 1, -1 };

	/* wrong magic */
	wav_begin(&b, 1, 1, 44100, 16);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	b.p[0] = 'X';
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);

	/* no fmt chunk (a LIST pads the file past the 44-byte minimum) */
	memset(&b, 0, sizeof(b));
	put_id(&b, "RIFF");
	put32(&b, 0);
	put_id(&b, "WAVE");
	chunk(&b, "LIST", "INFO-pad-pad-pad-pad-pad", 24);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);

	/* no data chunk */
	wav_begin(&b, 1, 1, 44100, 16);
	chunk(&b, "LIST", "INFO-pad-pad-pad-pad-pad", 24);
	wav_end(&b);
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);

	/* not PCM */
	wav_begin(&b, 3, 1, 44100, 16);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);

	/* not 16 bits */
	wav_begin(&b, 1, 1, 44100, 24);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);

	/* channel counts outside mono/stereo */
	wav_begin(&b, 1, 0, 44100, 16);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);

	wav_begin(&b, 1, 3, 44100, 16);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);

	/* rate bounds: 8000 and 192000 are in, one past either is out */
	wav_begin(&b, 1, 1, 7999, 16);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);

	wav_begin(&b, 1, 1, 192001, 16);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);

	wav_begin(&b, 1, 1, 8000, 16);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(parse_wav(b.p, b.len, &w));
	CHECK(w.rate == 8000);
	free(b.p);

	wav_begin(&b, 1, 1, 192000, 16);
	chunk(&b, "data", pcm, sizeof(pcm));
	wav_end(&b);
	CHECK(parse_wav(b.p, b.len, &w));
	CHECK(w.rate == 192000);
	free(b.p);

	/* data length not frame-aligned (3 bytes of mono audio) */
	wav_begin(&b, 1, 1, 44100, 16);
	chunk(&b, "data", "\1\2\3", 3);
	wav_end(&b);
	CHECK(!parse_wav(b.p, b.len, &w));
	free(b.p);
}

/* ------------------------------------------------------------------ */
/* Sample conversion.                                                  */
/* ------------------------------------------------------------------ */

static void test_conversions(void)
{
	int worst = 0;

	CHECK(s16_to_f(0) == 0.0f);
	CHECK(s16_to_f(-32768) == -1.0f);
	CHECK(s16_to_f(32767) < 1.0f);	/* a hair under full scale */
	CHECK(s16_to_f(32767) > 0.9999f);

	CHECK(f_to_s16(0.0f) == 0);
	CHECK(f_to_s16(1.0f) == 32767);	/* clamped, not wrapped */
	CHECK(f_to_s16(-1.0f) == -32767);
	CHECK(f_to_s16(2.0f) == 32767);
	CHECK(f_to_s16(-2.0f) == -32767);
	CHECK(f_to_s16(0.5f) == 16384);	/* lroundf(16383.5) */

	/* round-tripping is bounded by one LSB, for every code */
	for (int v = -32768; v <= 32767; v++) {
		int16_t rt = f_to_s16(s16_to_f((int16_t)v));
		int d = rt - v;

		if (d < 0)
			d = -d;
		if (d > worst)
			worst = d;
	}
	CHECK(worst <= 1);
}

/* ------------------------------------------------------------------ */
/* Resampling.                                                         */
/* ------------------------------------------------------------------ */

static double rms(const float *v, size_t n)
{
	double s = 0.0;

	for (size_t i = 0; i < n; i++)
		s += (double)v[i] * v[i];
	return n ? sqrt(s / (double)n) : 0.0;
}

static void test_resample_identity(void)
{
	float in[8] = { 0, 0.25f, 0.5f, 0.75f, 1.0f, 0.5f, 0.25f, 0 };
	float *out = (float *)1;	/* poison: the call must set it */

	CHECK(resample_frames(in, 4, 2, 48000, 48000, &out) == 4);
	CHECK(out == NULL);		/* caller keeps the input buffer */

	out = (float *)1;
	CHECK(resample_frames(in, 0, 2, 44100, 48000, &out) == 0);
	CHECK(out == NULL);
}

static void test_resample_counts_and_nodes(void)
{
	float in[8], *out = NULL;
	size_t n;

	for (int i = 0; i < 8; i++)
		in[i] = (float)i;	/* a ramp */

	/* up 2x: 4 frames -> 8, and the cubic passes through the
	 * input samples at the integer positions */
	n = resample_frames(in, 4, 1, 48000, 96000, &out);
	CHECK(n == 8);
	CHECK(out != NULL);
	for (size_t j = 0; j < n; j += 2)
		CHECK(fabsf(out[j] - (float)(j / 2)) < 1e-5f);
	free(out);

	/* down 2x: 8 frames -> 4 */
	out = NULL;
	n = resample_frames(in, 8, 1, 96000, 48000, &out);
	CHECK(n == 4);
	CHECK(out != NULL);
	free(out);
}

static void test_resample_interleave(void)
{
	/* constant L/R must stay constant and separate at any rate */
	float in[16], *out = NULL;
	size_t n;

	for (int i = 0; i < 8; i++) {
		in[2 * i] = 0.75f;
		in[2 * i + 1] = -0.5f;
	}
	n = resample_frames(in, 8, 2, 44100, 48000, &out);
	CHECK(n == (size_t)llround(8.0 * 48000 / 44100));
	CHECK(out != NULL);
	for (size_t i = 0; i < n; i++) {
		CHECK(fabsf(out[2 * i] - 0.75f) < 1e-5f);
		CHECK(fabsf(out[2 * i + 1] + 0.5f) < 1e-5f);
	}
	free(out);
}

/*
 * #32: decimation must not fold out-of-band energy into the
 * passband.  A 30 kHz tone is above the 48 kHz output Nyquist
 * (24 kHz) and would alias to 18 kHz at full amplitude without the
 * low-pass; a 10 kHz tone is the in-band control that must survive.
 */
static void test_antialias(void)
{
	enum { N = 4800 };	/* 25 ms at 192 kHz */
	float *in = malloc(N * sizeof(float));
	float *out = NULL;
	size_t n;

	for (size_t i = 0; i < N; i++)
		in[i] = 0.4f * sinf(2.0f * (float)M_PI * 30000.0f *
				   (float)i / 192000.0f);
	n = resample_frames(in, N, 1, 192000, 48000, &out);
	CHECK(n == N / 4);
	CHECK(out != NULL);
	CHECK(rms(out, n) < 0.1 * rms(in, N));	/* alias suppressed */
	free(out);

	for (size_t i = 0; i < N; i++)
		in[i] = 0.4f * sinf(2.0f * (float)M_PI * 10000.0f *
				   (float)i / 192000.0f);
	out = NULL;
	n = resample_frames(in, N, 1, 192000, 48000, &out);
	CHECK(n == N / 4);
	CHECK(out != NULL);
	CHECK(rms(out, n) > 0.9 * rms(in, N));	/* passband preserved */
	CHECK(rms(out, n) < 1.1 * rms(in, N));
	free(out);

	/* DC gain is exactly 1 in the interior: a constant survives
	 * decimation.  (The filter's zero-padded edges ramp in and out
	 * over AA_TAPS/2 input frames — expected, so the check skips
	 * them.) */
	for (size_t i = 0; i < N; i++)
		in[i] = 0.25f;
	out = NULL;
	n = resample_frames(in, N, 1, 192000, 48000, &out);
	CHECK(n == N / 4);
	float maxdev = 0.0f;

	for (size_t j = 16; j + 16 < n; j++) {
		float d = fabsf(out[j] - 0.25f);

		if (d > maxdev)
			maxdev = d;
	}
	CHECK(maxdev < 1e-4f);
	free(out);
	free(in);
}

/* ------------------------------------------------------------------ */

int main(void)
{
	test_parse_valid();
	test_parse_chunk_walking();
	test_parse_clamps_oversized_data();
	test_parse_size_zero_data();
	test_parse_rejections();
	test_conversions();
	test_resample_identity();
	test_resample_counts_and_nodes();
	test_resample_interleave();
	test_antialias();

	if (failures) {
		printf("unit_wav: FAILED %d/%d checks\n", failures, checks);
		return 1;
	}
	printf("unit_wav: PASSED %d/%d checks\n", checks, checks);
	return 0;
}
