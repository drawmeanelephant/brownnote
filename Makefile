CC ?= cc
CFLAGS ?= -O2 -Wall -Wextra -std=gnu11 -D_DEFAULT_SOURCE
LDLIBS ?= -lm

BIN = build/brownnote

all: $(BIN)

$(BIN): src/brownnote.c
	@mkdir -p build
	$(CC) $(CFLAGS) -o $@ $< $(LDLIBS)

test: all
	BENCH="$${BENCH:-vendor/GuitarPedal/Validation/bench/bench}" \
		tests/run_tests.sh

clean:
	rm -f $(BIN)

.PHONY: all test clean
