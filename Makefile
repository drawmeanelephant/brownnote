CC ?= cc
CFLAGS ?= -O2 -Wall -Wextra -std=gnu11 -D_DEFAULT_SOURCE
LDFLAGS ?=
LDLIBS ?= -lm
CARGO ?= cargo

BIN = build/brownnote
UNIT = build/unit_wav
FUZZ = build/fuzz_wav

# sanitizer flags per the hardening issue: run the whole suite, the
# unit tests and the fuzz corpus under ASan+UBSan
SAN_CFLAGS = -O1 -g -fsanitize=address,undefined \
	-fno-sanitize-recover=undefined -fno-omit-frame-pointer

all: $(BIN)

build:
	@mkdir -p build

$(BIN): build/brownnote.o build/wav.o
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $^ $(LDLIBS)

build/brownnote.o: src/brownnote.c src/wav.h | build
	$(CC) $(CFLAGS) -c -o $@ src/brownnote.c

build/wav.o: src/wav.c src/wav.h | build
	$(CC) $(CFLAGS) -c -o $@ src/wav.c

$(UNIT): tests/unit_wav.c build/wav.o src/wav.h | build
	$(CC) $(CFLAGS) $(LDFLAGS) -Isrc -o $@ tests/unit_wav.c build/wav.o $(LDLIBS)

unit: $(UNIT)
	$(UNIT)

test: all unit
	BENCH="$${BENCH:-vendor/GuitarPedal/Validation/bench/bench}" \
		tests/run_tests.sh

# corpus driver: run every seed input and a deterministic mutation
# round under ASan/UBSan (the binary is always built sanitized)
$(FUZZ): tests/fuzz_wav.c src/wav.c src/wav.h | build
	$(CC) $(CFLAGS) $(SAN_CFLAGS) $(LDFLAGS) -Isrc \
		-o $@ tests/fuzz_wav.c src/wav.c $(LDLIBS)

