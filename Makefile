CC ?= cc
CFLAGS ?= -O2 -Wall -Wextra -std=gnu11 -D_DEFAULT_SOURCE
LDFLAGS ?=
LDLIBS ?= -lm

BIN = build/brownnote
UNIT = build/unit_wav

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

clean:
	rm -rf build

.PHONY: all test unit clean
