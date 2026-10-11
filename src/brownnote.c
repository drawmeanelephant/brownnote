/*
 * brownnote — middleware between fart-app and the GuitarPedal bench.
 *
 *          fart-app | brownnote --effect fuzz | zcn
 *
 * brownnote adds no DSP of its own.  It spawns the GuitarPedal test
 * bench (Validation/bench — the pedal's actual audio core, built as a
 * host program) as a subprocess, ferries samples through it, and gets
 * out of the way.  The bench is the signal path; brownnote is the
 * plumbing between two programs that were never introduced.
 *
 * Two input modes, auto-detected by peeking at stdin:
 *
 *   raw  — float32 little-endian stereo frames at 48 kHz, the bench's
 *          own wire format, in one end and out the other.  Plumbing
 *          byte-identity is testable with --bench /bin/cat.
 *
 *   wav  — a RIFF/WAVE file (PCM s16le, mono or stereo, any rate
 *          8–192 kHz) on stdin.  brownnote converts to the bench's
 *          format, resamples to 48 kHz, runs the bench, resamples
 *          back, and writes a WAV with the input's parameters back.
 *          Resampling and width conversion are transport plumbing,
 *          not effects; the pedal's code still does everything that
 *          makes a sound a sound.
 *
 * The bench writes its stdout through a block buffer and only flushes
 * when it fills or at exit, so a naive write-everything-then-read loop
 * deadlocks: the bench stops reading stdin while blocked writing
 * stdout, both pipes fill, and everyone waits.  The pump below is a
 * poll() event loop that copies each direction as its pipe becomes
 * ready, which cannot deadlock.
 *
 * Build: see Makefile.  C11 + POSIX, no dependencies beyond libc.
 */

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <math.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#ifdef __APPLE__
#include <mach-o/dyld.h>	/* _NSGetExecutablePath */
#endif

/*
 * Exit codes: 0 success; 2 brownnote usage or format errors; anything
 * else is the bench's own exit status, propagated.  If the bench dies
 * on a signal, 128+signal, the shell convention.
 */
#define EXIT_USAGE 2

/*
 * SIGPIPE policy: ignored.  The two directions are handled explicitly
 * instead, because they mean different things:
 *
 *   - A write to OUR stdout failing with EPIPE means the program
 *     downstream went away (zcn exited, someone piped us into head).
 *     brownnote exits 141, the shell convention, without wedging the
 *     pipeline.
 *
 *   - A write to the BENCH's stdin failing with EPIPE means the bench
 *     itself died — an unknown effect name, say.  brownnote stops
 *     feeding it, drains the bench's stderr (which carries the bench's
 *     own candidate list) and stdout, and exits with the bench's code
 *     so the user sees the real error, not a broken pipe.
 */

static void die(int code, const char *fmt, ...)
{
	va_list ap;

	va_start(ap, fmt);
	vfprintf(stderr, fmt, ap);
	va_end(ap);
	fputc('\n', stderr);
	exit(code);
}

static void usage(FILE *out)
{
	fprintf(out,
"usage: brownnote [--effect NAME]... [--pot EFFECT:POT=VALUE]...\n"
"                 [--mix EFFECT=VAL]... [--bench PATH] [--list]\n"
"\n"
"  --effect NAME   route an effect, in order given (aliases accepted;\n"
"                  exact GuitarPedal names also work)\n"
"  --pot E:P=V     set a pot, passed to the bench verbatim\n"
"  --mix E=V       set a mix 0..120, passed to the bench verbatim\n"
"  --bench PATH    bench binary to run\n"
"  --list          list the effects the bench knows, from the bench\n"
"\n"
"stdin: raw float32 stereo 48 kHz, or a RIFF/WAVE file (auto-detected).\n"
"stdout: same shape as the input, processed by the routed effects.\n");
}

/* ------------------------------------------------------------------ */
/* Effect aliases.                                                     */
/* ------------------------------------------------------------------ */

/*
 * Common words for the pedal's effects, so that the acceptance line
 *
 *     fart-app | brownnote --effect fuzz | zcn
 *
 * works verbatim.  Resolution is case-insensitive and exact on the
 * alias; anything that is not an alias is passed to the bench
 * untouched, and the bench's own matcher (exact, then unique
 * substring, with candidate lists on ambiguity) does the rest.  No
 * alias key collides with a different bench name, so exact bench
 * names always reach the bench as typed.
 */
