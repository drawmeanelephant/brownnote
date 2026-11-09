#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used)]
//! The sampled-instrument backend (D18, #108) against the generated, MIT
//! mechanics fixture from the sampled-instrument spike. The fixture is one
//! triangle-wave preset (bank 0, program 0, root A4, 250 ms release); it
//! proves mechanics only and says nothing about musical quality.
use brownnote_native::instrument::{
    Font, InstrumentError, MAX_SOURCE_BYTES, Preset, RENDER_RATE, RenderKey,
};
use brownnote_native::performance::{EventId, TakeId};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    io::Cursor,
};

struct Counting;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static FREES: Cell<usize> = const { Cell::new(0) };
}

fn count(counter: &'static std::thread::LocalKey<Cell<usize>>) {
    let _ = TRACK.try_with(|track| {
        if track.get() {
            let _ = counter.try_with(|n| n.set(n.get() + 1));
        }
    });
}

// SAFETY: every allocation is forwarded unchanged to System; TLS only counts calls.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(&ALLOCS);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count(&FREES);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(&ALLOCS);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const FIXTURE: &[u8] = include_bytes!("../docs/spikes/sampled-instrument/fixtures/mechanics.sf2");
const RATE: u32 = 48_000;
const A4: u8 = 69;

fn id(sequence: u64) -> EventId {
    EventId {
        take: TakeId(7),
        sequence,
    }
}

fn font() -> Font {
    Font::prepare(FIXTURE, None).expect("mechanics fixture")
}

/// Peak absolute sample over `frames` frames of the player's output.
fn peak(player: &mut brownnote_native::instrument::Player, frames: usize) -> f32 {
    let (mut l, mut r) = ([0.0_f32; 480], [0.0_f32; 480]);
    let mut peak = 0.0_f32;
    let mut left = frames;
    while left > 0 {
        let n = left.min(480);
        player.render(&mut l[..n], &mut r[..n]);
        for v in l[..n].iter().chain(&r[..n]) {
            peak = peak.max(v.abs());
        }
        left -= n;
    }
    peak
}

#[test]
fn fixture_prepares_its_one_melodic_preset_and_refuses_missing_presets() {
    let font = font();
    assert_eq!(
        font.preset(),
        Preset {
            bank: 0,
            program: 0
        }
    );
    assert_eq!(font.presets().len(), 1);
    let missing = Preset {
        bank: 0,
        program: 5,
    };
    assert_eq!(
        Font::prepare(FIXTURE, Some(missing)).unwrap_err(),
        InstrumentError::PresetMissing(missing)
    );
    assert!(
        Font::prepare(
            FIXTURE,
            Some(Preset {
                bank: 0,
                program: 0
            })
        )
        .is_ok()
    );
}

#[test]
fn malformed_sources_are_refused_before_the_backend_sizes_anything() {
    assert_eq!(
        Font::prepare(b"RIFF\0\0\0\0WAVE", None).unwrap_err(),
        InstrumentError::NotSoundFont
    );
    let truncated = &FIXTURE[..FIXTURE.len() / 2];
    assert_eq!(
        Font::prepare(truncated, None).unwrap_err(),
        InstrumentError::Truncated
    );
    // A chunk that claims far more bytes than the file brings.
    let mut lying = FIXTURE.to_vec();
    lying[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        Font::prepare(&lying, None).unwrap_err(),
        InstrumentError::Truncated
    );
    let huge = vec![0_u8; MAX_SOURCE_BYTES as usize + 1];
    assert!(matches!(
        Font::prepare(&huge, None),
        Err(InstrumentError::TooLarge(_))
    ));
}

#[test]
fn same_pitch_overlaps_end_by_identity_oldest_first() {
    let font = font();
    let mut player = font.player(RATE, 0.5).unwrap();
    player.note_on(id(1), A4, 100);
    assert!(peak(&mut player, 4_800) > 0.01);
    player.note_on(id(2), A4, 100);
    assert!(peak(&mut player, 4_800) > 0.01);
    // Releasing the first copy leaves the second one sounding long past the
    // fixture's 250 ms release: the backend's channel/key note-off would have
    // released both.
    player.note_off(id(1));
    assert_eq!(player.held_count(), 1);
    let _ = peak(&mut player, RATE as usize / 2);
    assert!(peak(&mut player, 4_800) > 0.01, "second copy was released");
    player.note_off(id(2));
    let _ = peak(&mut player, RATE as usize / 2);
    assert_eq!(peak(&mut player, 4_800), 0.0, "release did not finish");
    assert_eq!(player.held_count(), 0);
    assert_eq!((player.stolen, player.dropped), (0, 0));
}

