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
 * Decimate interleaved frames by bandlimited interpolation: output
 * frame j sits at input position j*in_rate/out_rate, and is the
 * kernel-weighted sum of the taps around it.  Identical to a
 * polyphase bank with one phase per distinct fractional offset;
 * for integer ratios (192k->48k, 48k->8k) the offset never changes
 * and the kernel is evaluated once.  Writes into out, which the
 * caller sized for out_frames*channels.
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
		double frac = p - floor(p);
		long centre = (long)floor(p);
		double norm = 0.0;

		if (frac != h_frac) {
			for (size_t n = 0; n < taps; n++) {
				double u = (double)((long)n -
					(long)(taps - 1) / 2) - frac;
				h[n] = aa_kernel(u, half, fc);
				norm += h[n];
			}
			h_frac = frac;
		} else {
			for (size_t n = 0; n < taps; n++)
				norm += h[n];
		}

		for (unsigned c = 0; c < channels; c++) {
			double acc = 0.0;

			for (size_t n = 0; n < taps; n++) {
				long i = centre - (long)(taps - 1) / 2 +
					 (long)n;

				if (i < 0)
					i = 0;	/* clamp to the edge sample */
				if (i >= (long)in_frames)
					i = (long)in_frames - 1;
				acc += h[n] * in[(size_t)i * channels + c];
			}
			out[j * channels + c] = (float)(acc / norm);
		}
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
		size_t i = (size_t)p;
		double t = p - (double)i;
		size_t i0 = i ? i - 1 : 0;
		size_t i2 = (i + 1 < in_frames) ? i + 1 : in_frames - 1;
		size_t i3 = (i + 2 < in_frames) ? i + 2 : in_frames - 1;

		for (unsigned c = 0; c < channels; c++) {
			double x0 = in[i0 * channels + c];
			double x1 = in[i * channels + c];
			double x2 = in[i2 * channels + c];
			double x3 = in[i3 * channels + c];
			double a0 = -0.5 * x0 + 1.5 * x1 - 1.5 * x2 + 0.5 * x3;
			double a1 = x0 - 2.5 * x1 + 2.0 * x2 - 0.5 * x3;
			double a2 = -0.5 * x0 + 0.5 * x2;
			double a3 = x1;
			out[j * channels + c] =
				(float)(((a0 * t + a1) * t + a2) * t + a3);
		}
	}
	*out_p = out;
	return out_frames;
}
