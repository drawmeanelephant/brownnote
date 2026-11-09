//! The live CLAP instrument in the app (#112): the machine helper setting,
//! one mounted session, and the routing of held notes, sustain and panic from
//! the existing MIDI input path.
//!
//! Everything here runs on the UI thread. Starting and stopping the helper
//! (and handing the finished [`LiveVoice`] to the output stream) happens off
//! the audio callback; this module never schedules audio itself. It forwards
//! each action to the backend's bounded [`LiveSender`], which names the output
//! frame using the callback's own clock.
//!
//! The routing and status are wired now; choosing a plugin and opening the
//! output with the returned voice is the project-recall slice (B), so the
//! mounting half of this lane is not yet called by the app.
#![allow(dead_code)]

use super::State;
use brownnote_native::{
    midi::mapping::ControlIntent,
    paths::{Resolver, Resource},
    pedals::offline::{HelperCommand, Stage},
    performance::{Channel, EventId, Note, NoteOn, Pitch, Velocity},
    plugins::{
        helper,
        live::{
            LiveAction, LiveConfig, LiveEvent, LiveHost, LiveSender, LiveState, LiveStatus,
            LiveVoice, SendOutcome,
        },
    },
};
use std::path::{Path, PathBuf};

struct Session {
    host: LiveHost,
    sender: LiveSender,
    plugin: String,
}

/// The live-instrument lane state. `session` is the only live producer; the
/// callback's voice is owned by the output stream, not here.
#[derive(Default)]
pub struct Live {
    pub root: Option<PathBuf>,
    pub helper: Option<HelperCommand>,
    pub helper_error: Option<String>,
    pub status: Option<LiveStatus>,
    pub notice: Option<String>,
    pub error: Option<String>,
    session: Option<Session>,
}

impl Live {
    /// Safe for headless/test constructors: no settings file, no helper.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Only interactive startup calls this, with the machine preferences root.
    /// A missing optional helper is reported when a session is mounted, not as
    /// a startup error.
    pub fn load(root: Result<PathBuf, String>) -> Self {
        let mut live = Self::disabled();
        match root {
            Ok(root) => live.root = Some(root),
            Err(error) => live.helper_error = Some(error),
        }
        live
    }

    /// The machine-level helper program: saved preference, environment
    /// override, then the bounded executable-relative layout (D8).
    pub fn helper_command(&mut self) -> Result<HelperCommand, String> {
        if let Some(helper) = &self.helper {
            return Ok(helper.clone());
        }
        let paths = Resolver::current().map_err(|error| error.to_string())?;
        let helper = helper::resolve(&paths, self.root.as_deref())?;
        self.helper = Some(helper.clone());
        Ok(helper)
    }

    /// Replace the resolved helper (the preference editor's in-memory value).
    pub fn set_helper(&mut self, helper: HelperCommand) {
        self.helper = Some(helper);
        self.helper_error = None;
    }

    pub fn mounted(&self) -> bool {
        self.session.is_some()
    }

    pub fn status(&self) -> Option<LiveStatus> {
        self.session.as_ref().map(|session| session.host.status())
    }

    /// Starts the helper and the worker, returning the voice the output stream
    /// must hold. On failure the lane records why and stays unmounted; the
    /// sample mixer is untouched.
    pub fn mount(&mut self, stage: Stage, rate: u32, buffer_frames: u32) -> Option<LiveVoice> {
        self.unmount();
        let helper = match self.helper_command() {
            Ok(helper) => helper,
            Err(error) => {
                self.error = Some(error);
                return None;
            }
        };
        let config = LiveConfig::for_device(helper, rate, buffer_frames);
        match LiveHost::start(stage, config) {
            Ok((host, sender, voice)) => {
                self.session = Some(Session {
                    plugin: host.info().plugin.id.clone(),
                    host,
                    sender,
                });
                self.status = None;
                self.error = None;
                self.notice = Some("Live instrument started".into());
                Some(voice)
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.session = None;
                None
            }
        }
    }

