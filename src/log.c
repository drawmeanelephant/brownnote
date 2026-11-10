/*
 * Structured logging, trace ids, feature flags and phase profiling.
 *
 * Everything here is dependency-free C11 + POSIX.  Logging is opt-in so the
 * default stderr contract (plain "brownnote: ..." errors) does not change.
 */
#include "log.h"

#include <ctype.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define TRACE_MAX 64
#define LINE_MAX 2048

static enum bn_log_level threshold = BN_LOG_WARN;
static bool configured;
static char trace_id[TRACE_MAX];

struct flag {
	const char *name;
	bool value;
};

static struct flag flags[] = {
	{"alias-table", false},
	{"profile", false},
};

#define NFLAGS (sizeof(flags) / sizeof(flags[0]))

static bool profile_enabled;
static bool profile_started;
static double profile_mark;
static double profile_total_start;

struct phase {
	const char *name;
	double seconds;
};

static struct phase phases[8];
static size_t n_phases;

static double mono_now(void)
{
	struct timespec ts;

	if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0)
		return 0.0;
	return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static void wall_clock(char *out, size_t cap)
{
	struct timespec ts;

	if (clock_gettime(CLOCK_REALTIME, &ts) != 0) {
		snprintf(out, cap, "0.000");
		return;
	}
	snprintf(out, cap, "%lld.%03ld", (long long)ts.tv_sec,
		 (long)ts.tv_nsec / 1000000L);
}

static const char *level_name(enum bn_log_level level)
{
	switch (level) {
	case BN_LOG_ERROR:
		return "error";
	case BN_LOG_WARN:
		return "warn";
	case BN_LOG_INFO:
		return "info";
	case BN_LOG_DEBUG:
		return "debug";
	}
	return "unknown";
}

static bool equals_ci(const char *a, const char *b)
{
	while (*a && *b) {
		if (tolower((unsigned char)*a) != tolower((unsigned char)*b))
			return false;
		a++;
		b++;
	}
	return *a == '\0' && *b == '\0';
}

static bool sensitive_key(const char *key, size_t len)
{
	static const char *const exact[] = {
		"pass",	  "password", "passwd",	 "token",	  "secret",
		"key",	  "apikey",   "api_key", "authorization", "auth",
		"cookie", "session",  "dsn",	 "credential",	  "credentials",
	};
	static const char *const suffix[] = {
		"_pass", "_password", "_token",		"_secret",
		"_key",	 "_cookie",   "_authorization", "_dsn",
	};
	char buf[64];

	if (len >= sizeof(buf))
		len = sizeof(buf) - 1;
	memcpy(buf, key, len);
	buf[len] = '\0';
	for (size_t i = 0; i < sizeof(exact) / sizeof(exact[0]); i++)
		if (equals_ci(buf, exact[i]))
			return true;
	for (size_t i = 0; i < sizeof(suffix) / sizeof(suffix[0]); i++) {
		size_t n = strlen(suffix[i]);
		if (strlen(buf) >= n &&
		    equals_ci(buf + strlen(buf) - n, suffix[i]))
			return true;
	}
	return false;
}

void bn_log_scrub_value(char *out, size_t cap, const char *key,
			const char *value)
{
	if (sensitive_key(key, strlen(key)))
		snprintf(out, cap, "<redacted>");
	else
		snprintf(out, cap, "%s", value);
}

/*
 * Redact every key=value token whose key looks like a credential.  Values
 * end at whitespace, which covers the shapes this program logs
 * ("token=abc", "password=hunter2").
 */
static void scrub(char *s)
{
	char out[LINE_MAX];
	size_t oi = 0;
	const char *p = s;

	while (*p && oi + 1 < sizeof(out)) {
		if (isspace((unsigned char)*p)) {
			out[oi++] = *p++;
			continue;
		}
		const char *tok = p;
		const char *eq;
		while (*p && !isspace((unsigned char)*p))
			p++;
		size_t tlen = (size_t)(p - tok);
		eq = memchr(tok, '=', tlen);
		if (eq && eq != tok && sensitive_key(tok, (size_t)(eq - tok))) {
			size_t klen = (size_t)(eq - tok) + 1;
			size_t rlen = strlen("<redacted>");
			if (oi + klen + rlen < sizeof(out)) {
				memcpy(out + oi, tok, klen);
				oi += klen;
				memcpy(out + oi, "<redacted>", rlen);
				oi += rlen;
			}
		} else if (oi + tlen < sizeof(out)) {
			memcpy(out + oi, tok, tlen);
			oi += tlen;
		}
	}
	out[oi] = '\0';
	memcpy(s, out, oi + 1);
}

static void parse_log_level(const char *level)
{
	if (equals_ci(level, "error"))
		threshold = BN_LOG_ERROR;
	else if (equals_ci(level, "warn") || equals_ci(level, "warning"))
		threshold = BN_LOG_WARN;
	else if (equals_ci(level, "info"))
		threshold = BN_LOG_INFO;
	else if (equals_ci(level, "debug"))
		threshold = BN_LOG_DEBUG;
}

