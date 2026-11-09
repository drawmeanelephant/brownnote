//! One selected MIDI input on the pinned `midir` backend.
//!
//! Boundary rules (probe evidence in `native/docs/spikes/midi-input`):
//! - The backend callback only frames and enqueues bounded raw records
//!   ([`callback`]): no locks, no allocation, no mapping decisions there.
//! - [`Controller::poll`] runs on the caller's thread and owns every mapping,
//!   lifecycle (D14) and dispatch decision.
//! - The dispatch [`Sink`] reports a full destination (the 256-slot command
//!   queue) as `false`; the controller counts that loss explicitly and never
//!   leaves a stuck note.
//! - Disconnect/reconnect reconciles held notes and reports loss; mappings are
//!   never changed silently. Device selection is a machine preference (D8);
//!   mappings are project DTOs ([`MappingsDocument`]). This module owns neither
//!   storage format: the project document (#100) and the machine preferences
//!   file are wired by the integrator.
use super::{
    mapping::{
        ControlAction, ControlIntent, DuplicateMapping, Intent, Mapping, MappingKey, Mappings,
        NoteTarget, SourceChannel,
    },
    parser::{self, Data7, Message},
};
use crate::{
    performance::{
        Channel, EventId, InstrumentId, Note, NoteKey, NoteLifecycle, NoteOn, OriginalTime,
        PerformanceError, Pitch, SourceId, TakeId, Target, Velocity,
    },
    project::PadId,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
};

/// Longest raw packet handed across the callback boundary. The selected
/// backend frames complete messages; anything longer is counted and dropped,
/// never reinterpreted.
pub const RAW_CAPACITY: usize = 16;
/// Bounded raw handoff between the backend callback and [`Controller::poll`].
pub const RAW_QUEUE_CAPACITY: usize = 1024;

/// The CoreMIDI client name for enumeration and connections.
pub const CLIENT_NAME: &str = "brownnote-native";
/// The connection name midir shows on the connected port.
pub const CONNECTION_NAME: &str = "brownnote-input";

/// One framed backend packet. Fixed size, so the callback never allocates.
#[derive(Clone, Copy, Debug)]
pub struct RawMessage {
    /// Backend microseconds; the epoch is stable for one connection only.
    pub timestamp_us: u64,
    len: u8,
    bytes: [u8; RAW_CAPACITY],
}

impl RawMessage {
    pub fn new(timestamp_us: u64, bytes: &[u8]) -> Option<Self> {
        if bytes.len() > RAW_CAPACITY {
            return None;
        }
        let len = u8::try_from(bytes.len()).ok()?;
        let mut raw = Self {
            timestamp_us,
            len,
            bytes: [0; RAW_CAPACITY],
        };
        raw.bytes[..bytes.len()].copy_from_slice(bytes);
        Some(raw)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// Loss counters owned by the callback thread. Atomics only: the callback
/// never locks and never allocates.
#[derive(Default)]
pub struct CallbackCounters {
    /// Raw queue was full; the event never reached `poll`.
    pub lost_raw: AtomicU64,
    /// Packet longer than [`RAW_CAPACITY`]; dropped unread.
    pub oversized: AtomicU64,
}

impl CallbackCounters {
    pub fn snapshot(&self) -> CallbackLoss {
        CallbackLoss {
            lost_raw: self.lost_raw.load(Relaxed),
            oversized: self.oversized.load(Relaxed),
        }
    }
}

/// Loss observed on the callback thread since the last snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallbackLoss {
    pub lost_raw: u64,
    pub oversized: u64,
}

/// The `data` handed to the midir callback: the producer half of the bounded
/// raw queue plus the loss counters.
pub struct CallbackState {
    producer: rtrb::Producer<RawMessage>,
    counters: Arc<CallbackCounters>,
}

/// Decode-only callback body: frame, copy bounded bytes, push. No locks, no
/// allocation, no parsing decisions — malformed or unsupported packets are
/// counted downstream, never reinterpreted here.
pub fn callback(timestamp_us: u64, bytes: &[u8], state: &mut CallbackState) {
    match RawMessage::new(timestamp_us, bytes) {
        Some(raw) => {
            if state.producer.push(raw).is_err() {
                state.counters.lost_raw.fetch_add(1, Relaxed);
            }
        }
        None => {
            state.counters.oversized.fetch_add(1, Relaxed);
        }
    }
}

/// What the controller asks the app to do with a resolved message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// One-shot pad hit at full velocity, with its own event ID (D14).
    Trigger {
        pad: PadId,
        velocity: Velocity,
        event: EventId,
        time: OriginalTime,
    },
    /// Held-instrument note started; it entered the real `NoteLifecycle`.
    HeldStart(NoteOn),
    /// Held note completed by note-off, sustain pedal-up, all-notes-off,
    /// panic or disconnect.
    HeldEnd(Note),
    /// Sustain/panic state change in one source+channel scope.
    Control {
        scope: SourceChannel,
        action: ControlIntent,
    },
}

/// The dispatch boundary. Returns `false` when the destination (for example
/// the 256-slot engine command queue) was full and the action was lost.
pub trait Sink {
    fn dispatch(&mut self, action: Action) -> bool;
}

/// Cumulative controller counters. Loss is always reported separately from
/// capture and dispatch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Raw records drained in `poll`.
    pub received: u64,
    /// Unparsable or unsupported packets; counted, never reinterpreted.
    pub malformed: u64,
    /// Well-formed note-on/CC messages with no mapping.
    pub unmapped: u64,
    /// Accepted one-shot triggers.
    pub triggered: u64,
    /// Accepted held-note starts.
    pub held_started: u64,
    /// Completed held notes (whether or not the end dispatch was accepted).
    pub held_ended: u64,
    /// Note-offs with no held identity (one-shot pads, repeated releases).
    pub unmatched_releases: u64,
    /// Dispatches refused by a full destination queue.
    pub lost_commands: u64,
    /// Reconciled disconnects.
    pub disconnects: u64,
    /// Held notes released by a disconnect.
    pub released_on_disconnect: u64,
    /// Raw records that belonged to an already-ended connection.
    pub stale_events: u64,
    /// Clock anomalies (e.g. end before start); the note stays reconcilable.
    pub clock_anomalies: u64,
    /// Messages captured while learn mode was on.
    pub learned: u64,
}

/// A `poll` result: cumulative counters plus callback-thread loss.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub counters: Counters,
    pub callback_loss: CallbackLoss,
}

#[derive(Clone, Copy, Debug)]
struct Epoch {
    /// Timestamp of the first packet of this connection; the backend's
    /// microsecond epoch is stable only for the lifetime of a connection.
    first_timestamp_us: Option<u64>,
    /// Take time when the connection opened.
    base_time: OriginalTime,
}

