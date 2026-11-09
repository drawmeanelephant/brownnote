# Open Instrument fit assessment (#117)

## Recommendation

**No integration now (negative result).** Open Instrument stays an
assessment target, not a dependency, a project format, or an item on the
release path. No device was available to test (decision D2 in
[`native/docs/decisions.md`](../../decisions.md)), no user named one, and no
musical use case was proposed. #117's hardware close gate therefore stays
open. This document records the desk research only. It adds no code, no
dependency, and no adapter.

## Research question

Narrowly: does Open Instrument offer a musical controller or input
capability that Brownnote does not already get from MIDI, and is adapter
work justified now? It asks nothing about adopting the standards as a
hardware design, and nothing about test-equipment control.

## Draft status is not compatibility

The issue pins revision
`24b2caefb40bfa3c64251e09c3536c1e8b85668d` in the standards repository.
That revision exists and is the tip of `main` as of 2026-11-09: two
commits, both dated 16 August 2026, the later one titled "Publish Draft 1
Open Instrument standards". The repository lists **no releases** and **no
tags**.

At that revision, all five standards (OI-01 Link, OI-02 Protocol, OI-03
Power, OI-04 Standalone, OI-05 Stack) declare:

```
Version: 0.1.0
Status:  Draft 1
Date:    2026-08-16
```

The repository README calls them "design drafts, not yet stable
interoperability guarantees", and sections marked TBD, provisional,
candidate, or not frozen remain open. OI-02 lists the wire serialization,
unit encoding, fault schema, and measurement time-sync model among its
open decisions.

Brownnote makes **no Open Instrument compatibility claim** of any kind. It
does not carry an OI Compatibility Badge, claims no standard, and adds no
conformance statement to the project document. Naming a draft revision in
a document is citation, not conformance, and conformance to a draft is not
conformance to a released standard, because no released standard exists.

## The mismatch: measurement equipment, not a musical controller

Open Instrument aims at test and measurement hardware. Its own vocabulary
is power supplies, electronic loads, DAQ channels, multimeters, and
fixture I/O. OI-02's common operations are `get_identity()`,
`get_capabilities()`, `get_measurements()`, `arm()`, `configure_trigger()`
and setpoint calls on a supply or load channel.

Its contribution over a plain serial link is defined in those terms:
capability-based control, requested-versus-actual state, structured
measurements and faults, deterministic hardware triggering (OI-01: CAN-FD
control plus a dedicated differential trigger over CAT5e), and composition
of several instruments into one virtual instrument. None of that is a
performance event vocabulary. In the Draft 1 material read for this
assessment (the standards README, the project site, and OI-02 in full)
there is no note, pitch, velocity, pad, take, or musical-time model, and
nothing about transporting a performance event into Brownnote's take
clock.

Brownnote's existing input path is MIDI, which the
[MIDI input spike](../midi-input/README.md) already covers at the backend,
parser, mapping, and lifecycle boundaries. On the evidence read here, Open
Instrument does not name a capability Brownnote is missing. That is a
statement about the documents, not a prediction about where the project
goes, and this assessment does not speculate about future features or
timelines.

## Why nothing is justified yet

- Decision D2 records that no MIDI controller, audio interface, or
  external synth is available, and that #117 is parked until gear exists.
- No user has nominated a specific Open Instrument device or musical use
  for it, so there is no target to research, no host protocol to read, and
  no timing claim to check.
- Without a device, any adapter would be written against a draft and an
  imagined use. #117's acceptance criteria ask for the honest finding
  instead.

## What a future Go decision would need

The same gate the issue sets, unchanged by this document:

- a user-confirmed, named device and a stated musical use for it;
- a documented host protocol for that device, at a pinned revision;
- meaningful timing evidence for note-like events, not just message
  round-trips;
- a bounded mapping into Brownnote's performance-event path, with the
  limits written down;
- defined behaviour for reconnect and malformed or unexpected data;
- decoder fixtures checked in and reproducible;
- live hardware-in-the-loop evidence, kept separate from trace replay.

This document does not design that adapter. A future change that closes
the gate would arrive with a device, a reproduced result, and its own
review.

## Sources

All fetched 2026-11-09.

| Source | Status | Use here |
| --- | --- | --- |
| [openinstrument.dev](https://openinstrument.dev/) | fetched, 200 | Project scope, Draft 1 standard summaries, and the "current status" open-items list. |
| [standards tree at `24b2caef`](https://github.com/open-instrument-project/standards/tree/24b2caefb40bfa3c64251e09c3536c1e8b85668d) | fetched, 200 | Draft 1 metadata, Apache-2.0 licence, "no releases published", no tags. |
| [commits on `main`](https://github.com/open-instrument-project/standards/commits/main) | fetched, 200 | Confirms `24b2caef` is the newest revision, dated 16 August 2026. |
| [OI-02 Protocol at `24b2caef`](https://github.com/open-instrument-project/standards/blob/24b2caefb40bfa3c64251e09c3536c1e8b85668d/OI-02-Protocol/index.md) | fetched, 200 | Device/resource model, common operations, measurements and faults, open wire-format decisions. |

No third-party summary is cited. Nothing here is a benchmark, a latency
measurement, or a compatibility test; no hardware was run.

## Status

#117 remains open on its hardware gate. The desk-research slice is done;
the device slice is not started and cannot start until a device is named.
See the roadmap entry [G7](../../../../docs/site/content/roadmap.md#g7-hardware)
for the product framing.
