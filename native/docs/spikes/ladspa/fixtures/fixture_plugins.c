/*
 * Deliberately small and partly malformed LADSPA plugins for the #97 probe.
 * Original Brownnote code, MIT licensed. Not a musical plugin.
 *
 * The structs repeat the LADSPA 1.1 ABI so the fixture needs no SDK header.
 * If FIXTURE_LIFECYCLE_LOG is set, each lifecycle call appends one line.
 */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>

typedef float LADSPA_Data;
typedef void *LADSPA_Handle;

typedef struct {
	int HintDescriptor;
	LADSPA_Data LowerBound;
	LADSPA_Data UpperBound;
} LADSPA_PortRangeHint;

typedef struct _LADSPA_Descriptor {
	unsigned long UniqueID;
	const char *Label;
	int Properties;
	const char *Name;
	const char *Maker;
	const char *Copyright;
	unsigned long PortCount;
	const int *PortDescriptors;
	const char *const *PortNames;
	const LADSPA_PortRangeHint *PortRangeHints;
	void *ImplementationData;
	LADSPA_Handle (*instantiate)(const struct _LADSPA_Descriptor *, unsigned long);
	void (*connect_port)(LADSPA_Handle, unsigned long, LADSPA_Data *);
	void (*activate)(LADSPA_Handle);
	void (*run)(LADSPA_Handle, unsigned long);
	void (*run_adding)(LADSPA_Handle, unsigned long);
	void (*set_run_adding_gain)(LADSPA_Handle, LADSPA_Data);
	void (*deactivate)(LADSPA_Handle);
	void (*cleanup)(LADSPA_Handle);
} LADSPA_Descriptor;

enum { IN = 1, OUT = 2, CONTROL = 4, AUDIO = 8 };
enum {
	BELOW = 0x1, ABOVE = 0x2, TOGGLED = 0x4, SAMPLE_RATE = 0x8, LOG = 0x10,
	INTEGER = 0x20, D_MIN = 0x40, D_LOW = 0x80, D_MID = 0xC0, D_MAX = 0x140,
	D_1 = 0x240
};
enum { INPLACE_BROKEN = 0x2 };
enum { MODE_PASS, MODE_NAN, MODE_ENDLESS };

#define MAX_PORTS 12

typedef struct {
	const LADSPA_Descriptor *d;
	LADSPA_Data *port[MAX_PORTS];
	LADSPA_Data gain;
	unsigned long frame;
} Fixture;

static void note(const LADSPA_Descriptor *d, const char *what)
{
	const char *path = getenv("FIXTURE_LIFECYCLE_LOG");
	FILE *log;
	if (!path || !(log = fopen(path, "a")))
		return;
	fprintf(log, "%s %s\n", d->Label, what);
	fclose(log);
}

static LADSPA_Handle instantiate(const LADSPA_Descriptor *d, unsigned long rate)
{
	Fixture *f = calloc(1, sizeof *f);
	(void)rate;
	if (!f)
		return NULL;
	f->d = d;
	f->gain = 1;
	note(d, "instantiate");
	return f;
}

static void connect_port(LADSPA_Handle h, unsigned long i, LADSPA_Data *p)
{
	if (i < MAX_PORTS)
		((Fixture *)h)->port[i] = p;
}

static void activate(LADSPA_Handle h) { note(((Fixture *)h)->d, "activate"); }
static void deactivate(LADSPA_Handle h) { note(((Fixture *)h)->d, "deactivate"); }

static void cleanup(LADSPA_Handle h)
{
	note(((Fixture *)h)->d, "cleanup");
	free(h);
}

static int mode(const Fixture *f) { return (int)(long)f->d->ImplementationData; }

/* Port 0 is audio in and port 1 audio out for every fixture; "hints"
 * echoes its four control inputs (2..5) to its control outputs (6..9). */
static void process(LADSPA_Handle h, unsigned long n, int adding)
{
	Fixture *f = h;
	unsigned long i;
	for (i = 0; i < n; ++i, ++f->frame) {
		LADSPA_Data y = f->port[0][i];
		if (mode(f) == MODE_NAN && f->frame >= 1000)
			y = NAN;
		else if (mode(f) == MODE_ENDLESS)
			y = 0.5f;
		if (adding)
			f->port[1][i] += f->gain * y;
		else
			f->port[1][i] = y;
	}
	if (f->d->PortCount == 10)
		for (i = 0; i < 4; ++i)
			*f->port[6 + i] = *f->port[2 + i];
}

