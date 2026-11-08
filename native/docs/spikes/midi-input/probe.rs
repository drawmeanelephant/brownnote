use midir::{MidiInput, MidiInputConnection, MidiInputPort, MidiOutput, os::unix::VirtualOutput};
use serde_json::{Value, json};
use std::{
    error::Error,
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Sample = (u64, Vec<u8>, u128);

fn connect(
    port: &MidiInputPort,
    tx: SyncSender<Sample>,
    start: Instant,
) -> Result<MidiInputConnection<()>> {
    let input = MidiInput::new("brownnote-probe-input")?;
    Ok(input.connect(
        port,
        "brownnote-probe-connection",
        move |timestamp, bytes, _| {
            // This standalone probe is not the production bounded handoff.
            let _ = tx.try_send((timestamp, bytes.to_vec(), start.elapsed().as_nanos()));
        },
        (),
    )?)
}

fn receive(rx: &Receiver<Sample>, expected: &[u8]) -> Result<Value> {
    let (timestamp, bytes, received) = rx.recv_timeout(Duration::from_secs(2))?;
    if bytes != expected {
        return Err(format!("unexpected probe payload: {bytes:?}").into());
    }
    Ok(json!({
        "backend_timestamp_us": timestamp,
        "host_elapsed_ns": received,
        "bytes": bytes,
    }))
}

fn silence(rx: &Receiver<Sample>) -> Result<()> {
    match rx.recv_timeout(Duration::from_millis(100)) {
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
        other => Err(format!("expected bounded silent interval, got {other:?}").into()),
    }
}

fn run() -> Result<Value> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err("this feasibility probe is scoped to macOS arm64".into());
    }
    let start = Instant::now();
    let observer = MidiInput::new("brownnote-probe-enumeration")?;
    let initial = observer.ports();
    // Do not log user port names/IDs or connect to any of them.
    let iac_count = initial
        .iter()
        .filter(|port| {
            observer
                .port_name(port)
                .is_ok_and(|name| name.contains("IAC"))
        })
        .count();
    let name = format!("brownnote-owned-probe-{}", std::process::id());
    let (tx, rx) = mpsc::sync_channel(32);
    let mut source = MidiOutput::new("brownnote-probe-source")?.create_virtual(&name)?;
    let original_port = observer
        .ports()
        .into_iter()
        .find(|port| {
            !initial.contains(port) && observer.port_name(port).is_ok_and(|value| value == name)
        })
        .ok_or("owned virtual source missing")?;
    let original_id = original_port.id();
    let connection = connect(&original_port, tx.clone(), start)?;
    source.send(&[0x91, 40, 1])?;
    let first = receive(&rx, &[0x91, 40, 1])?;
    silence(&rx)?;
    source.send(&[0x91, 40, 127])?;
    let second = receive(&rx, &[0x91, 40, 127])?;

    let _ = connection.close();
    source.send(&[0x81, 40, 0])?;
    silence(&rx)?;
    let connection = connect(&original_port, tx.clone(), start)?;
    source.send(&[0x91, 40, 0])?;
    let reconnected = receive(&rx, &[0x91, 40, 0])?;

    let _ = source.close();
    let removed = observer.find_port_by_id(&original_id).is_none();
    // midir has no disconnect callback; silence alone is not loss detection.
    silence(&rx)?;
    let stale_name_error = observer.port_name(&original_port).is_err();
    let _ = connection.close();
    let mut replacement = MidiOutput::new("brownnote-probe-replacement")?.create_virtual(&name)?;
    let replacement_port = observer
        .ports()
        .into_iter()
        .find(|port| {
            !initial.contains(port) && observer.port_name(port).is_ok_and(|value| value == name)
        })
        .ok_or("replacement source missing")?;
    let replacement_id_changed = replacement_port.id() != original_id;
    let connection = connect(&replacement_port, tx.clone(), start)?;
    replacement.send(&[0x91, 40, 64])?;
    let recreated = receive(&rx, &[0x91, 40, 64])?;
    let _ = connection.close();
    let _ = replacement.close();
    let cleaned_up = observer.find_port_by_id(&replacement_port.id()).is_none();
    if !removed || !cleaned_up {
        return Err("owned source removal was not observed".into());
    }
    let stamps = [&first, &second, &reconnected, &recreated];
    if stamps.windows(2).any(|pair| {
        pair[0]["backend_timestamp_us"].as_u64() >= pair[1]["backend_timestamp_us"].as_u64()
    }) {
        return Err("probe timestamps were not increasing across reconnect".into());
    }
    Ok(json!({
        "backend_candidate": "midir =0.11.1 (CoreMIDI, default features)",
        "platform": "aarch64-apple-darwin",
        "existing_input_count": initial.len(),
        "existing_iac_name_count": iac_count,
        "existing_ports_opened": 0,
        "samples": [first, second, reconnected, recreated],
        "explicit_close_stopped_callbacks": true,
        "source_removal_enumerated": removed,
        "stale_port_name_error": stale_name_error,
        "same_name_recreation_changed_port_id": replacement_id_changed,
        "owned_endpoints_removed": cleaned_up,
        "final_input_count": observer.port_count(),
        "scope": "owned CoreMIDI virtual source only; not IAC, controller, audio, or production input",
    }))
}

fn main() {
    match run() {
        Ok(evidence) => println!("{evidence:#}"),
        Err(error) => {
            eprintln!("MIDI feasibility probe failed: {error}");
            std::process::exit(1);
        }
    }
}