/// The poll-side state of one selected input: mappings, the real #104 note
/// lifecycle (D14) and the bounded raw consumer. No backend types here, so
/// every behavior is testable without hardware.
pub struct Controller {
    source: SourceId,
    take: TakeId,
    sequence: u64,
    mappings: Mappings,
    lifecycle: NoteLifecycle,
    /// Mirror of the lifecycle's active keys, as counts, for scope enumeration
    /// (all-notes-off, panic, disconnect). Updated only on lifecycle success.
    active: BTreeMap<NoteKey, u32>,
    /// Scopes with the sustain pedal down, keyed by (source, channel) —
    /// `SourceChannel` itself is not `Ord`.
    sustained: BTreeSet<(SourceId, Channel)>,
    /// Physical releases deferred by sustain, per scope, oldest first.
    deferred: BTreeMap<(SourceId, Channel), VecDeque<NoteKey>>,
    consumer: Option<rtrb::Consumer<RawMessage>>,
    epoch: Option<Epoch>,
    learn: bool,
    learn_candidate: Option<MappingKey>,
    counters: Counters,
    callback_counters: Arc<CallbackCounters>,
}

impl Controller {
    pub fn new(take: TakeId, callback_counters: Arc<CallbackCounters>) -> Self {
        Self {
            source: SourceId(0),
            take,
            sequence: 0,
            // An empty entry list has no duplicate keys, so `new` cannot fail.
            mappings: Mappings::new(Vec::new())
                .unwrap_or_else(|_| unreachable!("empty mapping list has no duplicate keys")),
            lifecycle: NoteLifecycle::default(),
            active: BTreeMap::new(),
            sustained: BTreeSet::new(),
            deferred: BTreeMap::new(),
            consumer: None,
            epoch: None,
            learn: false,
            learn_candidate: None,
            counters: Counters::default(),
            callback_counters,
        }
    }

    /// Attach the consumer half of a (possibly new) raw queue. Leftovers of a
    /// replaced queue belonged to a dead connection: they are counted as loss,
    /// not dispatched.
    pub fn attach(&mut self, mut consumer: rtrb::Consumer<RawMessage>) {
        while consumer.pop().is_ok() {
            self.counters.stale_events += 1;
        }
        self.consumer = Some(consumer);
    }

    /// A fresh connection: a registry-owned source ID (never an enumeration
    /// index) and a fresh timestamp epoch based at take time `now`.
    pub fn begin_connection(&mut self, source: SourceId, now: OriginalTime) {
        self.source = source;
        self.epoch = Some(Epoch {
            first_timestamp_us: None,
            base_time: now,
        });
    }

    /// Reconcile a connection that ended: release every held note, clear
    /// sustain, count the disconnect. Mappings are never touched.
    pub fn end_connection(&mut self, sink: &mut impl Sink, now: OriginalTime) -> u64 {
        if self.epoch.is_none() {
            return 0;
        }
        self.counters.disconnects += 1;
        let keys: Vec<NoteKey> = self.active.keys().copied().collect();
        let mut released = 0;
        for key in keys {
            if self.end_held(key, now, sink) {
                released += 1;
                self.counters.released_on_disconnect += 1;
            }
        }
        self.sustained.clear();
        self.deferred.clear();
        self.epoch = None;
        released
    }

    /// Drain the bounded raw queue, parse, resolve and dispatch. Runs on the
    /// caller's thread; the callback only ever pushed raw records.
    pub fn poll(&mut self, sink: &mut impl Sink) -> Report {
        if let Some(mut consumer) = self.consumer.take() {
            while let Ok(raw) = consumer.pop() {
                self.counters.received += 1;
                if self.epoch.is_none() {
                    self.counters.stale_events += 1;
                    continue;
                }
                match parser::parse_message(raw.bytes()) {
                    Err(_) => self.counters.malformed += 1,
                    Ok(message) => {
                        let time = self.original_time(raw.timestamp_us);
                        self.handle(message, time, sink);
                    }
                }
            }
            self.consumer = Some(consumer);
        }
        Report {
            counters: self.counters,
            callback_loss: self.callback_counters.snapshot(),
        }
    }

    /// Backend microseconds to take-relative nanoseconds: a checked
    /// microsecond-to-nanosecond conversion against the connection's first
    /// packet, based at the take time the connection opened with.
    fn original_time(&mut self, timestamp_us: u64) -> OriginalTime {
        let Some(epoch) = &mut self.epoch else {
            return OriginalTime::from_nanos(0);
        };
        let first = *epoch.first_timestamp_us.get_or_insert(timestamp_us);
        let delta_ns = timestamp_us.saturating_sub(first).saturating_mul(1_000);
        OriginalTime::from_nanos(epoch.base_time.nanos().saturating_add(delta_ns))
    }

    fn handle(&mut self, message: Message, time: OriginalTime, sink: &mut impl Sink) {
        if self.learn
            && let Some(key) = MappingKey::learn(message)
        {
            self.learn_candidate = Some(key);
            self.counters.learned += 1;
            return;
        }
        let source = self.source;
        match self.mappings.resolve(source, message) {
            Some(Intent::Start {
                key,
                target,
                velocity,
            }) => {
                // D14: every note-on gets its own event ID.
                self.sequence += 1;
                let event = EventId {
                    take: self.take,
                    sequence: self.sequence,
                };
                match target {
                    NoteTarget::OneShot(pad) => {
                        // One-shot pads trigger once and ignore release.
                        if sink.dispatch(Action::Trigger {
                            pad,
                            velocity,
                            event,
                            time,
                        }) {
                            self.counters.triggered += 1;
                        } else {
                            self.counters.lost_commands += 1;
                        }
                    }
                    NoteTarget::Held(instrument) => {
                        let on = NoteOn {
                            id: event,
                            target: Target::Instrument(instrument),
                            key,
                            velocity,
                            original_time: time,
                        };
                        if sink.dispatch(Action::HeldStart(on.clone())) {
                            match self.lifecycle.note_on(on) {
                                Ok(()) => {
                                    *self.active.entry(key).or_insert(0) += 1;
                                    self.counters.held_started += 1;
                                }
                                Err(_) => self.counters.clock_anomalies += 1,
                            }
                        } else {
                            // The start never reached the engine, so no held
                            // identity is registered for it: no stuck note.
                            self.counters.lost_commands += 1;
                        }
                    }
                }
            }
            Some(Intent::Release(key)) => {
                let scope = (key.source, key.channel);
                if self.sustained.contains(&scope) {
                    // Sustain defers the physical release; FIFO pairing is
                    // preserved and pedal-up releases it.
                    self.deferred.entry(scope).or_default().push_back(key);
                } else {
                    self.end_held(key, time, sink);
                }
            }
            Some(Intent::Control { scope, action }) => {
                if !sink.dispatch(Action::Control { scope, action }) {
                    self.counters.lost_commands += 1;
                }
                self.apply_control((scope.source, scope.channel), action, time, sink);
            }
            None => self.counters.unmapped += 1,
        }
    }