static bool set_flag(const char *name, bool on)
{
	for (size_t i = 0; i < NFLAGS; i++) {
		if (equals_ci(name, flags[i].name)) {
			flags[i].value = on;
			return true;
		}
	}
	return false;
}

static void parse_flags(const char *spec)
{
	char *copy = strdup(spec);

	if (!copy) {
		fprintf(stderr, "brownnote: out of memory parsing flags\n");
		exit(2);
	}
	char *save = NULL;
	for (char *tok = strtok_r(copy, ",", &save); tok;
	     tok = strtok_r(NULL, ",", &save)) {
		char *eq = strchr(tok, '=');
		bool on = true;
		if (eq) {
			*eq = '\0';
			on = equals_ci(eq + 1, "on") ||
			     equals_ci(eq + 1, "true") ||
			     strcmp(eq + 1, "1") == 0;
		}
		if (!set_flag(tok, on)) {
			fprintf(stderr,
				"brownnote: unknown feature flag '%s' "
				"(known: alias-table, profile)\n",
				tok);
			free(copy);
			exit(2);
		}
	}
	free(copy);
}

static void init_trace_id(const char *id)
{
	if (id && *id) {
		snprintf(trace_id, sizeof(trace_id), "%s", id);
	} else {
		struct timespec ts;
		clock_gettime(CLOCK_REALTIME, &ts);
		snprintf(trace_id, sizeof(trace_id), "%08x%08x",
			 (unsigned)getpid(), (unsigned)ts.tv_nsec);
	}

	/* The bench child inherits the id, so tools can join both processes. */
	setenv("BROWNNOTE_TRACE_ID", trace_id, 0);
}

void bn_log_init(void)
{
	const char *level = getenv("BROWNNOTE_LOG");
	const char *id = getenv("BROWNNOTE_TRACE_ID");
	const char *spec = getenv("BROWNNOTE_FLAGS");
	const char *profile = getenv("BROWNNOTE_PROFILE");

	threshold = BN_LOG_WARN;
	configured = false;
	if (level && *level) {
		parse_log_level(level);
		configured = true;
	}
	init_trace_id(id);
	if (spec && *spec)
		parse_flags(spec);
	profile_enabled = bn_feature("profile") ||
			  (profile && *profile && strcmp(profile, "0") != 0);
}

bool bn_log_configured(void)
{
	return configured;
}

bool bn_log_enabled(enum bn_log_level level)
{
	return (int)level <= (int)threshold;
}

const char *bn_trace_id(void)
{
	return trace_id[0] ? trace_id : "untraced";
}

bool bn_feature(const char *name)
{
	for (size_t i = 0; i < NFLAGS; i++)
		if (equals_ci(name, flags[i].name))
			return flags[i].value;
	return false;
}

void bn_log(enum bn_log_level level, const char *fmt, ...)
{
	char msg[LINE_MAX];
	char stamp[32];
	va_list ap;

	if (!bn_log_enabled(level))
		return;
	va_start(ap, fmt);
	vsnprintf(msg, sizeof(msg), fmt, ap);
	va_end(ap);
	scrub(msg);
	wall_clock(stamp, sizeof(stamp));
	fprintf(stderr, "ts=%s level=%s pid=%ld trace=%s msg=\"%s\"\n", stamp,
		level_name(level), (long)getpid(), bn_trace_id(), msg);
}

void bn_log_kv(enum bn_log_level level, const char *key, const char *value)
{
	char clean[LINE_MAX];

	if (!bn_log_enabled(level))
		return;
	bn_log_scrub_value(clean, sizeof(clean), key, value ? value : "");
	bn_log(level, "%s=%s", key, clean);
}

void bn_profile_begin(void)
{
	profile_mark = mono_now();
	profile_total_start = profile_mark;
	profile_started = true;
}

void bn_profile_phase(const char *name)
{
	double now;

	if (!profile_enabled || !profile_started)
		return;
	now = mono_now();
	for (size_t i = 0; i < n_phases; i++) {
		if (strcmp(phases[i].name, name) == 0) {
			phases[i].seconds += now - profile_mark;
			profile_mark = now;
			return;
		}
	}
	if (n_phases < sizeof(phases) / sizeof(phases[0])) {
		phases[n_phases].name = name;
		phases[n_phases].seconds = now - profile_mark;
		n_phases++;
	}
	profile_mark = now;
}

void bn_profile_dump(void)
{
	double total;

	if (!profile_enabled || !profile_started)
		return;
	total = mono_now() - profile_total_start;
	fprintf(stderr,
		"{\"event\":\"profile\",\"trace\":\"%s\",\"total\":%.4f",
		bn_trace_id(), total);
	for (size_t i = 0; i < n_phases; i++)
		fprintf(stderr, ",\"%s\":%.4f", phases[i].name,
			phases[i].seconds);
	fprintf(stderr, "}\n");
}
