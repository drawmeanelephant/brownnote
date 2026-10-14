CC ?= cc
CFLAGS ?= -O2 -Wall -Wextra -std=gnu11 -D_DEFAULT_SOURCE
LDFLAGS ?=
LDLIBS ?= -lm

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

.PHONY: all test unit fuzz fuzz-libfuzzer sanitize clean presets studio
