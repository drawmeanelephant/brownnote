/*
 * Unit tests for src/log.c: trace ids, feature-flag parsing and the
 * credential redaction used by structured logging.
 */
#include "log.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int checks;
static int failed;

static void check(bool ok, const char *what)
{
	checks++;
	if (!ok) {
		failed++;
		fprintf(stderr, "FAIL: %s\n", what);
	}
}

int main(void)
{
	char buf[64];

	unsetenv("BROWNNOTE_LOG");
	unsetenv("BROWNNOTE_FLAGS");
	unsetenv("BROWNNOTE_PROFILE");
	unsetenv("BROWNNOTE_TRACE_ID");
	bn_log_init();

	check(strlen(bn_trace_id()) > 0, "a trace id is always available");
	check(strlen(bn_trace_id()) < 64, "trace id stays bounded");
	check(!bn_feature("alias-table"), "alias-table defaults off");
	check(!bn_feature("profile"), "profile defaults off");
	check(!bn_feature("no-such-flag"), "unknown flags are off");

	setenv("BROWNNOTE_FLAGS", "alias-table=on,profile=off", 1);
	setenv("BROWNNOTE_TRACE_ID", "trace-123", 1);
	bn_log_init();
	check(bn_feature("alias-table"), "alias-table=on enables the flag");
	check(!bn_feature("profile"), "profile=off keeps the flag off");
	check(strcmp(bn_trace_id(), "trace-123") == 0,
	      "BROWNNOTE_TRACE_ID is honored");

	setenv("BROWNNOTE_FLAGS", "profile=1", 1);
	bn_log_init();
	check(bn_feature("profile"), "numeric 1 enables a flag");

	bn_log_scrub_value(buf, sizeof(buf), "password", "hunter2");
	check(strcmp(buf, "<redacted>") == 0, "password is redacted");
	bn_log_scrub_value(buf, sizeof(buf), "api_token", "abc");
	check(strcmp(buf, "<redacted>") == 0, "api_token is redacted");
	bn_log_scrub_value(buf, sizeof(buf), "Authorization", "Bearer x");
	check(strcmp(buf, "<redacted>") == 0, "Authorization is redacted");
	bn_log_scrub_value(buf, sizeof(buf), "user", "alice");
	check(strcmp(buf, "alice") == 0, "user is not redacted");
	bn_log_scrub_value(buf, sizeof(buf), "monkey", "banana");
	check(strcmp(buf, "banana") == 0, "keys merely containing 'key' pass");

	bn_profile_begin();
	bn_profile_phase("parse");
	bn_profile_phase("parse");
	bn_profile_dump();

	/*
	 * Exercise the emit path, not just the helpers: capture stderr, log at
	 * every level, and check the line shape and redaction as a reader
	 * would.
	 */
	char path[] = "build/unit_log_stderr.XXXXXX";
	int fd = mkstemp(path);
	int saved = dup(STDERR_FILENO);
	check(fd >= 0 && saved >= 0, "stderr can be captured");
	if (fd >= 0 && saved >= 0) {
		fflush(stderr);
		dup2(fd, STDERR_FILENO);
		close(fd);

		unsetenv("BROWNNOTE_FLAGS");
		setenv("BROWNNOTE_LOG", "debug", 1);
		setenv("BROWNNOTE_TRACE_ID", "trace-log", 1);
		bn_log_init();
		check(bn_log_configured(), "BROWNNOTE_LOG configures logging");
		check(bn_log_enabled(BN_LOG_DEBUG),
		      "debug is on at debug level");
		check(bn_log_enabled(BN_LOG_ERROR), "errors are always on");

		bn_log(BN_LOG_INFO, "unit test %d", 7);
		bn_log_kv(BN_LOG_WARN, "password", "hunter2");
		bn_log_kv(BN_LOG_DEBUG, "monkey", "banana");
		bn_log(BN_LOG_ERROR, "error %s", "path");

		setenv("BROWNNOTE_LOG", "warn", 1);
		bn_log_init();
		check(!bn_log_enabled(BN_LOG_DEBUG),
		      "warn level suppresses debug events");
		check(bn_log_enabled(BN_LOG_WARN), "warn level keeps warnings");

		fflush(stderr);
		dup2(saved, STDERR_FILENO);
		close(saved);
		clearerr(stderr);

		char log[2048] = {0};
		FILE *f = fopen(path, "r");
		if (f) {
			size_t n = fread(log, 1, sizeof(log) - 1, f);
			log[n] = '\0';
			fclose(f);
		}
		check(strstr(log, "level=info") != NULL, "the level is logged");
		check(strstr(log, "trace=trace-log") != NULL,
		      "the trace id is logged");
		check(strstr(log, "level=error") != NULL,
		      "errors are logged too");
		check(strstr(log, "msg=\"unit test 7\"") != NULL,
		      "the message is logged");
		check(strstr(log, "password=<redacted>") != NULL,
		      "logged credential values are redacted");
		check(strstr(log, "monkey=banana") != NULL,
		      "non-credential values stay readable");
		check(strstr(log, "hunter2") == NULL,
		      "the secret never reaches the log");
		remove(path);
	}

	printf("unit_log: %s %d/%d checks\n", failed ? "FAILED" : "PASSED",
	       checks - failed, checks);
	return failed ? 1 : 0;
}