static const struct alias {
	const char *alias;
	const char *effect;
} aliases[] = {
	{ "fuzz",	"Rat Sketch" },
	{ "distortion",	"Rat Sketch" },
	{ "overdrive",	"Klonlike" },
	{ "boost",	"Boost" },
	{ "wah",	"Auto-Wah" },
	{ "compressor",	"Compressor" },
	{ "delay",	"Tape Echo" },
	{ "echo",	"Tape Echo" },
	{ "reverb",	"Reverb" },
	{ "chorus",	"Flanger" },
	{ "flanger",	"Flanger" },
	{ "phaser",	"Phaser" },
	{ "tremolo",	"Tremolo" },
	{ "vibrato",	"Vibrato" },
	{ "pitch",	"Pitch" },
	{ "eq",		"Parametric EQ" },
	{ "cabinet",	"Cabinet" },
	{ "amp",	"Frenchie Amp" },
	{ "preamp",	"Preamp" },
	{ "tone",	"Tone 1" },
};

static const char *resolve_alias(const char *name)
{
	for (size_t i = 0; i < sizeof(aliases) / sizeof(aliases[0]); i++)
		if (!strcasecmp(aliases[i].alias, name))
			return aliases[i].effect;
	return NULL;
}

/* ------------------------------------------------------------------ */
/* Growable byte buffer.                                               */
/* ------------------------------------------------------------------ */

struct membuf {
	unsigned char *p;
	size_t len, cap;
};

static void mb_reserve(struct membuf *b, size_t extra)
{
	if (b->len + extra <= b->cap)
		return;
	size_t cap = b->cap ? b->cap : 65536;
	while (cap < b->len + extra)
		cap *= 2;
	unsigned char *p = realloc(b->p, cap);
	if (!p)
		die(EXIT_FAILURE, "brownnote: out of memory");
	b->p = p;
	b->cap = cap;
}

static void mb_put(struct membuf *b, const void *src, size_t n)
{
	mb_reserve(b, n);
	memcpy(b->p + b->len, src, n);
	b->len += n;
}

/* ------------------------------------------------------------------ */
/* Bench subprocess.                                                   */
/* ------------------------------------------------------------------ */

static pid_t bench_pid = -1;

/*
 * On SIGINT/SIGTERM, pass it to the bench and leave.  The handler is
 * async-signal-safe in practice: kill, waitpid and _exit only.
 */
static void on_signal(int sig)
{
	if (bench_pid > 0) {
		kill(bench_pid, sig);
		while (waitpid(bench_pid, NULL, 0) < 0 && errno == EINTR)
			;
	}
	_exit(128 + sig);
}

/*
 * Find the bench binary: --bench flag, environment, the vendor clone
 * next to the working directory, or next to this executable.
 */
static const char *find_bench(const char *flag)
{
	static char path[PATH_MAX];

	if (flag)
		return flag;
	const char *env = getenv("BROWNNOTE_BENCH");
	if (env && *env)
		return env;
	if (access("vendor/GuitarPedal/Validation/bench/bench", X_OK) == 0)
		return "vendor/GuitarPedal/Validation/bench/bench";

#ifdef __APPLE__
	uint32_t sz = sizeof(path);
	if (_NSGetExecutablePath(path, &sz) == 0) {
		char *slash = strrchr(path, '/');
		if (slash) {
			snprintf(slash + 1, sizeof(path) - (slash + 1 - path),
				 "../vendor/GuitarPedal/Validation/bench/bench");
			if (access(path, X_OK) == 0)
				return path;
		}
	}
#else
	ssize_t n = readlink("/proc/self/exe", path, sizeof(path) - 1);
	if (n > 0) {
		path[n] = 0;
		char *slash = strrchr(path, '/');
		if (slash) {
			snprintf(slash + 1, sizeof(path) - (slash + 1 - path),
				 "../vendor/GuitarPedal/Validation/bench/bench");
			if (access(path, X_OK) == 0)
				return path;
		}
	}
#endif
	return NULL;
}

