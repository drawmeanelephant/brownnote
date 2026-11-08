//! Encoded fixtures for the mounted application, not fabricated persistent IDs.
use crate::ui::State;
use brownnote_native::{
    assets::ChokeIdentity,
    engine::{Command, SLOTS, Sample},
    import::{Imported, MediaPublisher, retain},
    kit::{Kit, encode_wav},
    project::PadId,
};

pub fn kit(value: f32, rate: u32, frames: usize) -> Kit {
    Kit::from_encoded(std::array::from_fn(|index| {
        let mut bytes = encode_wav(&Sample {
            data: vec![value; frames].into(),
            rate,
        })
        .unwrap();
        if index > 0 {
            // Distinct valid encoded files, with identical PCM as the original
            // fixtures. IDs describe bytes, not an invented hash of floats.
            bytes.extend_from_slice(b"JUNK");
            bytes.extend_from_slice(&4_u32.to_le_bytes());
            bytes.extend_from_slice(&(index as u32).to_le_bytes());
            let riff_bytes = (bytes.len() - 8) as u32;
            bytes[4..8].copy_from_slice(&riff_bytes.to_le_bytes());
        }
        Ok(bytes)
    }))
}

pub fn pad(index: usize) -> PadId {
    PadId::try_from(index as u8).unwrap()
}

pub fn slot(state: &State, index: usize) -> usize {
    state.assets.playback(pad(index)).unwrap().0
}

pub fn retained(state: &State, index: usize) -> Option<usize> {
    let id = state.assets.pad(pad(index)).assignment.as_ref()?.retained?;
    Some(state.assets.asset(id)?.slot)
}

pub fn source_slot(state: &State, index: usize) -> usize {
    state.assets.source(pad(index)).unwrap().slot
}

pub fn source(value: f32, frames: usize) -> Imported {
    kit(value, 8_000, frames).media[0].take().unwrap().imported
}

pub fn replace(state: &mut State, index: usize, imported: &Imported) {
    let bytes = encode_wav(&imported.sample).unwrap();
    state
        .media
        .owned
        .publish(&imported.asset.reference, &bytes)
        .unwrap();
    let ticket = state.assets.begin(pad(index));
    state
        .assets
        .replace_source(
            ticket,
            imported,
            ChokeIdentity::Asset(imported.asset.reference.id),
            |command| state.output.send(command),
        )
        .unwrap();
}

pub fn render(state: &mut State, index: usize, value: f32) {
    let imported = retain(
        Sample {
            data: vec![value; 8_000 * 30].into(),
            rate: 8_000,
        },
        "retained.wav".into(),
        state.media.owned.as_ref(),
    )
    .unwrap();
    let ticket = state.assets.begin(pad(index));
    state
        .assets
        .replace_retained(ticket, &imported, |command| state.output.send(command))
        .unwrap();
}

/// Occupy all slots through actual Load/Trigger/Unload admission. Live
/// assignments and long voices hold them, not fabricated slot-table entries.
pub fn fill_slots(state: &mut State, keep: usize) {
    let selected = state.selected;
    state.choke = false;
    state.output.send(Command::SetChoke(false));
    for index in 0..16 {
        if index != keep {
            replace(
                state,
                index,
                &source(0.01 + index as f32 * 0.001, 8_000 * 30),
            );
            render(state, index, 0.1 + index as f32 * 0.001);
            state.output.advance(0.001);
            state.poll_assets();
        }
    }
    let mut count = 0;
    while (0..SLOTS).any(|slot| state.assets.slots().is_empty(slot)) {
        state.trigger(15, 1.0);
        render(state, 15, 0.2 + count as f32 * 0.001);
        state.output.advance(0.001);
        state.poll_assets();
        count += 1;
        assert!(count < SLOTS, "could not hold the slot pool");
    }
    state.selected = selected;
}

pub fn retire_one(state: &mut State, index: usize) -> usize {
    render(state, index, 0.17);
    let previous = slot(state, index);
    state.trigger(index, 1.0);
    state.output.advance(0.001);
    render(state, index, 0.19);
    state.output.advance(0.001);
    state.poll_assets();
    assert!(!state.assets.slots().is_empty(previous));
    previous
}
