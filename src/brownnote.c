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
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#include "wav.h"

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
 * On SIGINT/SIGTERM, pass it to the bench and leave with the shell
 * convention.  The bench gets a couple of seconds to act on the
 * signal — a shell-script bench defers SIGINT while it waits on its
 * own foreground children, so a plain blocking waitpid would hang for
 * as long as those run — then SIGKILL, which nothing defers.  The
 * handler is async-signal-safe in practice: kill, waitpid, nanosleep
 * and _exit only.
 */
static void on_signal(int sig)
{
	if (bench_pid > 0) {
		bool gone = false;

		kill(bench_pid, sig);
		for (int i = 0; i < 40 && !gone; i++) {	/* ~2 s of grace */
			pid_t r = waitpid(bench_pid, NULL, WNOHANG);
			if (r == bench_pid || (r < 0 && errno == ECHILD))
				gone = true;
			else {
				struct timespec tick = { .tv_nsec = 50000000 };
				nanosleep(&tick, NULL);
			}
		}
		if (!gone) {
			kill(bench_pid, SIGKILL);
			while (waitpid(bench_pid, NULL, 0) < 0 &&
			       errno == EINTR)
				;
		}
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
 * or transforms: WAV mode converts stdin bytes to bench-format floats
 * on the way in (xf_in, still bounded by the queue's backpressure)
 * and bench output back to the input's shape on the way out (xf_out).
 */
struct pump {
	int in_fd;		/* our stdin, or -1 for memory source */
	int out_fd;		/* our stdout, or -1 for memory sink */
	int bench_in, bench_out, bench_err;

	struct membuf queue;	/* bytes waiting for the bench */
	size_t queue_pos;
	bool in_eof;		/* no more input will be enqueued */

	struct membuf *sink;	/* memory sink, or NULL for out_fd */

	/* input transform: stdin bytes -> queue bytes (WAV mode) */
	void (*xf_in)(struct pump *pm, const unsigned char *src,
		      size_t n, bool eof);
	void *xfi_ctx;
	/* output transform: bench bytes -> our stdout (WAV mode) */
	void (*xf_out)(struct pump *pm, const unsigned char *src,
		       size_t n, bool eof);
	void *xfo_ctx;
};

#define QUEUE_LIMIT (256 * 1024)	/* back off reading stdin past this */

/*
 * Write all of buf to fd.  EPIPE means the program downstream went
 * away — exit 141, the shell convention.  Anything else (ENOSPC,
 * EFBIG, EIO) is our own write failing and must not masquerade as
 * SIGPIPE.
 */
static void write_fd(int fd, const void *buf, size_t n)
{
	const unsigned char *p = buf;

	while (n) {
		ssize_t w = write(fd, p, n);

		if (w < 0) {
			if (errno == EINTR)
				continue;
			die(errno == EPIPE ? 128 + SIGPIPE : EXIT_FAILURE,
			    "brownnote: write stdout: %s", strerror(errno));
		}
		p += w;
		n -= w;
	}
}

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
		 * The queue only ever needs the bytes the bench has not
		 * eaten yet.  Drop what it already consumed, so WAV
		 * mode's streaming input keeps queue memory bounded by
		 * the backlog instead of the file size.
		 */
		if (pm->queue_pos == pm->queue.len) {
			pm->queue_pos = pm->queue.len = 0;
		} else if (pm->queue_pos > 262144) {
			pm->queue.len -= pm->queue_pos;
			memmove(pm->queue.p, pm->queue.p + pm->queue_pos,
				pm->queue.len);
			pm->queue_pos = 0;
		}

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
				if (pm->xf_out)
					pm->xf_out(pm, NULL, 0, true);
			} else if (pm->xf_out) {
				pm->xf_out(pm, buf, rd, false);
			} else if (pm->sink) {
				mb_put(pm->sink, buf, rd);
			} else {
				write_fd(pm->out_fd, buf, rd);
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
				if (pm->xf_in)
					pm->xf_in(pm, NULL, 0, true);
				pm->in_eof = true;
				close(pm->in_fd);
				pm->in_fd = -1;
			} else if (pm->xf_in) {
				pm->xf_in(pm, buf, rd, false);
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
		    dup2(err_pipe[1], 2) < 0) {
			/*
			 * Nearly unreachable (needs fds 0..2 closed at
			 * fork); say what happened anyway rather than a
			 * silent 127 — if fd 2 was the one that failed,
			 * this reaches the user's real stderr directly.
			 */
			fprintf(stderr, "brownnote: dup2: %s\n",
				strerror(errno));
			_exit(127);
		}
		close(in_pipe[0]); close(in_pipe[1]);
		close(out_pipe[0]); close(out_pipe[1]);
		close(err_pipe[0]); close(err_pipe[1]);
		execv(argv[0], argv);
		/*
		 * fd 2 is the err pipe here, so this reaches the user
		 * through the pump's stderr relay: a typo'd --bench or
		 * BROWNNOTE_BENCH used to die as a silent 127.
		 */
		fprintf(stderr, "brownnote: cannot exec %s: %s\n",
			argv[0], strerror(errno));
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
	/* memory source: all queued.  A transform may already have set
	 * in_eof (an empty WAV's data budget spent during the peek). */
	if (pm->in_fd < 0)
		pm->in_eof = true;

	bench_pid = pid;
	return pump_run(pm, pid);
}

/* ------------------------------------------------------------------ */
/* WAV mode: streaming input and output transforms.                    */
/* ------------------------------------------------------------------ */

/*
 * WAV mode used to buffer the whole input file, the whole float32
 * intermediate, the whole bench output and the whole PCM result —
 * a 4 GiB input needed ~13 GB.  It now streams in both directions:
 * stdin bytes walk the RIFF chunks to the data chunk, convert to
 * float and resample into the pump's queue as they arrive, and the
 * bench's output converts back and resamples to the input's shape
 * as it is produced.  The output header goes out first with the
 * streaming-WAV placeholder sizes (0xFFFFFFFF), then — when stdout
 * is a regular file — the real sizes are backfilled at the end, so
 * a seekable result is byte-for-byte the batch path's.
 */

/* The bench's end: float32 stereo -> resample -> collapse -> s16. */
struct wavout {
	struct wav_resample *rs;	/* 48000 -> input rate, stereo */
	unsigned out_channels;		/* the input's channel count */
	unsigned out_rate;
	uint64_t pcm_bytes;
	unsigned char fstash[8];	/* partial bench frame */
	unsigned fstash_len;
	unsigned char pbuf[8192];
	size_t pbuf_len;
	bool seekable;
	bool wrote_header;
	bool finished;
};

/*
 * The 44-byte header goes out ahead of the first PCM byte, with the
 * streaming-WAV placeholder sizes (0xFFFFFFFF); when stdout is a
 * regular file the real sizes are backfilled at the end, so a
 * seekable result is byte-for-byte the batch path's.  Lazy so a
 * bench that fails without emitting anything leaves stdout empty,
 * like the batch path.
 */
static void wavout_header(struct wavout *o)
{
	unsigned char hdr[44];

	if (o->wrote_header)
		return;
	memcpy(hdr, "RIFF", 4);
	wav_wr32(hdr + 4, 0xFFFFFFFF);
	memcpy(hdr + 8, "WAVE", 4);
	memcpy(hdr + 12, "fmt ", 4);
	wav_wr32(hdr + 16, 16);
	wav_wr16(hdr + 20, 1);
	wav_wr16(hdr + 22, (uint16_t)o->out_channels);
	wav_wr32(hdr + 24, o->out_rate);
	wav_wr32(hdr + 28, o->out_rate * o->out_channels * 2);
	wav_wr16(hdr + 32, (uint16_t)(o->out_channels * 2));
	wav_wr16(hdr + 34, 16);
	memcpy(hdr + 36, "data", 4);
	wav_wr32(hdr + 40, 0xFFFFFFFF);
	write_fd(1, hdr, 44);
	o->wrote_header = true;
}

static void wavout_pcm(struct wavout *o)
{
	/*
	 * RIFF's size fields are 32-bit.  The batch path checked the
	 * total before writing; streaming counts as it goes and fails
	 * the moment the limit is crossed.
	 */
	if (o->pcm_bytes + o->pbuf_len > (uint64_t)UINT32_MAX - 36)
		die(EXIT_USAGE, "brownnote: output passed the WAV "
		    "format's 4 GiB limit");
	wavout_header(o);
	write_fd(1, o->pbuf, o->pbuf_len);
	o->pcm_bytes += o->pbuf_len;
	o->pbuf_len = 0;
}

static void wavout_emit(void *ctx, const float *f, size_t n)
{
	struct wavout *o = ctx;

	for (size_t i = 0; i < n; i++) {
		if (o->out_channels == 1) {
			/* mono collapse takes the left channel */
			wav_wr16(o->pbuf + o->pbuf_len,
				 (uint16_t)f_to_s16(f[2 * i]));
			o->pbuf_len += 2;
		} else {
			wav_wr16(o->pbuf + o->pbuf_len,
				 (uint16_t)f_to_s16(f[2 * i]));
			wav_wr16(o->pbuf + o->pbuf_len + 2,
				 (uint16_t)f_to_s16(f[2 * i + 1]));
			o->pbuf_len += 4;
		}
		if (o->pbuf_len == sizeof(o->pbuf))
			wavout_pcm(o);
	}
}

/*
 * Feed bench output bytes into the resampler.  The tail (the
 * resampler's end-clamped frames, the partial-frame check and the
 * header backfill) is finalized by wavout_finish, called from main
 * only when the bench succeeded — like the batch path, a failed
 * bench produces no output.
 */
static void wavout_feed(struct pump *pm, const unsigned char *src,
			size_t n, bool eof)
{
	struct wavout *o = pm->xfo_ctx;
	float fb[512];

	if (!o) {
		if (n)
			die(EXIT_FAILURE, "brownnote: bench produced "
			    "output before the WAV header parsed");
		return;
	}
	if (eof)
		return;

	/* complete a split frame first */
	while (n && o->fstash_len < 8)
		o->fstash[o->fstash_len++] = *src++, n--;
	if (o->fstash_len == 8) {
		memcpy(fb, o->fstash, 8);
		if (!wav_resample_push(o->rs, fb, 1))
			die(EXIT_FAILURE, "brownnote: out of memory");
		o->fstash_len = 0;
	}
	while (n >= 8) {
		size_t frames = n / 8;

		if (frames > 256)
			frames = 256;
		memcpy(fb, src, frames * 8);
		if (!wav_resample_push(o->rs, fb, frames))
			die(EXIT_FAILURE, "brownnote: out of memory");
		src += frames * 8;
		n -= frames * 8;
	}
	while (n)
		o->fstash[o->fstash_len++] = *src++, n--;
}

/*
 * The bench succeeded and its stdout closed: emit the resampler's
 * end-clamped tail, the last PCM, and — on a regular file — the
 * real header sizes.  Called from main after run_bench returns 0.
 */
static void wavout_finish(struct pump *pm)
{
	struct wavout *o = pm->xfo_ctx;

	if (!o || o->finished)
		return;
	o->finished = true;
	if (o->fstash_len)
		die(EXIT_FAILURE,
		    "brownnote: bench returned a partial frame");
	wav_resample_finish(o->rs);
	wavout_pcm(o);
	if (!o->wrote_header)
		wavout_header(o);	/* a silent bench: header alone */
	if (o->seekable) {
		unsigned char sz[4];

		wav_wr32(sz, 36 + (uint32_t)o->pcm_bytes);
		if (pwrite(1, sz, 4, 4) != 4)
			die(EXIT_FAILURE, "brownnote: write stdout: %s",
			    strerror(errno));
		wav_wr32(sz, (uint32_t)o->pcm_bytes);
		if (pwrite(1, sz, 4, 40) != 4)
			die(EXIT_FAILURE, "brownnote: write stdout: %s",
			    strerror(errno));
	}
}

/*
 * Our stdin's end: RIFF bytes -> wav_info -> s16 -> float ->
 * resample to 48 kHz -> mono/stereo duplicate -> the pump's queue.
 */
struct wavsrc {
	struct pump *pm;
	struct wav_parse ps;
	bool parsed;
	uint64_t data_left;	/* audio bytes still expected */
	uint64_t data_bytes;
	unsigned frame_bytes;	/* 2 * channels */
	unsigned char fpart[4];	/* partial input frame */
	unsigned fpart_len;
	float fbuf[2048];
	struct wav_resample *rs;
	struct wavout *xfo;
	bool defer_align;	/* pipe input: check alignment at end */
	bool finished;
};

/* Resampled input-rate frames -> duplicated stereo floats -> queue. */
static void wavsrc_emit(void *ctx, const float *f, size_t n)
{
	struct wavsrc *s = ctx;
	struct membuf *q = &s->pm->queue;

	mb_reserve(q, n * 2 * sizeof(float));
	for (size_t i = 0; i < n; i++) {
		if (s->ps.w.channels == 1) {
			memcpy(q->p + q->len, f + i, sizeof(float));
			memcpy(q->p + q->len + sizeof(float), f + i,
			       sizeof(float));
		} else {
			memcpy(q->p + q->len, f + 2 * i, 2 * sizeof(float));
		}
		q->len += 2 * sizeof(float);
	}
}

/*
 * The data chunk's header just parsed: validate the format fields
 * in the batch parser's order, set the audio byte budget, and stand
 * up both resamplers plus the output's placeholder header.
 */
static void wavsrc_begin(struct wavsrc *s)
{
	struct wav_parse *ps = &s->ps;
	uint64_t avail;
	struct stat st;

	if (!ps->have_fmt)
		die(EXIT_USAGE, "brownnote: WAV is missing fmt or data");
	if (ps->format != 1 || ps->bits != 16)
		die(EXIT_USAGE, "brownnote: WAV must be PCM s16le "
		    "(got format %u, %u bits)", ps->format, ps->bits);
	if (ps->w.channels < 1 || ps->w.channels > 2)
		die(EXIT_USAGE, "brownnote: WAV must be mono or stereo "
		    "(got %u channels)", ps->w.channels);
	if (ps->w.rate < 8000 || ps->w.rate > 192000)
		die(EXIT_USAGE, "brownnote: WAV rate %u out of range "
		    "(8000..192000)", ps->w.rate);

	s->frame_bytes = 2 * ps->w.channels;
	s->data_left = ps->data_decl;
	/*
	 * A regular-file stdin tells us its size, so the declared data
	 * size clamps to EOF exactly like parse_wav and the
	 * frame-alignment check can reject up front.  For a pipe the
	 * effective length is only known at the data chunk's end.
	 */
	if (fstat(0, &st) == 0 && S_ISREG(st.st_mode) &&
	    (uint64_t)st.st_size >= ps->data_off) {
		avail = (uint64_t)st.st_size - ps->data_off;
		if (s->data_left > avail)
			s->data_left = avail;
		if (s->data_left % s->frame_bytes)
			die(EXIT_USAGE, "brownnote: WAV data length "
			    "not frame-aligned");
	} else {
		s->defer_align = true;
	}

	s->rs = wav_resample_new(ps->w.channels, ps->w.rate, 48000,
				 wavsrc_emit, s);
	s->xfo = calloc(1, sizeof(*s->xfo));
	if (!s->rs || !s->xfo)
		die(EXIT_FAILURE, "brownnote: out of memory");
	s->xfo->rs = wav_resample_new(2, 48000, ps->w.rate,
				    wavout_emit, s->xfo);
	if (!s->xfo->rs)
		die(EXIT_FAILURE, "brownnote: out of memory");
	s->xfo->out_channels = ps->w.channels;
	s->xfo->out_rate = ps->w.rate;
	s->xfo->seekable = fstat(1, &st) == 0 && S_ISREG(st.st_mode);
	s->pm->xfo_ctx = s->xfo;
	s->parsed = true;
}

/*
 * The audio budget is spent or stdin ended: the resampler tail goes
 * to the queue, and anything still coming on stdin is metadata.
 */
static void wavsrc_done(struct wavsrc *s)
{
	if (s->finished)
		return;
	s->finished = true;
	if (s->defer_align && s->data_bytes % s->frame_bytes)
		die(EXIT_USAGE,
		    "brownnote: WAV data length not frame-aligned");
	if (s->rs)
		wav_resample_finish(s->rs);
	s->pm->in_eof = true;
}

/* Convert audio bytes to floats and push them through the resampler. */
static void wavsrc_audio(struct wavsrc *s, const unsigned char *src,
			 size_t n)
{
	unsigned ch = s->ps.w.channels;

	s->data_bytes += n;
	s->data_left -= n;
	while (n) {
		/* complete the partial frame */
		while (n && s->fpart_len < s->frame_bytes)
			s->fpart[s->fpart_len++] = *src++, n--;
		if (s->fpart_len == s->frame_bytes) {
			for (unsigned c = 0; c < ch; c++)
				s->fbuf[c] = s16_to_f(
					(int16_t)wav_rd16(s->fpart + 2 * c));
			if (!wav_resample_push(s->rs, s->fbuf, 1))
				die(EXIT_FAILURE, "brownnote: out of memory");
			s->fpart_len = 0;
		}
		if (n >= s->frame_bytes) {
			size_t frames = n / s->frame_bytes;

			if (frames > 512)
				frames = 512;
			for (size_t i = 0; i < frames * ch; i++)
				s->fbuf[i] = s16_to_f(
					(int16_t)wav_rd16(src + 2 * i));
			if (!wav_resample_push(s->rs, s->fbuf, frames))
				die(EXIT_FAILURE, "brownnote: out of memory");
			src += frames * s->frame_bytes;
			n -= frames * s->frame_bytes;
		}
	}
}

/*
 * The pump's stdin transform: parse until the data chunk, then
 * stream audio.  At EOF, finish whatever state we are in.
 */
static void wavsrc_feed(struct pump *pm, const unsigned char *src,
			size_t n, bool eof)
{
	struct wavsrc *s = pm->xfi_ctx;

	if (s->finished)
		return;
	if (eof) {
		if (!s->parsed && !wav_parse_eof(&s->ps))
			die(EXIT_USAGE, "brownnote: %s", s->ps.err);
		wavsrc_done(s);
		return;
	}
	if (!s->parsed) {
		size_t used = wav_parse_feed(&s->ps, src, n);

		src += used;
		n -= used;
		if (s->ps.state == WAV_PS_ERR)
			die(EXIT_USAGE, "brownnote: %s", s->ps.err);
		if (s->ps.state != WAV_PS_DATA)
			return;		/* still in the header */
		wavsrc_begin(s);
	}
	if (n) {
		uint64_t k = s->data_left;

		if (k > n)
			k = n;
		if (k)
			wavsrc_audio(s, src, (size_t)k);
		/* anything past the budget is trailing metadata */
	}
	if (!s->data_left)
		wavsrc_done(s);
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
 * The bench's wire format is native-endian float32: it reads and
 * writes plain float, with no byte swapping.  Raw mode's documented
 * layout (little-endian) therefore only holds on a little-endian
 * host, and a big-endian host's raw stream would be garbage to every
 * little-endian consumer.  Refuse raw mode there rather than emit a
 * stream that silently violates the documented format.  WAV mode
 * converts explicitly on both sides of the bench and is
 * endian-independent.
 */
static bool host_is_little_endian(void)
{
	const union {
		uint16_t u;
		unsigned char b[2];
	} probe = { .u = 1 };

	return probe.b[0] == 1;
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
			fprintf(stderr, "brownnote: cannot exec %s: %s\n",
				bench, strerror(errno));
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
		 * peeked bytes lead the queue.  The bench's wire format
		 * is native-endian float32, so on a big-endian host the
		 * stream would not be the documented little-endian one
		 * — refuse rather than emit garbage.
		 */
		if (!host_is_little_endian())
			die(EXIT_USAGE, "brownnote: raw mode needs a "
			    "little-endian host (the bench's wire format "
			    "is native-endian float32); WAV mode is "
			    "unaffected");
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
	 * WAV mode: stream stdin through the chunk-walking parser into
	 * the bench's float32 stereo 48 kHz shape, and the bench's
	 * output back through the resampler into the input's rate and
	 * channel count.  Nothing is buffered beyond a block and the
	 * pump's bounded backlog; the output header goes out lazily
	 * with streaming-WAV placeholder sizes and is backfilled when
	 * stdout is a regular file, so a seekable result is
	 * byte-for-byte the batch path's.  See the WAV transforms
	 * above.
	 */
	struct wavsrc src;
	memset(&src, 0, sizeof(src));
	src.pm = &pm;
	wav_parse_init(&src.ps);
	pm.in_fd = 0;
	pm.xf_in = wavsrc_feed;
	pm.xfi_ctx = &src;
	pm.xf_out = wavout_feed;

	/* the four peeked bytes lead the stream */
	if (got)
		wavsrc_feed(&pm, head, got, false);

	bench_status = run_bench(bargv, &pm);
	if (bench_status)
		return bench_status;
	wavout_finish(&pm);
	return 0;
}