/*
 * The pump.  Copies bytes to the bench's stdin and from its stdout
 * and stderr as the pipes become ready, which is the only shape that
 * cannot deadlock against the bench's block-buffered stdout.
 *
 * All three bench pipes are O_NONBLOCK in the parent, and every write
 * is bounded, so no single syscall can ever sit blocked on a full pipe
 * while the other direction waits to be drained — that is the
 * two-party deadlock this whole loop exists to prevent.
 *
 * Sources and sinks are either file descriptors (streaming, raw mode)
 * or memory (WAV mode, where the whole signal is already in hand).
 */
struct pump {
	int in_fd;		/* our stdin, or -1 for memory source */
	int out_fd;		/* our stdout, or -1 for memory sink */
	int bench_in, bench_out, bench_err;

	struct membuf queue;	/* bytes waiting for the bench */
	size_t queue_pos;
	bool in_eof;		/* no more input will be enqueued */

	struct membuf *sink;	/* memory sink, or NULL for out_fd */
};

#define QUEUE_LIMIT (256 * 1024)	/* back off reading stdin past this */

/*
 * Read the bench's stderr and relay it to our fd 2.  If that write
 * fails — the reader of our stderr went away (`2>&1 | head`), or fd 2
 * is closed — stop relaying but keep draining and discard: a bench
 * blocked writing stderr because brownnote stopped listening backs up
 * into a pipeline-wide wedge (it stops reading stdin, the queue fills,
 * poll waits on stdout that never comes), and preventing that wedge is
 * brownnote's job, not the bench's.
 *
 * Returns true when the bench's stderr is finished: EOF, or a read
 * error that will not recover.
 */
static bool drain_err(int from)
{
	bool broken = false;

	for (;;) {
		unsigned char buf[65536];
		ssize_t n = read(from, buf, sizeof(buf));
		if (n < 0) {
			if (errno == EINTR)
				continue;
			if (errno == EAGAIN || errno == EWOULDBLOCK)
				return false;	/* nothing right now */
			return true;
		}
		if (n == 0)
			return true;

		size_t off = 0;
		while (off < (size_t)n) {
			if (broken)
				break;
			ssize_t w = write(2, buf + off, n - off);
			if (w < 0) {
				if (errno == EINTR)
					continue;
				broken = true;	/* discard from here on */
				break;
			}
			off += w;
		}
	}
}