    fn apply_control(
        &mut self,
        scope: (SourceId, Channel),
        action: ControlIntent,
        time: OriginalTime,
        sink: &mut impl Sink,
    ) {
        match action {
            ControlIntent::Sustain(down) => {
                if down {
                    self.sustained.insert(scope);
                } else {
                    self.sustained.remove(&scope);
                    if let Some(deferred) = self.deferred.remove(&scope) {
                        for key in deferred {
                            self.end_held(key, time, sink);
                        }
                    }
                }
            }
            ControlIntent::AllNotesOff => {
                // Acts as releases and honors sustain.
                for key in self.scope_keys(scope) {
                    if self.sustained.contains(&scope) {
                        self.deferred.entry(scope).or_default().push_back(key);
                    } else {
                        self.end_held(key, time, sink);
                    }
                }
            }
            ControlIntent::Panic => {
                // Immediately releases held notes and clears sustain in scope.
                for key in self.scope_keys(scope) {
                    self.end_held(key, time, sink);
                }
                self.sustained.remove(&scope);
                self.deferred.remove(&scope);
            }
        }
    }

    fn scope_keys(&self, scope: (SourceId, Channel)) -> Vec<NoteKey> {
        self.active
            .keys()
            .copied()
            .filter(|key| key.source == scope.0 && key.channel == scope.1)
            .collect()
    }

    /// End the oldest sounding note of this key (D14 FIFO). Returns false when
    /// there was no held identity (one-shot pads) or the end was anomalous; the
    /// note then stays active only in the anomalous case and is reconciled on
    /// disconnect.
    fn end_held(&mut self, key: NoteKey, time: OriginalTime, sink: &mut impl Sink) -> bool {
        match self.lifecycle.note_off(key, time) {
            Ok(note) => {
                if let Some(count) = self.active.get_mut(&key) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        self.active.remove(&key);
                    }
                }
                let accepted = sink.dispatch(Action::HeldEnd(note));
                self.counters.held_ended += 1;
                if !accepted {
                    self.counters.lost_commands += 1;
                }
                true
            }
            Err(PerformanceError::UnmatchedNoteOff(_)) => {
                self.counters.unmatched_releases += 1;
                false
            }
            Err(_) => {
                self.counters.clock_anomalies += 1;
                false
            }
        }
    }

    pub fn mappings(&self) -> &Mappings {
        &self.mappings
    }

    /// Replace the whole mapping set (project load). Already validated by
    /// `Mappings::new`.
    pub fn set_mappings(&mut self, mappings: Mappings) {
        self.mappings = mappings;
    }

    /// Learn/assign one entry, replacing any entry with the same key.
    pub fn set_mapping(&mut self, mapping: Mapping) -> Result<(), DuplicateMapping> {
        let mut entries = self.mappings.entries().to_vec();
        entries.retain(|existing| existing.key() != mapping.key());
        entries.push(mapping);
        self.mappings = Mappings::new(entries)?;
        Ok(())
    }

    pub fn remove_mapping(&mut self, key: MappingKey) -> bool {
        let entries: Vec<Mapping> = self
            .mappings
            .entries()
            .iter()
            .copied()
            .filter(|mapping| mapping.key() != key)
            .collect();
        let removed = entries.len() != self.mappings.entries().len();
        if let Ok(mappings) = Mappings::new(entries) {
            self.mappings = mappings;
        }
        removed
    }

    pub fn set_learn(&mut self, on: bool) {
        self.learn = on;
        if !on {
            self.learn_candidate = None;
        }
    }

    pub fn learn(&self) -> bool {
        self.learn
    }

    pub fn learn_candidate(&self) -> Option<MappingKey> {
        self.learn_candidate
    }

    pub fn take_learn_candidate(&mut self) -> Option<MappingKey> {
        self.learn_candidate.take()
    }

    pub fn source(&self) -> SourceId {
        self.source
    }

    pub fn take(&self) -> TakeId {
        self.take
    }

    pub fn set_take(&mut self, take: TakeId) {
        self.take = take;
    }

    pub fn connected(&self) -> bool {
        self.epoch.is_some()
    }

    pub fn active_held(&self) -> usize {
        self.lifecycle.active_count()
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn callback_loss(&self) -> CallbackLoss {
        self.callback_counters.snapshot()
    }
}

// `Mappings::new` on an empty list cannot fail; the arm above is unreachable.

/// Registry-owned source IDs: a fresh ID per connection, never an enumeration
/// index or a reused identity.
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

fn fresh_source_id() -> SourceId {
    SourceId(u128::from(NEXT_SOURCE.fetch_add(1, Relaxed)))
}

/// One enumerable input port. `id` is the backend's opaque stable ID; `name`
/// is display text and, per D8, only a reconnect hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortInfo {
    pub id: String,
    pub name: String,
}

/// What a `refresh` observed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Refresh {
    pub ports: Vec<PortInfo>,
    /// The connected port disappeared; it was reconciled as a disconnect.
    pub vanished: bool,
    /// A selected port was (re)connected by id or name hint.
    pub reconnected: Option<PortInfo>,
}

/// The midir-backed selected input: enumeration, one owned connection, and
/// the poll-side [`Controller`].
pub struct Input {
    /// Client used only for enumeration; connections create their own.
    enumerator: Option<midir::MidiInput>,
    connection: Option<midir::MidiInputConnection<CallbackState>>,
    port: Option<PortInfo>,
    selected: Option<PortInfo>,
    controller: Controller,
    callback_counters: Arc<CallbackCounters>,
    error: Option<String>,
}

impl Input {
    pub fn new(take: TakeId) -> Result<Self, String> {
        let enumerator = midir::MidiInput::new(CLIENT_NAME)
            .map_err(|error| format!("Cannot start the MIDI backend: {error}"))?;
        Ok(Self::assemble(Some(enumerator), take))
    }

    /// An input with no backend: headless sessions and deterministic scenes
    /// never touch CoreMIDI. `ports` reports the backend as unavailable and
    /// `connect` fails, but the controller can still be driven directly.
    pub fn headless(take: TakeId) -> Self {
        Self::assemble(None, take)
    }

    fn assemble(enumerator: Option<midir::MidiInput>, take: TakeId) -> Self {
        let callback_counters = Arc::new(CallbackCounters::default());
        Self {
            enumerator,
            connection: None,
            port: None,
            selected: None,
            controller: Controller::new(take, callback_counters.clone()),
            callback_counters,
            error: None,
        }
    }

