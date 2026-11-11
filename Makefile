CC ?= cc
# Dead code is a build error: -Wall already reports unused statics and
# variables, and the suite must not keep code nobody calls.
CFLAGS ?= -O2 -Wall -Wextra -Werror=unused-function \
	-Werror=unused-variable -std=gnu11 -D_DEFAULT_SOURCE
LDFLAGS ?=
LDLIBS ?= -lm
CARGO ?= cargo
PYTHON ?= python3
CLANG_FORMAT ?= clang-format

BIN = build/brownnote
UNIT = build/unit_wav
UNIT_LOG = build/unit_log
FUZZ = build/fuzz_wav
COV_DIR = build/coverage

# sanitizer flags per the hardening issue: run the whole suite, the
# unit tests and the fuzz corpus under ASan+UBSan
SAN_CFLAGS = -O1 -g -fsanitize=address,undefined \
	-fno-sanitize-recover=undefined -fno-omit-frame-pointer

all: $(BIN)

build:
	@mkdir -p build

$(BIN): build/brownnote.o build/wav.o build/log.o
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $^ $(LDLIBS)

build/brownnote.o: src/brownnote.c src/wav.h src/log.h | build
	$(CC) $(CFLAGS) -c -o $@ src/brownnote.c

build/wav.o: src/wav.c src/wav.h | build
	$(CC) $(CFLAGS) -c -o $@ src/wav.c

build/log.o: src/log.c src/log.h | build
	$(CC) $(CFLAGS) -c -o $@ src/log.c

$(UNIT): tests/unit_wav.c build/wav.o src/wav.h | build
	$(CC) $(CFLAGS) $(LDFLAGS) -Isrc -o $@ tests/unit_wav.c build/wav.o $(LDLIBS)

$(UNIT_LOG): tests/unit_log.c build/log.o src/log.h | build
	$(CC) $(CFLAGS) $(LDFLAGS) -Isrc -o $@ tests/unit_log.c build/log.o $(LDLIBS)

unit: $(UNIT) $(UNIT_LOG)
	$(UNIT)
	$(UNIT_LOG)

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
# The two pinned SoundCraft GUI tests are skipped by name too: they need the
# pinned app, CLI and Python and run only through `make native-soundcraft-test`,
# so `--include-ignored` still passes without BROWNNOTE_SOUNDCRAFT_RUN_PINNED=1.
CPAL_DEVICE_TEST = cpal_device_callback_timing_with_the_live_instrument
SOUNDCRAFT_SKIPS = --skip pinned_app_recipe_and_adapter \
	--skip pinned_app_reports_v3_live_take_notes_and_imports_the_retained_audio \
	--skip pinned_app_converts_writer_bundles_with_off_slice_sources

native-test: native-plugin-host
	$(CARGO) fmt --manifest-path native/Cargo.toml --check
	$(CARGO) clippy --manifest-path native/Cargo.toml --all-targets -- -D warnings
	$(CLAP_TEST_ENV) $(LADSPA_TEST_ENV) $(CARGO) test --manifest-path native/Cargo.toml -- --include-ignored --skip $(CPAL_DEVICE_TEST) $(SOUNDCRAFT_SKIPS)

# The render deadline and signal fixtures repeated in fresh processes with a
# slow `dirname` on PATH, which is how a startup timing regression shows up.
# It rebuilds with `--lib --no-run` and repeats three cases three times each,
# so it stays out of `native-test`; the `native-core` job in
# `.github/workflows/native.yml` runs it as its own step.
native-render-timeout:
	python3 native/tests/check_render_timeout.py

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
	$(CARGO) test --manifest-path native/Cargo.toml --test soundcraft_adapter pinned_app_reports_v3_live_take_notes_and_imports_the_retained_audio -- --ignored --exact
	$(CARGO) test --manifest-path native/Cargo.toml --test soundcraft_adapter pinned_app_converts_writer_bundles_with_off_slice_sources -- --ignored --exact

# ------------------------------------------------------------------
# Quality gates.  CI runs these through `make hygiene`; the pre-commit
# hook runs the staged-file subset.
# ------------------------------------------------------------------