static void run(LADSPA_Handle h, unsigned long n) { process(h, n, 0); }
static void run_adding(LADSPA_Handle h, unsigned long n) { process(h, n, 1); }
static void set_gain(LADSPA_Handle h, LADSPA_Data g) { ((Fixture *)h)->gain = g; }

static const char *const hint_names[] = {
	"in", "out", "rate", "log", "int", "toggle",
	"rate_out", "log_out", "int_out", "toggle_out"};
static const int hint_ports[] = {
	IN | AUDIO, OUT | AUDIO, IN | CONTROL, IN | CONTROL, IN | CONTROL, IN | CONTROL,
	OUT | CONTROL, OUT | CONTROL, OUT | CONTROL, OUT | CONTROL};
static const LADSPA_PortRangeHint hint_ranges[] = {
	{0, 0, 0}, {0, 0, 0},
	{BELOW | ABOVE | SAMPLE_RATE | D_MAX, 0, 0.5f},
	{BELOW | ABOVE | LOG | D_MID, 20, 20000},
	{BELOW | ABOVE | INTEGER | D_LOW, 1, 8},
	{TOGGLED | D_1, 0, 0},
	{0, 0, 0}, {0, 0, 0}, {0, 0, 0}, {0, 0, 0}};

static const char *const pass_names[] = {"in", "out"};
static const int pass_ports[] = {IN | AUDIO, OUT | AUDIO};
static const LADSPA_PortRangeHint pass_ranges[] = {{0, 0, 0}, {0, 0, 0}};

static const int no_output_ports[] = {IN | AUDIO};
static const int bad_ports[] = {IN | AUDIO, IN | OUT | AUDIO};

static const char *const bad_hint_names[] = {"in", "out", "inverted"};
static const int bad_hint_ports[] = {IN | AUDIO, OUT | AUDIO, IN | CONTROL};
static const LADSPA_PortRangeHint bad_hint_ranges[] = {
	{0, 0, 0}, {0, 0, 0}, {BELOW | ABOVE | D_MID, 1, 0}};

static const char *const wide_names[] = {"in", "out1", "out2", "out3"};
static const int wide_ports[] = {IN | AUDIO, OUT | AUDIO, OUT | AUDIO, OUT | AUDIO};
static const LADSPA_PortRangeHint wide_ranges[] = {
	{0, 0, 0}, {0, 0, 0}, {0, 0, 0}, {0, 0, 0}};

#define FIXTURE(id, label, props, count, ports, names, ranges, mode_, adding) \
	{id, label, props, label, "Brownnote fixture", "MIT", count, ports, names, \
	 ranges, (void *)(long)(mode_), instantiate, connect_port, activate, run, \
	 adding ? run_adding : NULL, adding ? set_gain : NULL, deactivate, cleanup}

static const LADSPA_Descriptor fixtures[] = {
	FIXTURE(9701, "hints", INPLACE_BROKEN, 10, hint_ports, hint_names, hint_ranges, MODE_PASS, 1),
	FIXTURE(9702, "no_output", 0, 1, no_output_ports, pass_names, pass_ranges, MODE_PASS, 0),
	FIXTURE(9703, "bad_port", 0, 2, bad_ports, pass_names, pass_ranges, MODE_PASS, 0),
	FIXTURE(9704, "nan", 0, 2, pass_ports, pass_names, pass_ranges, MODE_NAN, 0),
	FIXTURE(9705, "endless", 0, 2, pass_ports, pass_names, pass_ranges, MODE_ENDLESS, 0),
	FIXTURE(9706, "bad_hint", 0, 3, bad_hint_ports, bad_hint_names, bad_hint_ranges, MODE_PASS, 0),
	FIXTURE(9707, "wide", 0, 4, wide_ports, wide_names, wide_ranges, MODE_PASS, 0),
};

const LADSPA_Descriptor *ladspa_descriptor(unsigned long index)
{
	return index < sizeof fixtures / sizeof fixtures[0] ? &fixtures[index] : NULL;
}