    /// Closing the voice is the output stream's job; dropping the host tells
    /// the worker to close the helper, which may be inside a deadline.
    pub fn unmount(&mut self) {
        if let Some(session) = self.session.take() {
            self.notice = Some(format!("Live instrument {} stopped", session.plugin));
            drop(session);
        }
    }

    fn send(&mut self, action: LiveAction) -> Option<SendOutcome> {
        let session = self.session.as_mut()?;
        let frame = session.sender.earliest_exact();
        let outcome = session.sender.try_send(LiveEvent { frame, action });
        if let SendOutcome::Full { release_recovery } = outcome {
            self.notice = Some(if release_recovery {
                "Live event queue full; the instrument was asked to recover held notes".into()
            } else {
                "Live event queue full; a note may be late".into()
            });
        }
        Some(outcome)
    }

    pub fn note_on(
        &mut self,
        id: EventId,
        channel: Channel,
        pitch: Pitch,
        velocity: Velocity,
    ) -> Option<SendOutcome> {
        self.send(LiveAction::NoteOn {
            id,
            channel,
            pitch,
            velocity,
        })
    }

    pub fn note_off(&mut self, id: EventId) -> Option<SendOutcome> {
        self.send(LiveAction::NoteOff { id })
    }

    pub fn sustain(&mut self, channel: Channel, down: bool) -> Option<SendOutcome> {
        self.send(LiveAction::Sustain { channel, down })
    }

    pub fn panic(&mut self) -> Option<SendOutcome> {
        self.send(LiveAction::Panic)
    }

    /// Refreshes the observable status. Returns whether what the view shows
    /// changed; a disabled instrument keeps the fault text visible.
    pub fn poll(&mut self) -> bool {
        let status = self.session.as_ref().map(|session| session.host.status());
        if status == self.status {
            return false;
        }
        if let Some(status) = &status
            && status.state == LiveState::Disabled
        {
            self.error = status.fault.clone();
        }
        self.status = status;
        true
    }

    /// One-line status for a panel, matching the MIDI lane's style.
    pub fn status_line(&self) -> String {
        let Some(status) = &self.status else {
            return "LIVE · no instrument mounted".into();
        };
        let plugin = self
            .session
            .as_ref()
            .map_or("live instrument", |session| session.plugin.as_str());
        let mut line = format!(
            "LIVE · {plugin} · {:?} · {} voice(s) · underruns {} frame(s)",
            status.state, status.voices, status.counters.underrun_frames
        );
        if let Some(fault) = &status.fault {
            line.push_str(" · ");
            line.push_str(fault);
        }
        line
    }
}

/// Dispatch helpers called by the MIDI input sink (the existing input path),
/// so there is exactly one scheduler: the backend's [`LiveSender`].
pub fn poll(s: &mut State) -> bool {
    s.live.poll()
}

pub fn held_start(s: &mut State, on: &NoteOn) {
    let _ = s
        .live
        .note_on(on.id, on.key.channel, on.key.pitch, on.velocity);
}

pub fn held_end(s: &mut State, note: &Note) {
    let _ = s.live.note_off(note.on().id);
}

pub fn control(s: &mut State, channel: Channel, action: ControlIntent) {
    match action {
        ControlIntent::Sustain(down) => {
            let _ = s.live.sustain(channel, down);
        }
        ControlIntent::Panic => {
            let _ = s.live.panic();
        }
        // The controller ends released notes through `HeldEnd`.
        ControlIntent::AllNotesOff => {}
    }
}

pub fn panic(s: &mut State) {
    let _ = s.live.panic();
}

/// The current session's status, for tests and the (future) panel.
pub fn status(s: &State) -> Option<LiveStatus> {
    s.live.status()
}

/// Resolve a helper path for the preferences editor without mounting it.
pub fn resolve_helper(root: Option<&Path>) -> Result<HelperCommand, String> {
    let paths = Resolver::current().map_err(|error| error.to_string())?;
    helper::resolve(&paths, root)
}

/// The machine resource the helper path belongs to, for settings UIs.
pub const RESOURCE: Resource = Resource::ClapLive;