    /// Explicit enumeration of input ports. Never called from a callback.
    pub fn ports(&self) -> Result<Vec<PortInfo>, String> {
        let Some(enumerator) = &self.enumerator else {
            return Err("MIDI backend is unavailable in this session".into());
        };
        Ok(enumerator
            .ports()
            .iter()
            .map(|port| PortInfo {
                id: port.id(),
                name: enumerator.port_name(port).unwrap_or_else(|_| port.id()),
            })
            .collect())
    }

    pub fn select(&mut self, port: PortInfo) {
        self.selected = Some(port);
    }

    /// Restore a saved selection: the id first, the name only as a hint (D8).
    pub fn select_hint(&mut self, id: Option<String>, name: Option<String>) {
        match (id, name) {
            (None, None) => self.selected = None,
            (id, name) => {
                self.selected = Some(PortInfo {
                    id: id.unwrap_or_default(),
                    name: name.unwrap_or_default(),
                })
            }
        }
    }

    pub fn selected(&self) -> Option<&PortInfo> {
        self.selected.as_ref()
    }

    pub fn port(&self) -> Option<&PortInfo> {
        self.port.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn controller(&self) -> &Controller {
        &self.controller
    }

    pub fn controller_mut(&mut self) -> &mut Controller {
        &mut self.controller
    }

    /// Open the selected port. The saved name is only a reconnect hint: the id
    /// wins, and a namesake is never silently selected.
    pub fn connect(&mut self, now: OriginalTime) -> Result<PortInfo, String> {
        if self.connection.is_some() {
            return Err("An input is already connected".into());
        }
        let Some(enumerator) = &self.enumerator else {
            return Err("MIDI backend is unavailable in this session".into());
        };
        let Some(selected) = self.selected.clone() else {
            return Err("Select an input port first".into());
        };
        let ports = enumerator.ports();
        let port = ports
            .iter()
            .find(|port| port.id() == selected.id)
            .or_else(|| {
                ports.iter().find(|port| {
                    enumerator
                        .port_name(port)
                        .is_ok_and(|name| name == selected.name)
                })
            })
            .cloned()
            .ok_or_else(|| {
                format!(
                    "Selected input \"{}\" is not available; refresh the list",
                    selected.name
                )
            })?;
        let name = enumerator.port_name(&port).unwrap_or_else(|_| port.id());
        let input = midir::MidiInput::new(CLIENT_NAME)
            .map_err(|error| format!("Cannot start the MIDI backend: {error}"))?;
        // A fresh bounded handoff for this connection attempt.
        let (producer, consumer) = rtrb::RingBuffer::new(RAW_QUEUE_CAPACITY);
        self.controller.attach(consumer);
        let state = CallbackState {
            producer,
            counters: self.callback_counters.clone(),
        };
        match input.connect(&port, CONNECTION_NAME, callback, state) {
            Ok(connection) => {
                self.connection = Some(connection);
                self.port = Some(PortInfo {
                    id: port.id(),
                    name: name.clone(),
                });
                self.controller.begin_connection(fresh_source_id(), now);
                self.error = None;
                Ok(PortInfo {
                    id: port.id(),
                    name,
                })
            }
            Err(error) => {
                let kind = format!("{:?}", error.kind());
                Err(format!("Cannot connect to \"{name}\": {kind}"))
            }
        }
    }

    /// Close the connection (if any) and reconcile: drain in-flight events
    /// while the epoch is still valid, then release every held note and count
    /// the disconnect. Mappings and the selection are preserved.
    pub fn disconnect(&mut self, sink: &mut impl Sink, now: OriginalTime) -> u64 {
        let _ = self.controller.poll(sink);
        let released = if let Some(connection) = self.connection.take() {
            let (_input, _state) = connection.close();
            // A callback racing with close can still push once; drain again
            // before the epoch ends so it is dispatched, not lost silently.
            let _ = self.controller.poll(sink);
            self.controller.end_connection(sink, now)
        } else {
            0
        };
        self.port = None;
        released
    }

    /// Drain and dispatch pending input.
    pub fn poll(&mut self, sink: &mut impl Sink) -> Report {
        self.controller.poll(sink)
    }

    /// Explicit enumeration: reconcile a vanished port and reconnect a
    /// selected one (by id, else by the saved name hint).
    pub fn refresh(&mut self, sink: &mut impl Sink, now: OriginalTime) -> Result<Refresh, String> {
        let ports = self.ports()?;
        let mut vanished = false;
        if let Some(port) = &self.port
            && !ports.iter().any(|candidate| candidate.id == port.id)
        {
            self.disconnect(sink, now);
            vanished = true;
        }
        let mut reconnected = None;
        if self.connection.is_none()
            && self.selected.is_some()
            && let Ok(port) = self.connect(now)
        {
            reconnected = Some(port);
        }
        Ok(Refresh {
            ports,
            vanished,
            reconnected,
        })
    }

    pub fn status(&self) -> String {
        if let Some(port) = &self.port {
            format!(
                "CONNECTED · {} · source {}",
                port.name,
                self.controller.source().0
            )
        } else if let Some(error) = &self.error {
            format!("DISCONNECTED · {error}")
        } else if self.selected.is_some() {
            "DISCONNECTED · selected input not found; refresh the list".into()
        } else {
            "NO INPUT SELECTED".into()
        }
    }
}

/// Project DTO for mappings (D8). #100 owns adding it to the project document;
/// this is the serialization boundary, not a storage format.
pub const MAPPINGS_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingsDocument {
    pub version: u32,
    pub mappings: Vec<MappingDocument>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MappingDocument {
    Note {
        channel: u8,
        pitch: u8,
        target: NoteTargetDocument,
    },
    Controller {
        channel: u8,
        number: u8,
        action: ControlActionDocument,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NoteTargetDocument {
    OneShot {
        pad: PadId,
    },
    /// Decimal string of the u128 instrument ID (serde_json has no u128).
    Held {
        instrument: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlActionDocument {
    Sustain,
    AllNotesOff,
    Panic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MappingDocumentError {
    UnsupportedVersion(u32),
    InvalidChannel(u8),
    InvalidPitch(u8),
    InvalidController(u8),
    InvalidInstrument(String),
    Duplicate(MappingKey),
}

impl fmt::Display for MappingDocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(f, "midi mappings: unsupported document version {version}")
            }
            Self::InvalidChannel(value) => write!(f, "midi mappings: invalid channel {value}"),
            Self::InvalidPitch(value) => write!(f, "midi mappings: invalid pitch {value}"),
            Self::InvalidController(value) => {
                write!(f, "midi mappings: invalid controller number {value}")
            }
            Self::InvalidInstrument(value) => {
                write!(f, "midi mappings: invalid instrument id \"{value}\"")
            }
            Self::Duplicate(key) => write!(f, "midi mappings: duplicate key {key:?}"),
        }
    }
}
impl std::error::Error for MappingDocumentError {}

impl MappingsDocument {
    pub fn from_mappings(mappings: &Mappings) -> Self {
        Self {
            version: MAPPINGS_FORMAT_VERSION,
            mappings: mappings
                .entries()
                .iter()
                .map(|mapping| MappingDocument::from_mapping(*mapping))
                .collect(),
        }
    }

    pub fn to_mappings(&self) -> Result<Mappings, MappingDocumentError> {
        if self.version != MAPPINGS_FORMAT_VERSION {
            return Err(MappingDocumentError::UnsupportedVersion(self.version));
        }
        let mut entries = Vec::with_capacity(self.mappings.len());
        for document in &self.mappings {
            entries.push(document.clone().into_mapping()?);
        }
        Mappings::new(entries).map_err(|DuplicateMapping(key)| MappingDocumentError::Duplicate(key))
    }
}

impl MappingDocument {
    fn from_mapping(mapping: Mapping) -> Self {
        match mapping {
            Mapping::Note {
                channel,
                pitch,
                target,
            } => Self::Note {
                channel: channel.value(),
                pitch: pitch.value(),
                target: match target {
                    NoteTarget::OneShot(pad) => NoteTargetDocument::OneShot { pad },
                    NoteTarget::Held(instrument) => NoteTargetDocument::Held {
                        instrument: instrument.0.to_string(),
                    },
                },
            },
            Mapping::Controller {
                channel,
                number,
                action,
            } => Self::Controller {
                channel: channel.value(),
                number: number.value(),
                action: match action {
                    ControlAction::Sustain => ControlActionDocument::Sustain,
                    ControlAction::AllNotesOff => ControlActionDocument::AllNotesOff,
                    ControlAction::Panic => ControlActionDocument::Panic,
                },
            },
        }
    }

    fn into_mapping(self) -> Result<Mapping, MappingDocumentError> {
        Ok(match self {
            Self::Note {
                channel,
                pitch,
                target,
            } => Mapping::Note {
                channel: Channel::try_from(channel)
                    .map_err(|_| MappingDocumentError::InvalidChannel(channel))?,
                pitch: Pitch::try_from(pitch)
                    .map_err(|_| MappingDocumentError::InvalidPitch(pitch))?,
                target: match target {
                    NoteTargetDocument::OneShot { pad } => NoteTarget::OneShot(pad),
                    NoteTargetDocument::Held { instrument } => {
                        NoteTarget::Held(InstrumentId(instrument.parse::<u128>().map_err(
                            |_| MappingDocumentError::InvalidInstrument(instrument.clone()),
                        )?))
                    }
                },
            },
            Self::Controller {
                channel,
                number,
                action,
            } => Mapping::Controller {
                channel: Channel::try_from(channel)
                    .map_err(|_| MappingDocumentError::InvalidChannel(channel))?,
                number: Data7::try_from(number)
                    .map_err(|_| MappingDocumentError::InvalidController(number))?,
                action: match action {
                    ControlActionDocument::Sustain => ControlAction::Sustain,
                    ControlActionDocument::AllNotesOff => ControlAction::AllNotesOff,
                    ControlActionDocument::Panic => ControlAction::Panic,
                },
            },
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::{
        engine::{Command, QUEUE_CAPACITY, Sample, Slots},
        output::Output,
    };
    use std::time::Duration;

    fn ch(value: u8) -> Channel {
        Channel::try_from(value).unwrap()
    }
    fn pitch(value: u8) -> Pitch {
        Pitch::try_from(value).unwrap()
    }
    fn data(value: u8) -> Data7 {
        Data7::try_from(value).unwrap()
    }
    fn pad(value: u8) -> PadId {
        PadId::try_from(value).unwrap()
    }

    /// A controller plus the producer half of its raw queue, connected to a
    /// fresh source — the deterministic fixture path, no backend involved.
    fn rig(take: u128) -> (Controller, rtrb::Producer<RawMessage>) {
        let (producer, consumer) = rtrb::RingBuffer::new(RAW_QUEUE_CAPACITY);
        let mut controller = Controller::new(TakeId(take), Arc::new(CallbackCounters::default()));
        controller.attach(consumer);
        controller.begin_connection(SourceId(7), OriginalTime::from_nanos(1_000));
        (controller, producer)
    }

    fn push(producer: &mut rtrb::Producer<RawMessage>, ts_us: u64, bytes: &[u8]) {
        producer
            .push(RawMessage::new(ts_us, bytes).unwrap())
            .unwrap();
    }

    #[derive(Default)]
    struct VecSink {
        actions: Vec<Action>,
    }
    impl Sink for VecSink {
        fn dispatch(&mut self, action: Action) -> bool {
            self.actions.push(action);
            true
        }
    }

    fn mappings(target: NoteTarget) -> Mappings {
        Mappings::new(vec![
            Mapping::Note {
                channel: ch(1),
                pitch: pitch(40),
                target,
            },
            Mapping::Controller {
                channel: ch(1),
                number: data(64),
                action: ControlAction::Sustain,
            },
            Mapping::Controller {
                channel: ch(1),
                number: data(123),
                action: ControlAction::AllNotesOff,
            },
            Mapping::Controller {
                channel: ch(1),
                number: data(120),
                action: ControlAction::Panic,
            },
        ])
        .unwrap()
    }

    fn triggers(sink: &VecSink) -> Vec<(PadId, u8)> {
        sink.actions
            .iter()
            .filter_map(|action| match action {
                Action::Trigger { pad, velocity, .. } => Some((*pad, velocity.value())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn channel_two_note_forty_ignores_other_channels_and_keeps_full_velocity() {
        let (mut controller, mut producer) = rig(1);
        controller
            .set_mapping(Mapping::Note {
                channel: ch(1),
                pitch: pitch(40),
                target: NoteTarget::OneShot(pad(3)),
            })
            .unwrap();
        let mut sink = VecSink::default();
        for velocity in [1u8, 127] {
            push(
                &mut producer,
                10 + u64::from(velocity),
                &[0x91, 40, velocity],
            );
        }
        controller.poll(&mut sink);
        assert_eq!(triggers(&sink), vec![(pad(3), 1), (pad(3), 127)]);
        // Same note on any other channel does not respond.
        for channel in 0..16u8 {
            if channel != 1 {
                push(&mut producer, 200, &[0x90 | channel, 40, 127]);
            }
        }
        push(&mut producer, 201, &[0x91, 41, 127]);
        let before = sink.actions.len();
        controller.poll(&mut sink);
        assert_eq!(sink.actions.len(), before);
        assert_eq!(controller.counters().unmapped, 16);
        assert_eq!(controller.counters().triggered, 2);
    }

    #[test]
    fn repeated_same_pitch_notes_keep_identity_and_end_fifo() {
        let (mut controller, mut producer) = rig(2);
        controller.set_mappings(mappings(NoteTarget::Held(InstrumentId(17))));
        let mut sink = VecSink::default();
        push(&mut producer, 10, &[0x91, 40, 100]);
        push(&mut producer, 20, &[0x91, 40, 90]);
        push(&mut producer, 30, &[0x81, 40, 64]);
        push(&mut producer, 40, &[0x91, 40, 0]);
        controller.poll(&mut sink);
        let starts: Vec<EventId> = sink
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::HeldStart(on) => Some(on.id),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 2);
        assert_ne!(starts[0], starts[1], "every note-on gets its own event ID");
        let ends: Vec<&Note> = sink
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::HeldEnd(note) => Some(note),
                _ => None,
            })
            .collect();
        assert_eq!(ends.len(), 2);
        // D14 FIFO: the first note-off ends the oldest sounding note.
        // Timestamps are microseconds: on@10us -> 1000ns base, off@30us -> 21000ns.
        assert_eq!(ends[0].on().id, starts[0]);
        assert_eq!(ends[0].duration_nanos(), 20_000);
        assert_eq!(ends[1].on().id, starts[1]);
        assert_eq!(ends[1].duration_nanos(), 20_000);
        assert_eq!(controller.active_held(), 0);
        assert_eq!(controller.counters().held_started, 2);
        assert_eq!(controller.counters().held_ended, 2);
    }

    #[test]
    fn cc64_sustain_defers_releases_and_pedal_up_releases_fifo() {
        let (mut controller, mut producer) = rig(3);
        controller.set_mappings(mappings(NoteTarget::Held(InstrumentId(17))));
        let mut sink = VecSink::default();
        push(&mut producer, 10, &[0x91, 40, 100]);
        push(&mut producer, 20, &[0xb1, 64, 127]); // sustain down
        push(&mut producer, 30, &[0x81, 40, 64]); // physical release, deferred
        controller.poll(&mut sink);
        assert_eq!(controller.active_held(), 1, "sustain holds the note");
        assert_eq!(controller.counters().held_ended, 0);
        push(&mut producer, 40, &[0xb1, 64, 0]); // pedal up
        controller.poll(&mut sink);
        assert_eq!(controller.active_held(), 0);
        assert_eq!(controller.counters().held_ended, 1);
        let end = sink.actions.iter().find_map(|action| match action {
            Action::HeldEnd(note) => Some(note.original_end()),
            _ => None,
        });
        // Pedal up at 40us: base 1000ns + (40-10)us * 1000 = 31000ns.
        assert_eq!(end, Some(OriginalTime::from_nanos(31_000)));
    }

    #[test]
    fn panic_releases_held_notes_immediately_despite_sustain() {
        let (mut controller, mut producer) = rig(4);
        controller.set_mappings(mappings(NoteTarget::Held(InstrumentId(17))));
        let mut sink = VecSink::default();
        push(&mut producer, 10, &[0x91, 40, 100]);
        push(&mut producer, 20, &[0xb1, 64, 100]); // sustain down
        push(&mut producer, 30, &[0x81, 40, 64]); // deferred
        push(&mut producer, 40, &[0xb1, 120, 0]); // panic
        controller.poll(&mut sink);
        assert_eq!(controller.active_held(), 0, "panic ignores sustain");
        assert_eq!(controller.counters().held_ended, 1);
        // Sustain was cleared in scope: a later physical release is unmatched.
        push(&mut producer, 50, &[0x81, 40, 64]);
        controller.poll(&mut sink);
        assert_eq!(controller.counters().unmatched_releases, 1);
    }

    #[test]
    fn all_notes_off_acts_as_release_and_honors_sustain() {
        let (mut controller, mut producer) = rig(5);
        controller.set_mappings(mappings(NoteTarget::Held(InstrumentId(17))));
        let mut sink = VecSink::default();
        push(&mut producer, 10, &[0x91, 40, 100]);
        push(&mut producer, 20, &[0xb1, 123, 0]); // all notes off
        controller.poll(&mut sink);
        assert_eq!(controller.active_held(), 0);
        assert_eq!(controller.counters().held_ended, 1);
        // With sustain down, all-notes-off is deferred until pedal up.
        push(&mut producer, 30, &[0x91, 40, 100]);
        push(&mut producer, 40, &[0xb1, 64, 127]);
        push(&mut producer, 50, &[0xb1, 123, 0]);
        controller.poll(&mut sink);
        assert_eq!(controller.active_held(), 1, "sustain defers all-notes-off");
        push(&mut producer, 60, &[0xb1, 64, 0]);
        controller.poll(&mut sink);
        assert_eq!(controller.active_held(), 0);
        assert_eq!(controller.counters().held_ended, 2);
    }

    #[test]
    fn note_on_velocity_zero_acts_as_note_off() {
        let (mut controller, mut producer) = rig(6);
        controller.set_mappings(mappings(NoteTarget::Held(InstrumentId(17))));
        let mut sink = VecSink::default();
        push(&mut producer, 10, &[0x91, 40, 100]);
        push(&mut producer, 30, &[0x91, 40, 0]); // velocity-zero note on == off
        controller.poll(&mut sink);
        assert_eq!(controller.active_held(), 0);
        assert_eq!(controller.counters().held_ended, 1);
        assert_eq!(controller.counters().unmatched_releases, 0);
    }

    #[test]
    fn one_shot_pads_trigger_once_and_ignore_release() {
        let (mut controller, mut producer) = rig(7);
        controller.set_mappings(mappings(NoteTarget::OneShot(pad(5))));
        let mut sink = VecSink::default();
        push(&mut producer, 10, &[0x91, 40, 90]);
        push(&mut producer, 20, &[0x81, 40, 64]);
        push(&mut producer, 30, &[0x91, 40, 0]);
        controller.poll(&mut sink);
        assert_eq!(triggers(&sink), vec![(pad(5), 90)]);
        assert!(
            sink.actions
                .iter()
                .all(|action| !matches!(action, Action::HeldStart(_) | Action::HeldEnd(_)))
        );
        assert_eq!(controller.active_held(), 0);
        assert_eq!(controller.counters().unmatched_releases, 2);
    }

    /// A sink backed by the real 256-slot engine command queue.
    struct QueueSink<'a> {
        output: &'a mut Output,
    }
    impl Sink for QueueSink<'_> {
        fn dispatch(&mut self, action: Action) -> bool {
            match action {
                Action::Trigger { pad, velocity, .. } => self.output.send(Command::Trigger {
                    pad: pad.index(),
                    slot: 0,
                    group: 0,
                    velocity: f32::from(velocity.value()) / 127.0,
                    gain: 1.0,
                    pan: 0.0,
                    semitones: 0.0,
                }),
                _ => true,
            }
        }
    }

    #[test]
    fn burst_beyond_queue_capacity_counts_loss_and_later_input_continues() {
        let (mut controller, mut producer) = rig(8);
        controller
            .set_mapping(Mapping::Note {
                channel: ch(1),
                pitch: pitch(40),
                target: NoteTarget::OneShot(pad(0)),
            })
            .unwrap();
        let mut output = Output::virtual_driver(
            Slots::from_samples([Sample {
                data: Arc::from([0.25; 64]),
                rate: 44_100,
            }]),
            "test".into(),
            false,
        );
        // Fill the 256-slot queue exactly.
        for _ in 0..QUEUE_CAPACITY {
            assert!(output.send(Command::Panic));
        }
        assert!(!output.send(Command::Panic));
        assert_eq!(output.dropped, 1);
        // A burst beyond capacity: every dispatch is refused and counted.
        for index in 0..8u64 {
            push(&mut producer, 10 + index, &[0x91, 40, 100]);
        }
        {
            let mut sink = QueueSink {
                output: &mut output,
            };
            controller.poll(&mut sink);
        }
        assert_eq!(controller.counters().lost_commands, 8);
        assert_eq!(controller.counters().triggered, 0);
        assert_eq!(output.dropped, 9);
        // The virtual driver drains on advance; later input works again.
        output.advance(0.01);
        push(&mut producer, 100, &[0x91, 40, 100]);
        {
            let mut sink = QueueSink {
                output: &mut output,
            };
            controller.poll(&mut sink);
        }
        assert_eq!(controller.counters().triggered, 1);
        assert_eq!(controller.counters().lost_commands, 8);
        assert_eq!(
            output.dropped, 9,
            "the accepted trigger was queued, not dropped"
        );
    }

    #[test]
    fn disconnect_releases_held_notes_and_reconnect_keeps_mappings() {
        let (mut controller, mut producer) = rig(9);
        let original = mappings(NoteTarget::Held(InstrumentId(17)));
        controller.set_mappings(original.clone());
        let mut sink = VecSink::default();
        push(&mut producer, 10, &[0x91, 40, 100]);
        push(&mut producer, 20, &[0xb1, 64, 127]); // sustain down
        push(&mut producer, 30, &[0x81, 40, 64]); // deferred
        controller.poll(&mut sink);
        assert_eq!(controller.active_held(), 1);
        // Deterministic disconnect: held notes are released, sustain cleared.
        let released = controller.end_connection(&mut sink, OriginalTime::from_nanos(9_000));
        assert_eq!(released, 1);
        assert_eq!(controller.counters().disconnects, 1);
        assert_eq!(controller.counters().released_on_disconnect, 1);
        assert_eq!(controller.active_held(), 0);
        assert_eq!(controller.mappings(), &original, "never silently changed");
        assert!(!controller.connected());
        // Events that arrive after the epoch ended are counted as stale loss.
        let actions_before = sink.actions.len();
        push(&mut producer, 40, &[0x91, 40, 100]);
        controller.poll(&mut sink);
        assert_eq!(controller.counters().stale_events, 1);
        assert_eq!(
            sink.actions.len(),
            actions_before,
            "stale events are not dispatched"
        );
        // Reconnect with a fresh source: mappings still resolve, new identity.
        let (mut producer2, consumer2) = rtrb::RingBuffer::new(RAW_QUEUE_CAPACITY);
        controller.attach(consumer2);
        controller.begin_connection(SourceId(9), OriginalTime::from_nanos(20_000));
        push(&mut producer2, 10, &[0x91, 40, 80]);
        push(&mut producer2, 20, &[0x81, 40, 0]);
        controller.poll(&mut sink);
        assert_eq!(controller.counters().held_started, 2);
        assert_eq!(controller.counters().held_ended, 2);
        assert_eq!(controller.active_held(), 0);
        let keys: Vec<SourceId> = sink
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::HeldStart(on) => Some(on.key.source),
                _ => None,
            })
            .collect();
        assert_eq!(
            keys,
            vec![SourceId(7), SourceId(9)],
            "fresh source per connection"
        );
    }

    #[test]
    fn callback_counts_losses_without_reinterpreting_bytes() {
        let (producer, _consumer) = rtrb::RingBuffer::new(2);
        let counters = Arc::new(CallbackCounters::default());
        let mut state = CallbackState {
            producer,
            counters: counters.clone(),
        };
        callback(1, &[0x91, 40, 100], &mut state);
        callback(2, &[0x91, 40, 90], &mut state);
        callback(3, &[0x91, 40, 80], &mut state); // raw queue full
        callback(4, &[0xf0; 20], &mut state); // oversized
        let loss = counters.snapshot();
        assert_eq!(loss.lost_raw, 1);
        assert_eq!(loss.oversized, 1);
        // Malformed/unsupported packets are counted at poll, never reinterpreted.
        let (mut producer, consumer) = rtrb::RingBuffer::new(8);
        let mut controller = Controller::new(TakeId(1), counters);
        controller.attach(consumer);
        controller.begin_connection(SourceId(3), OriginalTime::from_nanos(0));
        for (ts, bytes) in [
            (10u64, &[0xf8][..]),       // clock: unsupported
            (11, &[0x90, 40][..]),      // wrong length
            (12, &[0x90, 200, 1][..]),  // high data bit
            (13, &[0x91, 40, 100][..]), // valid but unmapped
        ] {
            producer.push(RawMessage::new(ts, bytes).unwrap()).unwrap();
        }
        let mut sink = VecSink::default();
        controller.poll(&mut sink);
        assert_eq!(controller.counters().malformed, 3);
        assert_eq!(controller.counters().received, 4);
        assert_eq!(controller.counters().unmapped, 1);
        assert!(sink.actions.is_empty());
    }

    #[test]
    fn learn_captures_on_and_cc_but_never_a_release() {
        let (mut controller, mut producer) = rig(10);
        controller
            .set_mapping(Mapping::Note {
                channel: ch(1),
                pitch: pitch(40),
                target: NoteTarget::OneShot(pad(0)),
            })
            .unwrap();
        let mut sink = VecSink::default();
        controller.set_learn(true);
        push(&mut producer, 10, &[0x91, 40, 100]);
        controller.poll(&mut sink);
        assert_eq!(
            controller.learn_candidate(),
            Some(MappingKey::Note {
                channel: ch(1),
                pitch: pitch(40)
            })
        );
        assert_eq!(controller.counters().learned, 1);
        assert!(
            sink.actions.is_empty(),
            "learn mode captures, does not trigger"
        );
        push(&mut producer, 20, &[0xb1, 64, 127]);
        controller.poll(&mut sink);
        assert_eq!(
            controller.take_learn_candidate(),
            Some(MappingKey::Controller {
                channel: ch(1),
                number: data(64)
            })
        );
        push(&mut producer, 30, &[0x81, 40, 64]); // release never learns
        controller.poll(&mut sink);
        assert_eq!(controller.learn_candidate(), None);
        assert_eq!(controller.counters().learned, 2);
        controller.set_learn(false);
        assert_eq!(controller.learn_candidate(), None);
    }

    #[test]
    fn mapping_document_round_trips_through_json() {
        let original = Mappings::new(vec![
            Mapping::Note {
                channel: ch(1),
                pitch: pitch(40),
                target: NoteTarget::OneShot(pad(3)),
            },
            Mapping::Note {
                channel: ch(2),
                pitch: pitch(41),
                target: NoteTarget::Held(InstrumentId(17)),
            },
            Mapping::Controller {
                channel: ch(1),
                number: data(64),
                action: ControlAction::Sustain,
            },
            Mapping::Controller {
                channel: ch(0),
                number: data(120),
                action: ControlAction::Panic,
            },
            Mapping::Controller {
                channel: ch(15),
                number: data(123),
                action: ControlAction::AllNotesOff,
            },
        ])
        .unwrap();
        let document = MappingsDocument::from_mappings(&original);
        let json = serde_json::to_string_pretty(&document).unwrap();
        let parsed: MappingsDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, document);
        assert_eq!(parsed.to_mappings().unwrap(), original);
        // Invalid values are rejected, duplicates too, and the version gate holds.
        let bad_channel = r#"{"version":1,"mappings":[{"type":"note","channel":16,"pitch":40,"target":{"type":"one_shot","pad":3}}]}"#;
        let parsed: MappingsDocument = serde_json::from_str(bad_channel).unwrap();
        assert_eq!(
            parsed.to_mappings(),
            Err(MappingDocumentError::InvalidChannel(16))
        );
        let duplicate = r#"{"version":1,"mappings":[{"type":"note","channel":1,"pitch":40,"target":{"type":"one_shot","pad":3}},{"type":"note","channel":1,"pitch":40,"target":{"type":"one_shot","pad":4}}]}"#;
        let parsed: MappingsDocument = serde_json::from_str(duplicate).unwrap();
        assert!(matches!(
            parsed.to_mappings(),
            Err(MappingDocumentError::Duplicate(_))
        ));
        let old_version = r#"{"version":0,"mappings":[]}"#;
        let parsed: MappingsDocument = serde_json::from_str(old_version).unwrap();
        assert_eq!(
            parsed.to_mappings(),
            Err(MappingDocumentError::UnsupportedVersion(0))
        );
    }

    #[test]
    fn headless_input_reports_no_backend_but_still_drives_the_controller() {
        let mut input = Input::headless(TakeId(1));
        assert!(input.ports().is_err());
        input.select(PortInfo {
            id: "x".into(),
            name: "X".into(),
        });
        assert!(input.connect(OriginalTime::from_nanos(0)).is_err());
        assert!(input.status().contains("NO INPUT SELECTED") || input.selected().is_some());
        // The controller is still drivable for scenes and tests.
        let (mut producer, consumer) = rtrb::RingBuffer::new(RAW_QUEUE_CAPACITY);
        input.controller_mut().attach(consumer);
        input
            .controller_mut()
            .set_mapping(Mapping::Note {
                channel: ch(1),
                pitch: pitch(40),
                target: NoteTarget::OneShot(pad(0)),
            })
            .unwrap();
        input
            .controller_mut()
            .begin_connection(SourceId(1), OriginalTime::from_nanos(0));
        let mut sink = VecSink::default();
        push(&mut producer, 5, &[0x91, 40, 127]);
        input.poll(&mut sink);
        assert_eq!(triggers(&sink), vec![(pad(0), 127)]);
    }

    /// End-to-end through the real CoreMIDI backend: a process-owned virtual
    /// source (the same mechanism as the macOS IAC Driver), connected,
    /// messaged, disconnected and reconciled.
    #[cfg(target_os = "macos")]
    #[test]
    fn coremidi_virtual_port_end_to_end() {
        use midir::{MidiOutput, os::unix::VirtualOutput};
        let name = format!("brownnote-input-test-{}", std::process::id());
        let mut source = MidiOutput::new("brownnote-input-test-source")
            .map_err(|error| format!("{error:?}"))
            .and_then(|output| {
                output
                    .create_virtual(&name)
                    .map_err(|error| format!("{error:?}"))
            })
            .unwrap_or_else(|error| panic!("virtual source: {error}"));
        let mut input = Input::new(TakeId(11)).unwrap_or_else(|error| panic!("{error}"));
        input
            .controller_mut()
            .set_mapping(Mapping::Note {
                channel: ch(1),
                pitch: pitch(40),
                target: NoteTarget::OneShot(pad(0)),
            })
            .unwrap();
        // Enumerate, select, connect.
        let ports = input.ports().unwrap();
        let port = ports
            .into_iter()
            .find(|port| port.name == name)
            .unwrap_or_else(|| panic!("virtual port missing from enumeration"));
        input.select(port);
        let connected = input
            .connect(OriginalTime::from_nanos(0))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(connected.name, name);
        assert!(input.status().starts_with("CONNECTED"));
        // Send a note-on and a velocity-zero note-off through the real backend.
        source.send(&[0x91, 40, 100]).unwrap();
        source.send(&[0x91, 40, 0]).unwrap();
        let mut sink = VecSink::default();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let report = input.poll(&mut sink);
            if report.counters.triggered >= 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "virtual port never delivered: {:?}",
                report.counters
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(triggers(&sink), vec![(pad(0), 100)]);
        // The velocity-zero note-off can be delivered in a later CoreMIDI
        // callback than the note-on, so wait for it like the disconnect below.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let report = input.poll(&mut sink);
            if report.counters.unmatched_releases >= 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "velocity-zero note-off never arrived: {:?}",
                report.counters
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(input.poll(&mut sink).counters.unmatched_releases, 1);
        // Remove the source: refresh must detect the vanish and reconcile.
        drop(source);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut vanished = false;
        loop {
            let now = OriginalTime::from_nanos(1_000_000);
            match input.refresh(&mut sink, now) {
                Ok(refresh) => vanished |= refresh.vanished,
                Err(error) => panic!("refresh: {error}"),
            }
            if vanished || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(vanished, "source removal was not detected");
        assert_eq!(input.poll(&mut sink).counters.disconnects, 1);
        assert!(input.port().is_none());
        assert!(input.status().starts_with("DISCONNECTED"));
        // Mappings survived the disconnect.
        assert_eq!(input.controller().mappings().entries().len(), 1);
    }
}