fuzz: $(FUZZ)
	$(FUZZ) tests/corpus/*

# clang/libFuzzer build of the same target (LLVMFuzzerTestOneInput)
$(FUZZ)-lf: tests/fuzz_wav.c src/wav.c src/wav.h | build
	$(CC) $(CFLAGS) $(SAN_CFLAGS) -fsanitize=fuzzer $(LDFLAGS) -Isrc \
		-DFUZZ_LIBFUZZER -o $@ tests/fuzz_wav.c src/wav.c $(LDLIBS)

fuzz-libfuzzer: $(FUZZ)-lf
	$(FUZZ)-lf tests/corpus -max_total_time=60

# rebuild everything sanitized and run the full battery; CFLAGS is
# exported so the suite's inner `make` keeps the same flags
sanitize:
	$(MAKE) clean
	CFLAGS="-O1 -g -Wall -Wextra -std=gnu11 -D_DEFAULT_SOURCE $(SAN_CFLAGS)" \
		$(MAKE) test fuzz

clean:
	rm -rf build

presets: $(BIN)
	scripts/presets.sh pack

studio: $(BIN)
	python3 scripts/studio.py

native:
	$(CARGO) build --release --manifest-path native/Cargo.toml

# The offline plugin helper and its CLAP fault fixture: a separate workspace
# because it allows unsafe FFI. Tests find the binaries through these paths.
PLUGIN_HOST = native/plugin-host/Cargo.toml
PLUGIN_HOST_OUT = $${CARGO_TARGET_DIR:-$(CURDIR)/native/plugin-host/target}/debug
DYLIB = $(if $(filter Darwin,$(shell uname -s)),dylib,so)
CLAP_TEST_ENV = BROWNNOTE_CLAP_HELPER="$(PLUGIN_HOST_OUT)/brownnote-plugin-host" \
	BROWNNOTE_CLAP_FAULT_FIXTURE="$(PLUGIN_HOST_OUT)/libbrownnote_clap_fault_fixture.$(DYLIB)" \
	BROWNNOTE_REQUIRE_CLAP=1
LADSPA_TEST_ENV = BROWNNOTE_LADSPA_HELPER="$(PLUGIN_HOST_OUT)/brownnote-plugin-host" \
	BROWNNOTE_LADSPA_FIXTURE="$(PLUGIN_HOST_OUT)/libbrownnote_ladspa_fault_fixture.$(DYLIB)" \
	BROWNNOTE_REQUIRE_LADSPA=1

native-plugin-host:
	$(CARGO) fmt --all --manifest-path $(PLUGIN_HOST) --check
	$(CARGO) clippy --locked --manifest-path $(PLUGIN_HOST) --workspace --all-targets -- -D warnings
	$(CARGO) build --locked --manifest-path $(PLUGIN_HOST) --workspace

# `cpal_device_callback_timing_with_the_live_instrument` is skipped by name:
# it opens the default output device and fails under BROWNNOTE_REQUIRE_CLAP=1
# unless BROWNNOTE_LIVE_CPAL=1, so it runs only through
# `make native-clap-live-cpal-test` on a machine with audio hardware.
CPAL_DEVICE_TEST = cpal_device_callback_timing_with_the_live_instrument

native-test: native-plugin-host
	$(CARGO) fmt --manifest-path native/Cargo.toml --check
	$(CARGO) clippy --manifest-path native/Cargo.toml --all-targets -- -D warnings
	$(CLAP_TEST_ENV) $(LADSPA_TEST_ENV) $(CARGO) test --manifest-path native/Cargo.toml -- --include-ignored --skip pinned_app_recipe_and_adapter --skip $(CPAL_DEVICE_TEST)

# Explicit opt-in: requires the pinned Airwindows Consolidated bundle in
# BROWNNOTE_CLAP_EFFECT (see native/plugin-host/README.md).
native-clap-effect-test: native-plugin-host
	$(CLAP_TEST_ENV) BROWNNOTE_REQUIRE_CLAP_EFFECT=1 $(CARGO) test --manifest-path native/Cargo.toml --test clap_offline -- --nocapture

# Mounted app workflow, including the pinned real effect, with no helper skips.
native-plugin-workflow-test: native-plugin-host
	$(CLAP_TEST_ENV) BROWNNOTE_REQUIRE_CLAP_EFFECT=1 $(CARGO) test --manifest-path native/Cargo.toml --bin brownnote-native ui::pedals::plugin::tests:: -- --nocapture

# Required (not opt-in): the live CLAP instrument helper tests hard-fail
# under BROWNNOTE_REQUIRE_CLAP=1 when the brownnote-clap-live binary or the
# fault fixture is missing. The mounted app live lane (recall, retained take
# audio, reopen-disabled, export) runs here too, so its tests cannot silently
# skip in CI.
#
# The real output device test is excluded by name, not passed: CI has no
# audio hardware, and BROWNNOTE_REQUIRE_CLAP=1 makes an unset
# BROWNNOTE_LIVE_CPAL a failure instead of a skip. The callback's allocation
# behaviour is already covered device-free by
# `engine_callback_with_the_live_instrument_allocates_and_frees_nothing`
# here, and its timing math by tests/output_timing.rs in `make native-test`.
# The device measurement itself is `make native-clap-live-cpal-test`.
native-clap-live-test: native-plugin-host
	$(CLAP_TEST_ENV) $(CARGO) test --manifest-path native/Cargo.toml --test clap_live -- --nocapture --skip $(CPAL_DEVICE_TEST)
	$(CLAP_TEST_ENV) $(CARGO) test --manifest-path native/Cargo.toml --bin brownnote-native ui::live::tests:: -- --nocapture

# Explicit opt-in: opens the default output device and measures the composed
# callback against its real period (BROWNNOTE_LIVE_CPAL=1). Needs audio
# hardware, so it is not part of any required lane.
native-clap-live-cpal-test: native-plugin-host
	$(CLAP_TEST_ENV) BROWNNOTE_LIVE_CPAL=1 $(CARGO) test --manifest-path native/Cargo.toml --test clap_live -- $(CPAL_DEVICE_TEST) --exact --nocapture

# Explicit opt-in: provisions and verifies the pinned CAPS 0.9.26 library
# (native/plugin-host/caps/provision.py, hard hash failures, nothing bundled).
native-ladspa-plate-test: native-plugin-host
	python3 native/plugin-host/caps/provision.py
	$(LADSPA_TEST_ENV) BROWNNOTE_LADSPA_PLATE=$$(python3 native/plugin-host/caps/provision.py | tail -n 1) \
	BROWNNOTE_REQUIRE_LADSPA_PLATE=1 $(CARGO) test --manifest-path native/Cargo.toml --test ladspa_offline -- --nocapture

# Mounted app LADSPA workflow, including the pinned Plate, with no helper or
# Plate skips.
native-ladspa-workflow-test: native-plugin-host
	python3 native/plugin-host/caps/provision.py
	$(LADSPA_TEST_ENV) BROWNNOTE_LADSPA_PLATE=$$(python3 native/plugin-host/caps/provision.py | tail -n 1) \
	BROWNNOTE_REQUIRE_LADSPA_PLATE=1 $(CARGO) test --manifest-path native/Cargo.toml --bin brownnote-native ui::pedals::plugin::tests:: -- --nocapture

# Explicit opt-in: requires the pinned SoundCraft app, CLI and Python environment.
native-soundcraft-test:
	$(CARGO) test --manifest-path native/Cargo.toml --test soundcraft_adapter pinned_app_recipe_and_adapter -- --ignored --exact

.PHONY: all test unit fuzz fuzz-libfuzzer sanitize clean presets studio native native-test native-soundcraft-test native-plugin-host native-clap-effect-test native-plugin-workflow-test native-clap-live-test native-clap-live-cpal-test native-ladspa-plate-test native-ladspa-workflow-test
