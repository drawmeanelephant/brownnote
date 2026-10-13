/*
 * brownnote's WAV core: the RIFF/WAVE parser, the sample conversions
 * and the resampler.  Everything that touches the file format or the
 * sample math, split out of brownnote.c so it can be tested
 * (tests/unit_wav.c, `make unit`) and fuzzed without the CLI around
 * it.  Nothing here knows about pipes, benches or exit codes:
 * parse_wav reports on stderr and returns false, and resample_frames
 * reports allocation failure with (size_t)-1.
 */

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "wav.h"

/* ------------------------------------------------------------------ */
/* WAV: the file format fart-app writes and zcn checks.               */
/* ------------------------------------------------------------------ */

uint32_t wav_rd32(const unsigned char *p)
{
	return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
	       ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

uint16_t wav_rd16(const unsigned char *p)
{
	return (uint16_t)(p[0] | (p[1] << 8));
}

void wav_wr32(unsigned char *p, uint32_t v)
{
	p[0] = v; p[1] = v >> 8; p[2] = v >> 16; p[3] = v >> 24;
}

void wav_wr16(unsigned char *p, uint16_t v)
{
	p[0] = v; p[1] = v >> 8;
}

/*
 * Parse RIFF/WAVE: PCM s16le, mono or stereo, any rate 8-192 kHz.
 * Chunks are walked properly, so LIST and friends pass by; a chunk
 * whose size runs past the end of the file is clamped.
 */
bool parse_wav(const unsigned char *p, size_t len, struct wav_info *w)
{
	if (len < 44 || memcmp(p, "RIFF", 4) || memcmp(p + 8, "WAVE", 4)) {
		fprintf(stderr, "brownnote: not a RIFF/WAVE file\n");
		return false;
	}
	size_t pos = 12;
	bool have_fmt = false, have_data = false;
	uint16_t format = 0, bits = 0;

	while (pos + 8 <= len) {
		uint32_t csz = wav_rd32(p + pos + 4);

		if (pos + 8 + csz > len)
			csz = (uint32_t)(len - pos - 8);
		const unsigned char *body = p + pos + 8;

		if (!memcmp(p + pos, "fmt ", 4) && csz >= 16) {
			format = wav_rd16(body);
			w->channels = wav_rd16(body + 2);
			w->rate = wav_rd32(body + 4);
			bits = wav_rd16(body + 14);
			have_fmt = true;
		} else if (!memcmp(p + pos, "data", 4)) {
			w->data_off = pos + 8;
			/*
			 * A declared size of 0 is an empty chunk — every
			 * chunk after it is metadata, not audio.  Only a
			 * size that runs past the end of the file (the
			 * 0xFFFFFFFF of streaming writers) is the
			 * to-EOF fallback, by way of the clamp above.
			 */
			w->data_len = csz;
			have_data = true;
		}
		pos += 8 + csz + (csz & 1);	/* chunks are word-aligned */
	}

	if (!have_fmt || !have_data) {
		fprintf(stderr, "brownnote: WAV is missing fmt or data\n");
		return false;
	}
	if (format != 1 || bits != 16) {
		fprintf(stderr, "brownnote: WAV must be PCM s16le "
			"(got format %u, %u bits)\n", format, bits);
		return false;
	}
	if (w->channels < 1 || w->channels > 2) {
		fprintf(stderr, "brownnote: WAV must be mono or stereo "
			"(got %u channels)\n", w->channels);
		return false;
	}
	if (w->rate < 8000 || w->rate > 192000) {
		fprintf(stderr, "brownnote: WAV rate %u out of range "
			"(8000..192000)\n", w->rate);
		return false;
	}
	if (w->data_len % (2 * w->channels)) {
		fprintf(stderr, "brownnote: WAV data length not frame-aligned\n");
		return false;
	}
	return true;
}

/* ------------------------------------------------------------------ */
/* Sample conversion and resampling.                                   */
/* ------------------------------------------------------------------ */

/*
 * s16 -> float: v/32768, so full scale lands a hair under 1.0.
 * float -> s16: lroundf(clamp(v)*32767).  Asymmetric on purpose —
 * the common convention — and commented here so nobody "fixes" it:
 * round-tripping is not identity, it is bounded by one LSB.
 */
float s16_to_f(int16_t v) { return (float)v / 32768.0f; }
int16_t f_to_s16(float v)
{
	if (v >= 1.0f) return 32767;
	if (v <= -1.0f) return -32767;
	return (int16_t)lroundf(v * 32767.0f);
}

/*
 * Anti-aliasing for decimation.  The cubic interpolator is a fine
 * upsampler, but used to drop rate it merely samples the input at
 * the output rate, folding everything above the output Nyquist back
 * into the passband: a 30 kHz tone in a 192 kHz file comes out of a
 * 48 kHz decimation as 18 kHz, at full amplitude.  And even with a
 * prefilter in front of it, the cubic itself rolls off steeply near
 * the input Nyquist (-2.8 dB at 19 kHz from 48 kHz), which no
 * filter can buy back.
 *
 * So decimation interpolates with a Kaiser-windowed sinc evaluated
 * at each output sample's fractional input position — a polyphase
 * resampler written without phase tables.  The kernel is stretched
 * by the ratio, so the same convolution anti-aliases and
 * interpolates: cutoff at 95% of the output Nyquist, ~-60 dB of
 * stopband where aliases would land, and a passband flat to a
 * fraction of a dB.  Each output is normalized by its own tap sum
 * and taps that reach past an edge clamp to the edge sample, which
 * keeps a constant signal flat right up to the first and last
 * frame.  Transport plumbing, same as the resampler itself — the
 * minimum that keeps the round trip honest, not a DSP statement.
 */
#define AA_BETA		8.6	/* Kaiser: ~-70 dB nominal stopband */
#define AA_CUTOFF	0.475	/* of out_rate, in input samples */
#define AA_MIN_TAPS	65	/* odd, so the kernel is centred */
#define AA_MAX_TAPS	2049
#define AA_BASE_TAPS	65	/* taps scale with in_rate/out_rate */

static double sinc(double x)
{
	return x == 0.0 ? 1.0 : sin(M_PI * x) / (M_PI * x);
}

/*
 * Modified Bessel function I0 by series expansion.  Converges fast
 * for the |x| <= AA_BETA range a Kaiser window needs.
 */
static double bessel_i0(double x)
{
	double y = (x / 2.0) * (x / 2.0), sum = 1.0, t = 1.0;

	for (int k = 1; k < 50; k++) {
		t *= y / ((double)k * k);
		sum += t;
		if (t < 1e-12 * sum)
			break;
	}
	return sum;
}

/* Kaiser-windowed sinc: 2*fc*sinc(2*fc*u)*w(u), u in input samples. */
static double aa_kernel(double u, double half, double fc)
{
	double x = u / half;
	double w;

	if (x < -1.0 || x > 1.0)
		return 0.0;
	w = bessel_i0(AA_BETA * sqrt(1.0 - x * x)) / bessel_i0(AA_BETA);
	return 2.0 * fc * sinc(2.0 * fc * u) * w;
}

static size_t aa_taps(unsigned in_rate, unsigned out_rate)
{
	double t = AA_BASE_TAPS * (double)in_rate / (double)out_rate;
	size_t taps;

	if (t < AA_MIN_TAPS)
		t = AA_MIN_TAPS;
	if (t > AA_MAX_TAPS)
		t = AA_MAX_TAPS;
	taps = (size_t)llround(t);
	return (taps & 1) ? taps : taps + 1;
}

/*
 * One output frame of the bandlimited decimator: the kernel-weighted
 * sum of `taps` samples around input position p, normalized.  The
 * samples are read through a sliding window: in[k] is input frame
 * k + sub, meaningful for k + sub in [0, total); indices past either
 * end clamp to the edge sample — the same edge behaviour at the
 * head of the file and, once total is the full count, the tail.
 * *h and *h_frac are a one-phase kernel cache: for integer ratios
 * every output sits at the same fractional offset and the kernel is
 * evaluated once.
 */
static void decimate_frame(const float *in, size_t sub, size_t total,
			   unsigned channels, size_t taps, double half,
			   double fc, double p, double *h, double *h_frac,
			   float *out)
{
	double frac = p - floor(p);
	long centre = (long)floor(p);
	double norm = 0.0;

	if (frac != *h_frac) {
		for (size_t n = 0; n < taps; n++) {
			double u = (double)((long)n -
				(long)(taps - 1) / 2) - frac;
			h[n] = aa_kernel(u, half, fc);
			norm += h[n];
		}
		*h_frac = frac;
	} else {
		for (size_t n = 0; n < taps; n++)
			norm += h[n];
	}

	for (unsigned c = 0; c < channels; c++) {
		double acc = 0.0;

		for (size_t n = 0; n < taps; n++) {
			long i = centre - (long)(taps - 1) / 2 + (long)n;

			if (i < 0)
				i = 0;	/* clamp to the edge sample */
			if (i >= (long)total)
				i = (long)total - 1;
			acc += h[n] * in[(size_t)(i - (long)sub) * channels + c];
		}
		out[c] = (float)(acc / norm);
	}
}

/*
 * Decimate interleaved frames by bandlimited interpolation: output
 * frame j sits at input position j*in_rate/out_rate, and is the
 * kernel-weighted sum of the taps around it.  Identical to a
 * polyphase bank with one phase per distinct fractional offset.
 * Writes into out, which the caller sized for out_frames*channels.
 */
static void decimate(const float *in, size_t in_frames, unsigned channels,
		     unsigned in_rate, unsigned out_rate,
		     size_t out_frames, float *out)
{
	const size_t taps = aa_taps(in_rate, out_rate);
	const double half = (taps - 1) / 2.0;
	const double fc = AA_CUTOFF * (double)out_rate / (double)in_rate;
	double h[AA_MAX_TAPS];
	double h_frac = -1.0;	/* frac the cached kernel was built for */

	for (size_t j = 0; j < out_frames; j++) {
		double p = (double)j * (double)in_rate / (double)out_rate;

		decimate_frame(in, 0, in_frames, channels, taps, half, fc,
			       p, h, &h_frac, out + j * channels);
	}
}

/*
 * One output frame of the Catmull-Rom upsampler at input position
 * p: the cubic through the four surrounding samples, read through
 * the same sliding window as decimate_frame (in[k] is input frame
 * k + sub, indices past the ends clamp).
 */
static void cubic_frame(const float *in, size_t sub, size_t total,
			unsigned channels, double p, float *out)
{
	size_t i = (size_t)p;
	double t = p - (double)i;
	size_t i0 = i ? i - 1 : 0;
	size_t i2 = (i + 1 < total) ? i + 1 : total - 1;
	size_t i3 = (i + 2 < total) ? i + 2 : total - 1;

	for (unsigned c = 0; c < channels; c++) {
		double x0 = in[(i0 - sub) * channels + c];
		double x1 = in[(i - sub) * channels + c];
		double x2 = in[(i2 - sub) * channels + c];
		double x3 = in[(i3 - sub) * channels + c];
		double a0 = -0.5 * x0 + 1.5 * x1 - 1.5 * x2 + 0.5 * x3;
		double a1 = x0 - 2.5 * x1 + 2.0 * x2 - 0.5 * x3;
		double a2 = -0.5 * x0 + 0.5 * x2;
		double a3 = x1;
		out[c] = (float)(((a0 * t + a1) * t + a2) * t + a3);
	}
}

/*
 * Catmull-Rom cubic resampling, per channel, over interleaved frames.
 * Transport plumbing to meet the bench's 48 kHz — not an effect; it
 * is the same interpolation any file converter does.  Decimation
 * (in_rate > out_rate) runs the bandlimited interpolator above, so
 * energy above the output Nyquist is strongly attenuated rather
 * than folded back into the passband; upsampling needs no filter —
 * interpolation cannot alias.
 */
size_t resample_frames(const float *in, size_t in_frames,
		       unsigned channels,
		       unsigned in_rate, unsigned out_rate,
		       float **out_p)
{
	if (in_rate == out_rate || in_frames == 0) {
		*out_p = NULL;
		return in_frames;	/* caller keeps the input buffer */
	}

	size_t out_frames = (size_t)llround(
		(double)in_frames * (double)out_rate / (double)in_rate);
	/*
	 * out_frames can legitimately be 0 — one 192 kHz frame is less
	 * than half a 48 kHz frame — and malloc(0) may return NULL.
	 */
	float *out = malloc(out_frames * channels * sizeof(float) + 1);
	if (!out) {
		*out_p = NULL;
		return (size_t)-1;
	}

	if (in_rate > out_rate) {
		decimate(in, in_frames, channels, in_rate, out_rate,
			 out_frames, out);
		*out_p = out;
		return out_frames;
	}

	for (size_t j = 0; j < out_frames; j++) {
		double p = (double)j * (double)in_rate / (double)out_rate;

		cubic_frame(in, 0, in_frames, channels, p,
			    out + j * channels);
	}
	*out_p = out;
	return out_frames;
}

/* ------------------------------------------------------------------ */
/* Streaming resampler: resample_frames fed in blocks, byte-for-byte.  */
/* ------------------------------------------------------------------ */

/*
 * The sliding input window.  Output frame j reads input frames
 * [floor(p_j) - back, floor(p_j) + ahead] — half the kernel either
 * side for the decimator, one frame behind and two ahead for the
 * cubic — clamped to [0, total).  An output is ready once its whole
 * window has arrived; the tail that is still waiting on look-ahead
 * is emitted by wav_resample_finish with the end clamp, matching the
 * batch version's edges exactly.
 */
struct wav_resample {
	unsigned channels, in_rate, out_rate;
	wav_emit_fn emit;
	void *emit_ctx;

	size_t taps;		/* decimation kernel size */
	double half, fc;
	size_t back, ahead;	/* window around floor(p) */

	float *win;		/* interleaved input from frame `base` */
	size_t win_cap;		/* capacity, frames */
	size_t base;		/* input index of win[0] */
	size_t win_len;		/* frames held: base .. base+win_len */
	size_t in_seen;		/* total pushed */
	size_t out_done;	/* emitted so far */

	float *obuf;		/* emit staging */
	size_t obuf_len, obuf_cap;

	double h[AA_MAX_TAPS];
	double h_frac;
};

#define RS_EMIT_CHUNK 4096	/* frames per emit callback, at most */

struct wav_resample *wav_resample_new(unsigned channels,
				      unsigned in_rate, unsigned out_rate,
				      wav_emit_fn emit, void *emit_ctx)
{
	struct wav_resample *rs = calloc(1, sizeof(*rs));

	if (!rs)
		return NULL;
	rs->channels = channels;
	rs->in_rate = in_rate;
	rs->out_rate = out_rate;
	rs->emit = emit;
	rs->emit_ctx = emit_ctx;
	rs->h_frac = -1.0;
	if (in_rate > out_rate) {
		rs->taps = aa_taps(in_rate, out_rate);
		rs->half = (rs->taps - 1) / 2.0;
		rs->fc = AA_CUTOFF * (double)out_rate / (double)in_rate;
		rs->back = rs->ahead = (rs->taps - 1) / 2;
	} else if (in_rate < out_rate) {
		rs->back = 1;	/* the cubic's x0 */
		rs->ahead = 2;	/* and its x3 */
	}
	/* equal rates: back/ahead stay 0 and push passes through */
	rs->obuf = malloc(RS_EMIT_CHUNK * channels * sizeof(float));
	if (!rs->obuf) {
		free(rs);
		return NULL;
	}
	rs->obuf_cap = RS_EMIT_CHUNK;
	return rs;
}

void wav_resample_free(struct wav_resample *rs)
{
	if (!rs)
		return;
	free(rs->win);
	free(rs->obuf);
	free(rs);
}

static void rs_flush_obuf(struct wav_resample *rs)
{
	if (rs->obuf_len) {
		rs->emit(rs->emit_ctx, rs->obuf, rs->obuf_len);
		rs->obuf_len = 0;
	}
}

/*
 * Output frame j is ready once every input frame its window needs
 * has arrived: floor(p_j) + ahead <= the last input index seen.
 */
static bool rs_ready(struct wav_resample *rs, size_t j)
{
	double p = (double)j * (double)rs->in_rate / (double)rs->out_rate;
	size_t i = (size_t)p;

	return rs->in_seen > 0 && i + rs->ahead <= rs->in_seen - 1;
}

static void rs_emit_frame(struct wav_resample *rs, size_t j)
{
	double p = (double)j * (double)rs->in_rate / (double)rs->out_rate;
	float *dst = rs->obuf + rs->obuf_len * rs->channels;

	if (rs->in_rate > rs->out_rate)
		decimate_frame(rs->win, rs->base, rs->in_seen, rs->channels,
			       rs->taps, rs->half, rs->fc, p,
			       rs->h, &rs->h_frac, dst);
	else
		cubic_frame(rs->win, rs->base, rs->in_seen, rs->channels,
			    p, dst);
	if (++rs->obuf_len == rs->obuf_cap)
		rs_flush_obuf(rs);
}

bool wav_resample_push(struct wav_resample *rs,
		       const float *in, size_t in_frames)
{
	if (rs->in_rate == rs->out_rate) {
		/* passthrough: emit the block as-is */
		if (in_frames)
			rs->emit(rs->emit_ctx, in, in_frames);
		rs->in_seen += in_frames;
		rs->out_done += in_frames;
		return true;
	}

	/* Drop history below the next output's window. */
	if (rs->win_len) {
		double p = (double)rs->out_done * (double)rs->in_rate /
			(double)rs->out_rate;
		long lo = (long)p - (long)rs->back;

		if (lo > (long)rs->base) {
			size_t drop = (size_t)lo - rs->base;

			if (drop > rs->win_len)
				drop = rs->win_len;
			memmove(rs->win, rs->win + drop * rs->channels,
				(rs->win_len - drop) * rs->channels *
				sizeof(float));
			rs->base += drop;
			rs->win_len -= drop;
		}
	}

	if (rs->win_len + in_frames > rs->win_cap) {
		size_t cap = rs->win_cap ? rs->win_cap : 4096;
		float *w;

		while (cap < rs->win_len + in_frames)
			cap *= 2;
		w = realloc(rs->win, cap * rs->channels * sizeof(float));
		if (!w)
			return false;
		rs->win = w;
		rs->win_cap = cap;
	}
	memcpy(rs->win + rs->win_len * rs->channels, in,
	       in_frames * rs->channels * sizeof(float));
	rs->win_len += in_frames;
	rs->in_seen += in_frames;

	while (rs->out_done < (size_t)-1 && rs_ready(rs, rs->out_done)) {
		rs_emit_frame(rs, rs->out_done);
		rs->out_done++;
	}
	rs_flush_obuf(rs);
	return true;
}

void wav_resample_finish(struct wav_resample *rs)
{
	size_t out_total;

	if (rs->in_rate == rs->out_rate)
		return;		/* everything passed through already */

	out_total = (size_t)llround((double)rs->in_seen *
		(double)rs->out_rate / (double)rs->in_rate);
	while (rs->out_done < out_total) {
		rs_emit_frame(rs, rs->out_done);
		rs->out_done++;
	}
	rs_flush_obuf(rs);
}

/* ------------------------------------------------------------------ */
/* Streaming RIFF/WAVE chunk walker.                                   */
/* ------------------------------------------------------------------ */

void wav_parse_init(struct wav_parse *ps)
{
	memset(ps, 0, sizeof(*ps));
	ps->state = WAV_PS_RIFF;
}

static void wav_parse_fail(struct wav_parse *ps, const char *err)
{
	ps->err = err;
	ps->state = WAV_PS_ERR;
}

size_t wav_parse_feed(struct wav_parse *ps, const unsigned char *p,
		      size_t n)
{
	size_t used = 0;

	while (used < n && ps->state != WAV_PS_DATA &&
	       ps->state != WAV_PS_ERR) {
		switch (ps->state) {
		case WAV_PS_RIFF: {
			size_t k = 12 - ps->hdr_have;

			if (k > n - used)
				k = n - used;
			memcpy(ps->hdr + ps->hdr_have, p + used, k);
			ps->hdr_have += k;
			used += k;
			ps->pos += k;
			if (ps->hdr_have == 12) {
				if (memcmp(ps->hdr, "RIFF", 4) ||
				    memcmp(ps->hdr + 8, "WAVE", 4))
					wav_parse_fail(ps,
						"not a RIFF/WAVE file");
				else {
					ps->state = WAV_PS_HDR;
					ps->hdr_have = 0;
				}
			}
			break;
		}
		case WAV_PS_HDR: {
			size_t k = 8 - ps->hdr_have;
			uint32_t csz;

			if (k > n - used)
				k = n - used;
			memcpy(ps->hdr + ps->hdr_have, p + used, k);
			ps->hdr_have += k;
			used += k;
			ps->pos += k;
			if (ps->hdr_have < 8)
				break;
			csz = wav_rd32(ps->hdr + 4);
			if (!memcmp(ps->hdr, "fmt ", 4) && csz >= 16) {
				ps->state = WAV_PS_FMT;
				ps->fmt_have = 0;
				ps->skip = csz - 16 + (csz & 1);
			} else if (!memcmp(ps->hdr, "data", 4)) {
				ps->data_off = ps->pos;
				ps->data_decl = csz;
				ps->w.data_off = ps->pos;
				ps->state = WAV_PS_DATA;
			} else {
				ps->skip = (uint64_t)csz + (csz & 1);
				ps->state = WAV_PS_SKIP;
			}
			ps->hdr_have = 0;
			break;
		}
		case WAV_PS_FMT: {
			size_t k = 16 - ps->fmt_have;

			if (k > n - used)
				k = n - used;
			memcpy(ps->fmt + ps->fmt_have, p + used, k);
			ps->fmt_have += k;
			used += k;
			ps->pos += k;
			if (ps->fmt_have == 16) {
				ps->format = wav_rd16(ps->fmt);
				ps->w.channels = wav_rd16(ps->fmt + 2);
				ps->w.rate = wav_rd32(ps->fmt + 4);
				ps->bits = wav_rd16(ps->fmt + 14);
				ps->have_fmt = true;
				ps->state = ps->skip ? WAV_PS_SKIP : WAV_PS_HDR;
			}
			break;
		}
		case WAV_PS_SKIP: {
			uint64_t k = ps->skip;

			if (k > n - used)
				k = n - used;
			used += (size_t)k;
			ps->pos += k;
			ps->skip -= k;
			if (!ps->skip)
				ps->state = WAV_PS_HDR;
			break;
		}
		}
	}
	return used;
}

bool wav_parse_eof(struct wav_parse *ps)
{
	/*
	 * The batch parser rejects any file under 44 bytes as "not a
	 * RIFF/WAVE" before looking at the magic; keep the same order.
	 */
	if (ps->pos < 44) {
		wav_parse_fail(ps, "not a RIFF/WAVE file");
		return false;
	}
	wav_parse_fail(ps, "WAV is missing fmt or data");
	return false;
}