static int pump_run(struct pump *pm, pid_t pid)
{
	bool out_open = true, err_open = true;

	while (out_open || err_open) {
		struct pollfd pfd[4];
		int n = 0;
		int pi_in = -1, pi_our_stdin = -1, pi_out = -1, pi_err = -1;

		/*
		 * Close the bench's stdin once everything is written and
		 * no more input is coming; that is what tells the bench
		 * to finish and flush.  Before poll, not after: with an
		 * empty queue and no input fd (a one-frame 192 kHz WAV
		 * resamples to zero bench frames, so the queue is empty
		 * and in_fd is -1) no poll event could ever fire the
		 * check, and bench and pump would wait on each other
		 * forever.
		 */
		if (pm->bench_in >= 0 && pm->in_eof &&
		    pm->queue_pos == pm->queue.len) {
			close(pm->bench_in);
			pm->bench_in = -1;
		}

		if (out_open) {
			pfd[n].fd = pm->bench_out;
			pfd[n].events = POLLIN;
			pi_out = n++;
		}
		if (err_open) {
			pfd[n].fd = pm->bench_err;
			pfd[n].events = POLLIN;
			pi_err = n++;
		}
		if (pm->bench_in >= 0 && pm->queue_pos < pm->queue.len) {
			pfd[n].fd = pm->bench_in;
			pfd[n].events = POLLOUT;
			pi_in = n++;
		} else if (pm->in_fd >= 0 && !pm->in_eof &&
			   pm->queue.len - pm->queue_pos < QUEUE_LIMIT) {
			pfd[n].fd = pm->in_fd;
			pfd[n].events = POLLIN;
			pi_our_stdin = n++;
		}

		int r = poll(pfd, n, -1);
		if (r < 0) {
			if (errno == EINTR)
				continue;
			die(EXIT_FAILURE, "brownnote: poll: %s", strerror(errno));
		}

		/*
		 * Every polled fd is handled on POLLERR/POLLNVAL too, by
		 * running its natural operation and letting the result
		 * decide.  macOS poll() reports POLLNVAL for character
		 * devices like /dev/null — immediately, on every call —
		 * so an event the loop does not act on comes right back
		 * and the pump busy-spins at 100% CPU forever.
		 */
		if (pi_out >= 0 &&
		    (pfd[pi_out].revents &
		     (POLLIN | POLLHUP | POLLERR | POLLNVAL))) {
			unsigned char buf[65536];
			ssize_t rd;
			do {
				rd = read(pm->bench_out, buf, sizeof(buf));
			} while (rd < 0 && errno == EINTR);
			if (rd < 0 && (errno == EAGAIN ||
				       errno == EWOULDBLOCK))
				rd = 0;
			if (rd < 0)
				die(EXIT_FAILURE, "brownnote: read bench: %s",
				    strerror(errno));
			if (rd == 0) {
				out_open = false;
			} else if (pm->sink) {
				mb_put(pm->sink, buf, rd);
			} else {
				size_t off = 0;
				while (off < (size_t)rd) {
					ssize_t w = write(pm->out_fd, buf + off,
							  rd - off);
					if (w < 0) {
						if (errno == EINTR)
							continue;
						/* downstream is gone */
						die(128 + SIGPIPE,
						    "brownnote: write stdout: %s",
						    strerror(errno));
					}
					off += w;
				}
			}
		}
		if (pi_err >= 0 &&
		    (pfd[pi_err].revents &
		     (POLLIN | POLLHUP | POLLERR | POLLNVAL))) {
			if (drain_err(pm->bench_err))
				err_open = false;
		}
		if (pi_in >= 0 &&
		    (pfd[pi_in].revents & (POLLOUT | POLLERR | POLLNVAL))) {
			size_t have = pm->queue.len - pm->queue_pos;
			if (have > 65536)
				have = 65536;	/* bounded, never sit on a full pipe */
			ssize_t w = write(pm->bench_in,
					  pm->queue.p + pm->queue_pos, have);
			if (w < 0) {
				if (errno == EINTR || errno == EAGAIN ||
				    errno == EWOULDBLOCK) {
					/* nothing written; fine, poll will retry */
				} else if (errno == EPIPE) {
					/* the bench died; drain and report */
					close(pm->bench_in);
					pm->bench_in = -1;
				} else {
					die(EXIT_FAILURE,
					    "brownnote: write bench: %s",
					    strerror(errno));
				}
			} else {
				pm->queue_pos += w;
			}
		}
		if (pi_our_stdin >= 0 &&
		    (pfd[pi_our_stdin].revents &
		     (POLLIN | POLLHUP | POLLERR | POLLNVAL))) {
			unsigned char buf[65536];
			bool unpollable =
				(pfd[pi_our_stdin].revents & POLLNVAL) != 0;
			ssize_t rd;
			do {
				rd = read(pm->in_fd, buf, sizeof(buf));
			} while (rd < 0 && errno == EINTR);
			if (rd < 0 && (errno == EAGAIN ||
				       errno == EWOULDBLOCK)) {
				/*
				 * Nothing there after all.  On a fd poll
				 * can describe, that is an ordinary
				 * spurious wakeup: wait for the next
				 * one.  On an unpollable fd (macOS
				 * character devices), poll returns at
				 * once every time, so an empty read is
				 * how EOF looks — anything else spins
				 * forever between poll and read.
				 */
				if (unpollable)
					rd = 0;
				else
					continue;
			}
			if (rd < 0)
				die(EXIT_FAILURE, "brownnote: read stdin: %s",
				    strerror(errno));
			if (rd == 0) {
				pm->in_eof = true;
				close(pm->in_fd);
				pm->in_fd = -1;
			} else {
				mb_put(&pm->queue, buf, rd);
			}
		}
	}

	/* Drain the bench's stderr summary that lands after stdout EOF. */
	for (;;) {
		struct pollfd p = { .fd = pm->bench_err, .events = POLLIN };
		if (pm->bench_err < 0 || poll(&p, 1, 250) != 1)
			break;
		if (drain_err(pm->bench_err))
			break;
	}

	int status = 0;
	while (waitpid(pid, &status, 0) < 0 && errno == EINTR)
		;
	if (WIFEXITED(status))
		return WEXITSTATUS(status);
	if (WIFSIGNALED(status))
		return 128 + WTERMSIG(status);
	return EXIT_FAILURE;
}

