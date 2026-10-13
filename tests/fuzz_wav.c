/*
 * Fuzz harness for the WAV core (src/wav.c).  One entry point,
 * wav_fuzz_one(), exercised two ways:
 *
 *   make fuzz            corpus driver — runs every file in
 *                        tests/corpus plus a round of deterministic
 *                        mutations of each, under ASan/UBSan
 *   make fuzz-libfuzzer  clang/libFuzzer — LLVMFuzzerTestOneInput
 *
 * The target: batch-parse the blob; when it parses, convert a bounded
 * slice of its PCM and run it through both resamplers — batch and
 * streaming — which must agree exactly (that agreement is itself the
 * strongest check here).  The streaming chunk walker always runs,
 * under several feed blockings.
 *
 * parse_wav() reports its rejections on stderr — the chatter during
 * corpus and mutation runs is expected.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "wav.h"

/* Frames of a parsed file to actually resample: enough to exercise
 * the kernels and edges, small enough to keep mutations fast. */
#define FUZZ_MAX_FRAMES 384

struct collect {
	float *v;
	size_t n, cap;	/* n counts frames */
	unsigned ch;
};

static void collect_emit(void *ctx, const float *frames, size_t n)
{
	struct collect *c = ctx;
	size_t need = c->n + n;

	if (need > c->cap) {
		size_t cap = c->cap ? c->cap : 256;

		while (cap < need)
			cap *= 2;
		c->v = realloc(c->v, cap * c->ch * sizeof(*c->v));
		if (!c->v) {
			fprintf(stderr, "fuzz_wav: out of memory\n");
			exit(1);
		}
		c->cap = cap;
	}
	memcpy(c->v + c->n * c->ch, frames, n * c->ch * sizeof(*c->v));
	c->n += n;
}

/* The streaming resampler must emit exactly resample_frames' output. */
static void fuzz_resample(const float *f, size_t frames, unsigned ch,
			  unsigned in_rate, unsigned out_rate,
			  size_t blk)
{
	float *ref_p = NULL;
	size_t ref_n = resample_frames(f, frames, ch, in_rate, out_rate,
				       &ref_p);
	const float *ref = ref_p ? ref_p : f;
	struct collect c = { .ch = ch };
	struct wav_resample *rs;

	if (ref_n == (size_t)-1)
		return;
	rs = wav_resample_new(ch, in_rate, out_rate, collect_emit, &c);
	if (!rs) {
		free(ref_p);
		return;
	}
	if (!blk)
		blk = 1;
	for (size_t off = 0; off < frames; off += blk) {
		size_t k = frames - off < blk ? frames - off : blk;

		wav_resample_push(rs, f + off * ch, k);
	}
	wav_resample_finish(rs);
	if (c.n != ref_n ||
	    (c.n && memcmp(c.v, ref, c.n * ch * sizeof(float)))) {
		fprintf(stderr, "fuzz_wav: stream/batch resample mismatch "
			"(%zu vs %zu frames, %u->%u Hz)\n",
			c.n, ref_n, in_rate, out_rate);
		abort();
	}
	free(c.v);
	free(ref_p);
	wav_resample_free(rs);
}