fmt:
	$(CLANG_FORMAT) -i src/*.c src/*.h tests/*.c
	$(CARGO) fmt --manifest-path native/Cargo.toml
	$(CARGO) fmt --all --manifest-path native/plugin-host/Cargo.toml

fmt-check: fmt-check-c fmt-check-rust

fmt-check-c:
	$(CLANG_FORMAT) --dry-run --Werror src/*.c src/*.h tests/*.c

fmt-check-rust:
	$(CARGO) fmt --manifest-path native/Cargo.toml --check
	$(CARGO) fmt --all --manifest-path native/plugin-host/Cargo.toml --check

hooks:
	git config core.hooksPath .githooks
	chmod +x .githooks/pre-commit
	@echo "pre-commit hook installed (core.hooksPath=.githooks)"

# One command from a fresh clone to a built tree with the hooks installed.
setup: hooks
	@test -d vendor/GuitarPedal || \
		git clone --depth 1 https://github.com/torvalds/GuitarPedal vendor/GuitarPedal
	@test -x vendor/GuitarPedal/Validation/bench || \
		$(MAKE) -C vendor/GuitarPedal/Validation bench
	$(MAKE) all
	@echo "setup: ok — 'make test' is the gate"

studio-check:
	$(PYTHON) tests/studio_smoke.py
	$(PYTHON) tests/studio_security.py
	$(PYTHON) tests/dast_studio.py

# Against an already-running studio, for a deployed or remote instance:
#   make dast STUDIO_URL=127.0.0.1:8765
dast:
	$(PYTHON) tests/dast_studio.py --url $(STUDIO_URL)

check-todos:
	scripts/check_todos.sh

check-large:
	$(PYTHON) scripts/check_large_files.py

check-versions:
	$(PYTHON) scripts/check_versions.py

check-agents:
	$(PYTHON) tests/check_agents_md.py

check-openapi:
	$(PYTHON) tests/check_openapi.py

check-secrets:
	$(PYTHON) scripts/check_secrets.py

complexity: complexity-c complexity-rust

# lizard arrives as a console script (pipx, venv or a distro package); fall
# back to the module form when only the library is installed.
LIZARD ?= $(shell command -v lizard 2>/dev/null || printf '%s -m lizard' "$(PYTHON)")

complexity-c:
	$(PYTHON) scripts/check_complexity.py

complexity-rust:
	$(CARGO) clippy --manifest-path native/Cargo.toml --all-targets -- -D warnings -W clippy::cognitive_complexity

dup:
	npx --yes jscpd@4 --config .jscpd.json

unused-deps:
	$(CARGO) machete native
	$(CARGO) machete native/plugin-host

# The coverage target dir is pinned to native/target (instead of
# cargo-llvm-cov's default native/target/llvm-cov-target) because
# `paths::Resolver` only recognizes Cargo's native/target/{debug,release}[/deps]
# layout when it looks for recordings/, build/brownnote and the bench. A
# deeper target dir makes those tests fall back to a non-existent tools/ tree.
native-coverage:
	CARGO_LLVM_COV_TARGET_DIR=native/target \
		$(CARGO) llvm-cov --manifest-path native/Cargo.toml --lcov \
		--output-path native/target/lcov.info --fail-under-lines 60

coverage: 
	@mkdir -p $(COV_DIR)
	$(CC) $(CFLAGS) --coverage -Isrc -c -o $(COV_DIR)/wav.o src/wav.c
	$(CC) $(CFLAGS) --coverage -Isrc -c -o $(COV_DIR)/log.o src/log.c
	$(CC) $(CFLAGS) --coverage $(LDFLAGS) -Isrc -o $(COV_DIR)/unit_wav \
		tests/unit_wav.c $(COV_DIR)/wav.o $(LDLIBS)
	$(CC) $(CFLAGS) --coverage $(LDFLAGS) -Isrc -o $(COV_DIR)/unit_log \
		tests/unit_log.c $(COV_DIR)/log.o $(LDLIBS)
	$(COV_DIR)/unit_wav >/dev/null
	$(COV_DIR)/unit_log >/dev/null
	$(PYTHON) scripts/coverage_gate.py --dir $(COV_DIR) --minimum 70 \
		--source src/wav.c --source src/log.c

sizes: native
	$(CARGO) tree --manifest-path native/Cargo.toml --duplicates
	$(PYTHON) scripts/check_binary_size.py \
		--binary native/target/release/brownnote-native --budget-mb 160

hygiene: fmt-check check-todos check-large check-versions check-agents \
	check-openapi check-secrets complexity dup
	@echo "hygiene: ok"

.PHONY: all test unit fuzz fuzz-libfuzzer sanitize clean presets studio studio-check dast native native-test native-render-timeout native-soundcraft-test native-plugin-host native-clap-effect-test native-plugin-workflow-test native-clap-live-test native-clap-live-cpal-test native-ladspa-plate-test native-ladspa-workflow-test fmt fmt-check fmt-check-c fmt-check-rust hooks setup check-todos check-large check-versions check-agents check-openapi check-secrets complexity complexity-c complexity-rust dup unused-deps coverage native-coverage sizes hygiene
