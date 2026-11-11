#ifndef BROWNNOTE_LOG_H
#define BROWNNOTE_LOG_H

#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>

/*
 * Structured diagnostics for the CLI.
 *
 * Logging is opt-in: with BROWNNOTE_LOG unset nothing new reaches stderr,
 * so the historical error text and the suite's expectations are unchanged.
 * Set BROWNNOTE_LOG=error|warn|info|debug for key=value lines that carry a
 * correlation id and redact credential-shaped values.
 *
 * The trace id comes from BROWNNOTE_TRACE_ID or is generated per process; it
 * is exported to the bench child so one run can be followed end to end.
 *
 * Feature flags (BROWNNOTE_FLAGS=name=on,other=off) are a fixed registry:
 * an unknown name is a usage error, never a silent typo.
 *
 * Profiling (BROWNNOTE_PROFILE=1 or profile=on) records phase durations and
 * prints one JSON object at exit.
 */

enum bn_log_level {
	BN_LOG_ERROR = 0,
	BN_LOG_WARN,
	BN_LOG_INFO,
	BN_LOG_DEBUG,
};

void bn_log_init(void);
bool bn_log_enabled(enum bn_log_level level);
bool bn_log_configured(void);

void bn_log(enum bn_log_level level, const char *fmt, ...)
	__attribute__((format(printf, 2, 3)));

/* Log one key=value pair, redacting the value when the key is sensitive. */
void bn_log_kv(enum bn_log_level level, const char *key, const char *value);

/* Copy a value into out, or "<redacted>" when key names a credential. */
void bn_log_scrub_value(char *out, size_t cap, const char *key,
			const char *value);

const char *bn_trace_id(void);

bool bn_feature(const char *name);

void bn_profile_begin(void);
void bn_profile_phase(const char *name);
void bn_profile_dump(void);

#endif /* BROWNNOTE_LOG_H */