static void wav_fuzz_one(const unsigned char *d, size_t n)
{
	struct wav_info w;
	struct wav_parse ps;
	static const size_t feeds[] = { 1, 7, 4096 };

	/* the batch parser; on success, resample a bounded slice */
	memset(&w, 0, sizeof(w));
	if (parse_wav(d, n, &w) && w.data_off + 4 <= n) {
		size_t frames = w.data_len / (2u * w.channels);
		float *f;

		if (frames > FUZZ_MAX_FRAMES)
			frames = FUZZ_MAX_FRAMES;
		/* the declared size may run past the buffer — clamp to it */
		if (frames > (n - w.data_off) / (2u * w.channels))
			frames = (n - w.data_off) / (2u * w.channels);
		f = malloc((frames ? frames : 1) * w.channels * sizeof(*f));
		if (f) {
			for (size_t i = 0; i < frames * w.channels; i++)
				f[i] = s16_to_f((int16_t)wav_rd16(
						d + w.data_off + 2 * i));
			fuzz_resample(f, frames, w.channels, w.rate, 48000, 1);
			fuzz_resample(f, frames, w.channels, w.rate, 8000, 13);
			fuzz_resample(f, frames, w.channels, w.rate, 192000, 0);
			free(f);
		}
	}

	/* the streaming chunk walker, under three feed blockings */
	for (size_t b = 0; b < sizeof(feeds) / sizeof(feeds[0]); b++) {
		size_t used = 0;

		wav_parse_init(&ps);
		while (used < n && ps.state != WAV_PS_DATA &&
		       ps.state != WAV_PS_ERR) {
			size_t k = n - used < feeds[b] ? n - used : feeds[b];

			used += wav_parse_feed(&ps, d + used, k);
		}
		if (ps.state != WAV_PS_DATA)
			(void)wav_parse_eof(&ps);
	}
}

#ifdef FUZZ_LIBFUZZER

int LLVMFuzzerTestOneInput(const uint8_t *d, size_t n)
{
	wav_fuzz_one(d, n);
	return 0;
}

#else /* corpus driver with deterministic mutations */

/*
 * SplitMix64: platform-independent PRNG so a mutation round is
 * reproducible everywhere.
 */
static uint64_t rng_state = 0x9E3779B97F4A7C15ULL;

static uint64_t rng_next(void)
{
	uint64_t z;

	rng_state += 0x9E3779B97F4A7C15ULL;
	z = rng_state;
	z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
	z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
	return z ^ (z >> 31);
}

#define MUTATIONS 48

/* one deterministic mutation round: byte flips, truncations, tails */
static void mutate_and_run(const unsigned char *d, size_t n)
{
	unsigned char *m = malloc(n + 64);

	if (!m)
		return;
	for (int i = 0; i < MUTATIONS; i++) {
		size_t mn = n;
		unsigned op = (unsigned)(rng_next() % 3);

		memcpy(m, d, n);
		switch (op) {
		case 0: {	/* 1-4 byte flips at random spots */
			unsigned flips = 1 + (unsigned)(rng_next() % 4);

			for (unsigned j = 0; j < flips && mn; j++)
				m[rng_next() % mn] ^= (unsigned char)rng_next();
			break;
		}
		case 1:		/* truncate */
			mn = mn ? (size_t)(rng_next() % (uint64_t)mn) : 0;
			break;
		default:	/* append a few junk bytes */
			for (unsigned j = 0; j < 1 + rng_next() % 8; j++)
				m[mn++] = (unsigned char)rng_next();
			break;
		}
		wav_fuzz_one(m, mn);
	}
	free(m);
}

int main(int argc, char **argv)
{
	int bad = 0;

	if (argc < 2) {
		fprintf(stderr, "usage: %s FILE...\n", argv[0]);
		return 2;
	}
	for (int i = 1; i < argc; i++) {
		FILE *fp = fopen(argv[i], "rb");
		unsigned char *d;
		size_t n;

		if (!fp) {
			perror(argv[i]);
			bad = 1;
			continue;
		}
		d = malloc(1 << 20);
		if (!d) {
			fclose(fp);
			return 1;
		}
		n = fread(d, 1, 1 << 20, fp);
		fclose(fp);
		/* mutations are seeded from the content, so a run is
		 * reproducible byte-for-byte on every platform */
		for (size_t j = 0; j < n; j++)
			rng_state = rng_state * 1099511628211ULL ^ d[j];
		wav_fuzz_one(d, n);
		mutate_and_run(d, n);
		free(d);
		printf("fuzz_wav: %s (%zu bytes, original + %d mutations)\n",
		       argv[i], n, MUTATIONS);
	}
	return bad;
}
#endif