/*
 * Spawn the bench with argv (argv[0] = path) and pump audio through
 * it.  Returns the bench's exit status.
 */
static int run_bench(char **argv, struct pump *pm)
{
	int in_pipe[2], out_pipe[2], err_pipe[2];

	if (pipe(in_pipe) || pipe(out_pipe) || pipe(err_pipe))
		die(EXIT_FAILURE, "brownnote: pipe: %s", strerror(errno));

	pid_t pid = fork();
	if (pid < 0)
		die(EXIT_FAILURE, "brownnote: fork: %s", strerror(errno));

	if (pid == 0) {
		if (dup2(in_pipe[0], 0) < 0 || dup2(out_pipe[1], 1) < 0 ||
		    dup2(err_pipe[1], 2) < 0)
			_exit(127);
		close(in_pipe[0]); close(in_pipe[1]);
		close(out_pipe[0]); close(out_pipe[1]);
		close(err_pipe[0]); close(err_pipe[1]);
		execv(argv[0], argv);
		_exit(127);
	}

	close(in_pipe[0]);
	close(out_pipe[1]);
	close(err_pipe[1]);

	/* Non-blocking: poll() decides when each direction can move. */
	fcntl(in_pipe[1], F_SETFL, O_NONBLOCK);
	fcntl(out_pipe[0], F_SETFL, O_NONBLOCK);
	fcntl(err_pipe[0], F_SETFL, O_NONBLOCK);

	pm->bench_in = in_pipe[1];
	pm->bench_out = out_pipe[0];
	pm->bench_err = err_pipe[0];
	pm->in_eof = (pm->in_fd < 0);	/* memory source: all queued */

	bench_pid = pid;
	return pump_run(pm, pid);
}

/* ------------------------------------------------------------------ */
/* WAV: the file format fart-app writes and zcn checks.               */
/* ------------------------------------------------------------------ */

struct wav_info {
	unsigned channels;
	unsigned rate;
	size_t data_off, data_len;
};