#[test]
fn a_replayed_id_held_twice_releases_its_oldest_copy_first() {
    let font = font();
    let mut player = font.player(RATE, 0.5).unwrap();
    player.note_on(id(1), A4, 100);
    player.note_on(id(1), A4, 100);
    player.note_off(id(1));
    assert_eq!(player.held_count(), 1);
    player.note_off(id(1));
    player.note_off(id(1));
    assert_eq!(player.held_count(), 0);
}

#[test]
fn velocity_changes_level() {
    let font = font();
    let level = |velocity| {
        let mut player = font.player(RATE, 0.5).unwrap();
        player.note_on(id(1), A4, velocity);
        peak(&mut player, 4_800)
    };
    let (soft, loud) = (level(30), level(127));
    assert!(soft > 0.0 && loud > soft * 1.5, "soft {soft} loud {loud}");
}

#[test]
fn panic_fades_to_silence_and_forgets_held_identities() {
    let font = font();
    let mut player = font.player(RATE, 0.5).unwrap();
    for n in 0..8 {
        player.note_on(id(n), 48 + n as u8 * 3, 110);
    }
    assert!(peak(&mut player, 4_800) > 0.01);
    player.panic(480);
    assert_eq!(player.held_count(), 0);
    let _ = peak(&mut player, 480);
    assert_eq!(peak(&mut player, 4_800), 0.0, "tail survived the panic");
    // A note-on during a later fade starts once the fade ends.
    player.note_on(id(20), A4, 100);
    player.panic(960);
    player.note_on(id(21), A4, 100);
    let _ = peak(&mut player, 960);
    assert_eq!(player.held_count(), 1);
    assert!(peak(&mut player, 4_800) > 0.01);
}

#[test]
fn renders_are_deterministic_stereo_float_with_the_release_tail() {
    let font = font();
    let key = RenderKey {
        pitch: A4,
        velocity: 100,
        gate_frames: RENDER_RATE as u64 / 10,
    };
    let first = font.render(key, 0.5).unwrap();
    assert_eq!(first, font.render(key, 0.5).unwrap());
    let reader = hound::WavReader::new(Cursor::new(&first)).unwrap();
    let spec = reader.spec();
    assert_eq!(
        (spec.channels, spec.sample_rate, spec.bits_per_sample),
        (2, RENDER_RATE, 32)
    );
    let frames = reader.duration() as u64;
    // Gate plus most of the 250 ms release, and not the 20 s tail limit.
    assert!(
        frames > key.gate_frames + RENDER_RATE as u64 / 8,
        "{frames}"
    );
    assert!(frames < key.gate_frames + RENDER_RATE as u64, "{frames}");
    let softer = font
        .render(
            RenderKey {
                velocity: 40,
                ..key
            },
            0.5,
        )
        .unwrap();
    assert_ne!(first, softer);
}

#[test]
fn callback_player_allocates_and_frees_nothing_after_construction() {
    let font = font();
    for block in [31_usize, 64, 256] {
        let mut player = font.player(RATE, 0.5).unwrap();
        let (mut l, mut r) = (vec![0.0_f32; block], vec![0.0_f32; block]);
        ALLOCS.with(|n| n.set(0));
        FREES.with(|n| n.set(0));
        TRACK.with(|t| t.set(true));
        let mut sequence = 0;
        for cycle in 0..400_u64 {
            // Saturate held capacity and same-pitch channels to force steals.
            for _ in 0..3 {
                sequence += 1;
                player.note_on(id(sequence), A4 + (sequence % 5) as u8, 100);
            }
            if cycle % 2 == 0 {
                player.note_off(id(sequence.saturating_sub(7)));
            }
            if cycle % 97 == 0 {
                player.panic(block as u32 * 2);
            }
            player.render(&mut l, &mut r);
        }
        TRACK.with(|t| t.set(false));
        assert!(player.stolen > 0, "capacity was never exercised");
        assert_eq!(ALLOCS.with(Cell::get), 0, "allocations at block {block}");
        assert_eq!(FREES.with(Cell::get), 0, "frees at block {block}");
    }
}