static uint32_t rd32(const unsigned char *p)
{
	return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
	       ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static uint16_t rd16(const unsigned char *p)
{
	return (uint16_t)(p[0] | (p[1] << 8));
}

static void wr32(unsigned char *p, uint32_t v)
{
	p[0] = v; p[1] = v >> 8; p[2] = v >> 16; p[3] = v >> 24;
}

static void wr16(unsigned char *p, uint16_t v)
{
	p[0] = v; p[1] = v >> 8;
}

/*
 * Parse RIFF/WAVE: PCM s16le, mono or stereo, any rate 8-192 kHz.
 * Chunks are walked properly, so LIST and friends pass by; a chunk
 * whose size runs past the end of the file is clamped.
 */
static bool parse_wav(const unsigned char *p, size_t len, struct wav_info *w)
{
	if (len < 44 || memcmp(p, "RIFF", 4) || memcmp(p + 8, "WAVE", 4)) {
		fprintf(stderr, "brownnote: not a RIFF/WAVE file\n");
		return false;
	}
	size_t pos = 12;
	bool have_fmt = false;
	uint16_t format = 0, bits = 0;

	while (pos + 8 <= len) {
		uint32_t csz = rd32(p + pos + 4);

		if (pos + 8 + csz > len)
			csz = (uint32_t)(len - pos - 8);
		const unsigned char *body = p + pos + 8;

		if (!memcmp(p + pos, "fmt ", 4) && csz >= 16) {
			format = rd16(body);
			w->channels = rd16(body + 2);
			w->rate = rd32(body + 4);
			bits = rd16(body + 14);
			have_fmt = true;
		} else if (!memcmp(p + pos, "data", 4)) {
			w->data_off = pos + 8;
			w->data_len = csz ? csz : len - w->data_off;
		}
		pos += 8 + csz + (csz & 1);	/* chunks are word-aligned */
	}

	if (!have_fmt || !w->data_len) {
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
static float s16_to_f(int16_t v) { return (float)v / 32768.0f; }
static int16_t f_to_s16(float v)
{
	if (v >= 1.0f) return 32767;
	if (v <= -1.0f) return -32767;
	return (int16_t)lroundf(v * 32767.0f);
}

/*
 * Catmull-Rom cubic resampling, per channel, over interleaved frames.
 * Transport plumbing to meet the bench's 48 kHz — not an effect; it
 * is the same interpolation any file converter does.
 */
static size_t resample_frames(const float *in, size_t in_frames,
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
	if (!out)
		die(EXIT_FAILURE, "brownnote: out of memory");

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

/* ------------------------------------------------------------------ */
/* main.                                                               */
/* ------------------------------------------------------------------ */

#define MAX_ARGS 256

/*
 * Read from stdin, waiting for data to actually arrive.  A
 * poll()-driven parent legally hands brownnote a non-blocking stdin,
 * where read() fails with EAGAIN merely because the bytes have not
 * been written yet — that is not EOF, and treating it as one
 * misdetects the mode (a late WAV pumped through as raw float32) or
 * truncates the WAV body.  On EAGAIN, wait for readability like a
 * blocking fd would; EINTR retries; anything else is a real error.
 */
static ssize_t read_input(void *buf, size_t n)
{
	for (;;) {
		ssize_t r = read(0, buf, n);
		if (r >= 0)
			return r;
		if (errno == EINTR)
			continue;
		if (errno == EAGAIN || errno == EWOULDBLOCK) {
			struct pollfd p = { .fd = 0, .events = POLLIN };
			poll(&p, 1, -1);
			continue;
		}
		return -1;
	}
}

/*
 * Worst case bench argv: path + 2 slots for each of routes/pots/mixes
 * (flag + value), each individually capped at MAX_ARGS, plus NULL.
 * Sizing this wrong is how a shell script of 768 flags becomes a heap
 * abort, so it is spelled out and asserted below.
 */
#define BARGV_MAX (1 + 6 * MAX_ARGS + 8)

int main(int argc, char **argv)
{
	const char *bench_flag = NULL;
	const char *routes[MAX_ARGS];
	const char *pots[MAX_ARGS], *mixes[MAX_ARGS];
	int n_routes = 0, n_pots = 0, n_mixes = 0;
	bool want_list = false;

	for (int i = 1; i < argc; i++) {
		const char *a = argv[i];

		if (!strcmp(a, "--help") || !strcmp(a, "-h")) {
			usage(stdout);
			return 0;
		}
		if (!strcmp(a, "--list")) {
			want_list = true;
			continue;
		}
		if (!strcmp(a, "--bench")) {
			if (++i >= argc)
				die(EXIT_USAGE, "brownnote: --bench wants a path");
			bench_flag = argv[i];
			continue;
		}
		if (!strcmp(a, "--effect") || !strcmp(a, "--route")) {
			if (++i >= argc)
				die(EXIT_USAGE, "brownnote: %s wants a name", a);
			if (n_routes >= MAX_ARGS)
				die(EXIT_USAGE, "brownnote: too many %s", a);
			/*
			 * Alias first (case-insensitive, exact); anything
			 * else goes to the bench's own matcher verbatim.
			 */
			routes[n_routes++] = resolve_alias(argv[i]) ?: argv[i];
			continue;
		}
		if (!strcmp(a, "--pot")) {
			if (++i >= argc)
				die(EXIT_USAGE, "brownnote: --pot wants EFFECT:POT=VALUE");
			if (n_pots >= MAX_ARGS)
				die(EXIT_USAGE, "brownnote: too many --pot");
			pots[n_pots++] = argv[i];
			continue;
		}
		if (!strcmp(a, "--mix")) {
			if (++i >= argc)
				die(EXIT_USAGE, "brownnote: --mix wants EFFECT=VALUE");
			if (n_mixes >= MAX_ARGS)
				die(EXIT_USAGE, "brownnote: too many --mix");
			mixes[n_mixes++] = argv[i];
			continue;
		}
		die(EXIT_USAGE, "brownnote: unknown option %s "
			"(try --help)", a);
	}

	const char *bench = find_bench(bench_flag);
	if (!bench)
		die(EXIT_USAGE, "brownnote: no bench binary found "
			"(--bench PATH, BROWNNOTE_BENCH, or "
			"vendor/GuitarPedal/Validation/bench/bench)");

	signal(SIGPIPE, SIG_IGN);

	struct sigaction sa;
	memset(&sa, 0, sizeof(sa));
	sa.sa_handler = on_signal;
	sigemptyset(&sa.sa_mask);
	sigaction(SIGINT, &sa, NULL);
	sigaction(SIGTERM, &sa, NULL);

	/*
	 * --list: let the bench speak for itself, stdio inherited.
	 */
	if (want_list) {
		pid_t pid = fork();
		if (pid == 0) {
			execl(bench, bench, "--list", (char *)NULL);
			_exit(127);
		}
		int status;
		while (waitpid(pid, &status, 0) < 0 && errno == EINTR)
			;
		return WIFEXITED(status) ? WEXITSTATUS(status) : 127;
	}

	/* Peek at stdin to pick a mode. */
	unsigned char head[4];
	size_t got = 0;
	while (got < 4) {
		ssize_t n = read_input(head + got, 4 - got);
		if (n < 0)
			die(EXIT_FAILURE, "brownnote: read stdin: %s",
			    strerror(errno));
		if (n == 0)
			break;
		got += n;
	}
	bool wav_mode = (got == 4 && !memcmp(head, "RIFF", 4));

	/* Assemble the bench command line. */
	char *bargv[BARGV_MAX];
	int bn = 0;
	bargv[bn++] = (char *)bench;
	for (int i = 0; i < n_routes; i++) {
		bargv[bn++] = (char *)"--route";
		bargv[bn++] = (char *)routes[i];
	}
	for (int i = 0; i < n_pots; i++) {
		bargv[bn++] = (char *)"--pot";
		bargv[bn++] = (char *)pots[i];
	}
	for (int i = 0; i < n_mixes; i++) {
		bargv[bn++] = (char *)"--mix";
		bargv[bn++] = (char *)mixes[i];
	}
	/*
	 * The three caps above each allow MAX_ARGS, so bn can reach
	 * 1 + 6*MAX_ARGS; BARGV_MAX is sized for exactly that, and this
	 * assert keeps a future edit from quietly unlinking the two.
	 */
	if (bn >= BARGV_MAX)
		die(EXIT_FAILURE, "brownnote: bench argv overflow (bug)");
	bargv[bn] = NULL;

	struct pump pm;
	memset(&pm, 0, sizeof(pm));
	pm.in_fd = -1;
	pm.out_fd = -1;
	int bench_status;

	if (!wav_mode) {
		/*
		 * Raw mode: stream stdin -> bench -> stdout.  The four
		 * peeked bytes lead the queue.
		 */
		if (got)
			mb_put(&pm.queue, head, got);
		pm.in_fd = 0;
		pm.out_fd = 1;
		bench_status = run_bench(bargv, &pm);
		if (bench_status)
			return bench_status;
		return 0;
	}

	/*
	 * WAV mode: the rest of stdin is the file body.  Convert to the
	 * bench's float32 stereo 48 kHz, process, convert back to the
	 * input's shape.
	 */
	struct membuf file = { 0 };
	mb_put(&file, head, 4);
	for (;;) {
		unsigned char buf[65536];
		ssize_t n = read_input(buf, sizeof(buf));
		if (n < 0)
			die(EXIT_FAILURE, "brownnote: read stdin: %s",
			    strerror(errno));
		if (n == 0)
			break;
		mb_put(&file, buf, n);
	}

	/*
	 * parse_wav only writes data_off/data_len when it actually walks
	 * a data chunk, and its missing-data guard reads data_len either
	 * way — so w must start at zero, or a WAV with a fmt but no data
	 * chunk is judged on stack garbage: wild-pointer reads, giant
	 * mallocs, nondeterministic verdicts.
	 */
	struct wav_info w;
	memset(&w, 0, sizeof(w));
	if (!parse_wav(file.p, file.len, &w))
		return EXIT_USAGE;

	size_t in_frames = w.data_len / (2 * w.channels);
	float *frames = malloc((in_frames ? in_frames : 1) *
			       w.channels * sizeof(float));
	if (!frames)
		die(EXIT_FAILURE, "brownnote: out of memory");
	for (size_t i = 0; i < in_frames * w.channels; i++)
		frames[i] = s16_to_f(
			(int16_t)rd16(file.p + w.data_off + 2 * i));

	/* to the bench's 48 kHz ... */
	float *res = NULL;
	size_t frames48 = resample_frames(frames, in_frames, w.channels,
					  w.rate, 48000, &res);
	const float *at48 = res ? res : frames;

	/* ... and to stereo */
	size_t st_frames = frames48;
	float *stereo = malloc((st_frames ? st_frames : 1) * 2 * sizeof(float));
	if (!stereo)
		die(EXIT_FAILURE, "brownnote: out of memory");
	if (w.channels == 1) {
		for (size_t i = 0; i < st_frames; i++) {
			stereo[2 * i] = at48[i];
			stereo[2 * i + 1] = at48[i];
		}
	} else {
		memcpy(stereo, at48, st_frames * 2 * sizeof(float));
	}
	free(res);
	free(frames);

	/* queue it all for the bench and collect the result */
	struct membuf sink = { 0 };
	mb_reserve(&pm.queue, st_frames * 2 * sizeof(float));
	pm.queue.len = st_frames * 2 * sizeof(float);
	if (pm.queue.len)	/* empty queue: pm.queue.p is NULL */
		memcpy(pm.queue.p, stereo, pm.queue.len);
	pm.sink = &sink;
	free(stereo);

	bench_status = run_bench(bargv, &pm);
	if (bench_status)
		return bench_status;

	if (sink.len % (2 * sizeof(float)))
		die(EXIT_FAILURE, "brownnote: bench returned a partial frame");

	size_t out48 = sink.len / (2 * sizeof(float));
	const float *outp = (const float *)sink.p;

	/* back to the input rate ... */
	float *back = NULL;
	size_t out_frames = resample_frames(outp, out48, 2, 48000,
					    w.rate, &back);
	const float *at_in = back ? back : outp;

	/*
	 * ... and the input's channel count.  Mono output takes the
	 * left channel: the pedal's stereo effects can differ per
	 * side, and choosing one channel is the documented, honest
	 * collapse (documented here, and in the README).
	 */
	int16_t *pcm = malloc((out_frames ? out_frames : 1) *
			      w.channels * sizeof(int16_t));
	if (!pcm)
		die(EXIT_FAILURE, "brownnote: out of memory");
	if (w.channels == 1) {
		for (size_t i = 0; i < out_frames; i++)
			pcm[i] = f_to_s16(at_in[2 * i]);
	} else {
		for (size_t i = 0; i < out_frames * 2; i++)
			pcm[i] = f_to_s16(at_in[i]);
	}

	/* canonical 44-byte header, mirroring the input's parameters */
	unsigned char hdr[44];
	uint32_t data_bytes = (uint32_t)(out_frames * w.channels * 2);
	memcpy(hdr, "RIFF", 4);
	wr32(hdr + 4, 36 + data_bytes);
	memcpy(hdr + 8, "WAVE", 4);
	memcpy(hdr + 12, "fmt ", 4);
	wr32(hdr + 16, 16);
	wr16(hdr + 20, 1);
	wr16(hdr + 22, (uint16_t)w.channels);
	wr32(hdr + 24, w.rate);
	wr32(hdr + 28, w.rate * w.channels * 2);
	wr16(hdr + 32, (uint16_t)(w.channels * 2));
	wr16(hdr + 34, 16);
	memcpy(hdr + 36, "data", 4);
	wr32(hdr + 40, data_bytes);

	if (fwrite(hdr, 1, 44, stdout) != 44 ||
	    fwrite(pcm, 2, out_frames * w.channels, stdout) !=
		    out_frames * w.channels)
		die(EXIT_FAILURE, "brownnote: short write on stdout");
	if (fflush(stdout))
		die(EXIT_FAILURE, "brownnote: %s", strerror(errno));

	return 0;
}
