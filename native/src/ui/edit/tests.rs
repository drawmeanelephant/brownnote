#![allow(clippy::unwrap_used)]
use super::*;
use brownnote_native::performance::{LoopBars, MusicalPosition, OriginalTime, Tempo};
use rune::{Cx, Env, Key, KeyEvent, Mods, Runtime, Tx};

struct App {
    edit: Editor,
    takes: Takes,
}

fn access(s: &mut App) -> (&mut Editor, &mut Takes) {
    (&mut s.edit, &mut s.takes)
}

fn read(s: &App) -> (&Editor, &Takes) {
    (&s.edit, &s.takes)
}

fn view(s: &App, _: &mut Cx<App>) -> El {
    col().w_full().h_full().child(panel(s, read, access))
}

fn app_poll(s: &mut App, _: f64, _: &mut Tx) -> bool {
    poll(s, access)
}

fn runtime(kind: SceneKind) -> Runtime<App> {
    let mut app = App {
        edit: Editor::default(),
        takes: Takes::new(LoopBars::try_from(8).unwrap(), TakeId(700)),
    };
    scene(&mut app.edit, &mut app.takes, kind).unwrap();
    let mut rt = Runtime::new(app, view, (820.0, 600.0), Env::new(true, 2.0));
    rt.poll = Some(app_poll);
    rt.set_full_keyboard_access(true);
    rt.frame(0.0);
    rt
}

fn key(rt: &mut Runtime<App>, key: Key) {
    rt.key(KeyEvent {
        key,
        mods: Mods::default(),
    });
    rt.frame(0.0);
}

fn focus(rt: &mut Runtime<App>, target: &str) {
    for _ in 0..40 {
        if rt
            .elements()
            .iter()
            .any(|e| e.key.as_deref() == Some(target) && e.focused)
        {
            return;
        }
        key(rt, Key::Tab);
    }
    panic!("Tab never reached {target}");
}

fn press(rt: &mut Runtime<App>, target: &str) {
    focus(rt, target);
    key(rt, Key::Text(" ".into()));
}

fn event(sequence: u64) -> EventId {
    EventId {
        take: TakeId(700),
        sequence,
    }
}

fn effective(rt: &Runtime<App>, id: EventId) -> brownnote_native::performance::EffectiveNote {
    rt.state
        .edit
        .model
        .effective_take_notes(&rt.state.takes, id.take)
        .unwrap()
        .into_iter()
        .find(|note| note.id == id)
        .unwrap()
}

fn label(rt: &Runtime<App>, target: &str) -> String {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(target))
        .unwrap()
        .label
        .unwrap()
}

fn enabled(rt: &Runtime<App>, target: &str) -> bool {
    rt.elements()
        .iter()
        .find(|e| e.key.as_deref() == Some(target))
        .unwrap()
        .focusable
}

#[test]
fn keyboard_controls_edit_effective_events_and_undo_redo_in_order() {
    let mut rt = runtime(SceneKind::Selected);
    let originals = rt.state.takes.take(TakeId(700)).unwrap().notes().to_vec();
    let unaffected = effective(&rt, event(1));
    assert!(!enabled(&rt, "edit-undo"));
    assert!(!enabled(&rt, "edit-redo"));

    press(&mut rt, "edit-quantize");
    assert_eq!(
        rt.state.edit.model.quantization(TakeId(700)),
        Quantization::Sixteenth
    );
    assert_eq!(
        effective(&rt, event(0)).start,
        MusicalPosition::from_original(OriginalTime::from_nanos(0), Tempo::default())
    );
    focus(&mut rt, "edit-quantize");
    key(&mut rt, Key::Text(" ".into()));
    assert_eq!(
        rt.state.edit.model.quantization(TakeId(700)),
        Quantization::Off
    );
    assert_eq!(
        effective(&rt, event(0)).start,
        MusicalPosition::from_original(OriginalTime::from_nanos(10_000_000), Tempo::default())
    );

    press(&mut rt, "edit-left");
    assert_eq!(
        effective(&rt, event(0)).start,
        MusicalPosition::from_original(OriginalTime::from_nanos(9_000_000), Tempo::default())
    );
    press(&mut rt, "edit-right");
    assert_eq!(
        effective(&rt, event(0)).start,
        MusicalPosition::from_original(OriginalTime::from_nanos(10_000_000), Tempo::default())
    );
    press(&mut rt, "edit-velocity-up");
    assert_eq!(effective(&rt, event(0)).velocity.value(), 91);
    press(&mut rt, "edit-velocity-down");
    assert_eq!(effective(&rt, event(0)).velocity.value(), 90);
    press(&mut rt, "edit-delete");
    assert!(label(&rt, "edit-selection").contains("DELETED"));
    for target in [
        "edit-left",
        "edit-right",
        "edit-delete",
        "edit-velocity",
        "edit-velocity-up",
    ] {
        assert!(!enabled(&rt, target), "{target}");
    }
    assert_eq!(
        rt.state
            .edit
            .model
            .effective_take_notes(&rt.state.takes, TakeId(700))
            .unwrap()
            .len(),
        1
    );
    press(&mut rt, "edit-undo");
    assert_eq!(effective(&rt, event(0)).velocity.value(), 90);
    press(&mut rt, "edit-redo");
    assert_eq!(
        rt.state
            .edit
            .model
            .effective_take_notes(&rt.state.takes, TakeId(700))
            .unwrap()
            .len(),
        1
    );
    press(&mut rt, "edit-undo");
    assert_eq!(effective(&rt, event(1)), unaffected);
    assert_eq!(rt.state.takes.take(TakeId(700)).unwrap().notes(), originals);
    press(&mut rt, "edit-right");
    assert!(!enabled(&rt, "edit-redo"), "a fresh edit invalidates redo");
    assert!(label(&rt, "edit-history").contains("/ 100"));
}

#[test]
fn take_and_event_navigation_mute_remove_and_restore_use_the_registry() {
    let mut rt = runtime(SceneKind::Selected);
    press(&mut rt, "edit-event-next");
    assert_eq!(rt.state.edit.selected_event, Some(event(1)));
    assert!(!enabled(&rt, "edit-event-next"));
    press(&mut rt, "edit-event-prev");
    assert_eq!(rt.state.edit.selected_event, Some(event(0)));
    press(&mut rt, "edit-take-next");
    assert_eq!(rt.state.takes.selected(), Some(TakeId(701)));
    assert_eq!(rt.state.edit.selected_event.unwrap().take, TakeId(701));
    press(&mut rt, "edit-take-prev");
    assert_eq!(
        rt.state.edit.model.undo_depth(),
        0,
        "navigation is not an edit"
    );
    press(&mut rt, "edit-mute");
    assert!(rt.state.takes.take(TakeId(700)).unwrap().muted());
    assert!(label(&rt, "edit-selection").contains("MUTED"));
    assert_eq!(
        rt.state
            .edit
            .model
            .effective_notes(&rt.state.takes)
            .unwrap()
            .len(),
        2
    );
    press(&mut rt, "edit-undo");
    assert!(!rt.state.takes.take(TakeId(700)).unwrap().muted());
    press(&mut rt, "edit-redo");
    assert!(rt.state.takes.take(TakeId(700)).unwrap().muted());
    press(&mut rt, "edit-remove");
    assert!(
        rt.state
            .takes
            .take(TakeId(700))
            .unwrap()
            .removal()
            .is_some()
    );
    assert_eq!(rt.state.takes.selected(), Some(TakeId(701)));
    press(&mut rt, "edit-undo");
    assert!(
        rt.state
            .takes
            .take(TakeId(700))
            .unwrap()
            .removal()
            .is_none()
    );
    press(&mut rt, "edit-take-prev");
    assert!(rt.state.takes.take(TakeId(700)).unwrap().muted());
    assert_eq!(rt.state.takes.take(TakeId(700)).unwrap().notes().len(), 2);
}

#[test]
fn slider_keyboard_steps_and_limits_do_not_record_clamped_noops() {
    let mut rt = runtime(SceneKind::Selected);
    focus(&mut rt, "edit-velocity");
    key(&mut rt, Key::Right);
    assert_eq!(effective(&rt, event(0)).velocity.value(), 91);
    key(&mut rt, Key::Left);
    assert_eq!(effective(&rt, event(0)).velocity.value(), 90);
    key(&mut rt, Key::Home);
    assert_eq!(effective(&rt, event(0)).velocity.value(), 1);
    let depth = rt.state.edit.model.undo_depth();
    key(&mut rt, Key::Left);
    key(&mut rt, Key::Home);
    assert_eq!(rt.state.edit.model.undo_depth(), depth);
    assert!(!enabled(&rt, "edit-velocity-down"));
    key(&mut rt, Key::End);
    assert_eq!(effective(&rt, event(0)).velocity.value(), 127);
    let depth = rt.state.edit.model.undo_depth();
    key(&mut rt, Key::Right);
    assert_eq!(rt.state.edit.model.undo_depth(), depth);
    assert!(!enabled(&rt, "edit-velocity-up"));
    press(&mut rt, "edit-undo");
    assert_eq!(effective(&rt, event(0)).velocity.value(), 1);
}

fn start_drag(rt: &mut Runtime<App>) -> (f32, f32) {
    let ([x, y, w, h], _) = rt.presented("edit-velocity").unwrap();
    rt.pointer_down(x + w - 3.0, y + h / 2.0);
    rt.frame(0.0);
    rt.pointer_move(x + w * 0.4, y + h / 2.0);
    rt.frame(0.0);
    rt.pointer_move(x + w * 0.6, y + h / 2.0);
    rt.frame(0.0);
    (x + w * 0.6, y + h / 2.0)
}

#[test]
fn continuous_pointer_drag_is_one_undo_step_and_blocks_unrelated_actions() {
    let mut rt = runtime(SceneKind::Selected);
    press(&mut rt, "edit-velocity-up");
    press(&mut rt, "edit-undo");
    let (x, y) = start_drag(&mut rt);
    assert_ne!(effective(&rt, event(0)).velocity.value(), 90);
    assert_eq!(rt.state.edit.model.undo_depth(), 0);
    assert!(rt.state.edit.model.drag_open());
    for target in [
        "edit-undo",
        "edit-take-next",
        "edit-event-next",
        "edit-mute",
        "edit-velocity-up",
    ] {
        assert!(!enabled(&rt, target), "{target}");
    }
    // A key still routed to the captured slider must not enter the drag.
    key(&mut rt, Key::Right);
    rt.pointer_up(x, y);
    rt.frame(0.0);
    assert_eq!(rt.state.edit.model.undo_depth(), 1);
    assert!(!rt.state.edit.model.drag_open());
    assert!(!rt.state.edit.model.can_redo());
    let after = effective(&rt, event(0));
    press(&mut rt, "edit-undo");
    assert_eq!(effective(&rt, event(0)).velocity.value(), 90);
    press(&mut rt, "edit-redo");
    assert_eq!(effective(&rt, event(0)), after);
}

#[test]
fn escape_and_focus_loss_cancel_drag_and_preserve_redo_including_late_release() {
    for escape in [true, false] {
        let mut rt = runtime(SceneKind::Selected);
        press(&mut rt, "edit-velocity-up");
        press(&mut rt, "edit-undo");
        let (x, y) = start_drag(&mut rt);
        assert!(rt.state.edit.model.can_redo());
        if escape {
            key(&mut rt, Key::Escape);
        } else {
            rt.window_focus(true);
            rt.window_focus(false);
            rt.frame(0.0);
        }
        assert_eq!(effective(&rt, event(0)).velocity.value(), 90);
        assert!(!rt.state.edit.model.drag_open());
        assert_eq!(rt.state.edit.model.undo_depth(), 0);
        assert!(rt.state.edit.model.can_redo());
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert_eq!(rt.state.edit.model.undo_depth(), 0);
        press(&mut rt, "edit-redo");
        assert_eq!(effective(&rt, event(0)).velocity.value(), 91);
    }
}

#[test]
fn external_selection_and_registry_changes_reconcile_without_stale_edits() {
    let mut rt = runtime(SceneKind::Selected);
    let (x, y) = start_drag(&mut rt);
    rt.state.takes.select(TakeId(701)).unwrap();
    assert!(poll(&mut rt.state, access));
    assert!(!rt.state.edit.model.drag_open());
    assert_eq!(effective(&rt, event(0)).velocity.value(), 90);
    assert_eq!(rt.state.edit.selected_event.unwrap().take, TakeId(701));
    rt.pointer_up(x, y);
    rt.frame(0.0);
    assert_eq!(rt.state.edit.model.undo_depth(), 0);
    rt.state.edit.selected_event = Some(EventId {
        take: TakeId(999),
        sequence: 5,
    });
    assert!(poll(&mut rt.state, access));
    assert_eq!(rt.state.edit.selected_event.unwrap().take, TakeId(701));
    assert!(!poll(&mut rt.state, access), "idle polling must not spin");
    rt.state.takes = Takes::new(LoopBars::try_from(1).unwrap(), TakeId(900));
    assert!(poll(&mut rt.state, access));
    assert_eq!(rt.state.edit.selected_event, None);
    assert!(!poll(&mut rt.state, access));
}

#[test]
fn stationary_and_return_to_start_drags_leave_history_and_redo_unchanged() {
    for move_away in [false, true] {
        let mut rt = runtime(SceneKind::Selected);
        press(&mut rt, "edit-velocity-up");
        press(&mut rt, "edit-undo");
        let ([x, y, w, h], _) = rt.presented("edit-velocity").unwrap();
        let (knob, center_y) = (x + 7.0 + 89.0 / 126.0 * (w - 14.0), y + h / 2.0);
        rt.pointer_down(knob, center_y);
        rt.frame(0.0);
        if move_away {
            rt.pointer_move(x + 10.0, center_y);
            rt.frame(0.0);
            assert_ne!(effective(&rt, event(0)).velocity.value(), 90);
            rt.pointer_move(knob, center_y);
            rt.frame(0.0);
        }
        rt.pointer_up(knob, center_y);
        rt.frame(0.0);
        assert_eq!(effective(&rt, event(0)).velocity.value(), 90);
        assert!(!rt.state.edit.model.drag_open());
        assert_eq!(rt.state.edit.model.undo_depth(), 0);
        assert!(rt.state.edit.model.can_redo());
        press(&mut rt, "edit-redo");
        assert_eq!(effective(&rt, event(0)).velocity.value(), 91);
    }
}

#[test]
fn shortening_loop_hides_selected_event_and_removal_cancels_inflight_drag() {
    use brownnote_native::{
        performance::{Channel, NoteKey, Pitch, SourceId, Target},
        takes::PassOptions,
    };
    let mut rt = runtime(SceneKind::Selected);
    let take = rt.state.takes.arm(PassOptions::default()).unwrap();
    rt.state.takes.start(take).unwrap();
    let note_key = NoteKey {
        source: SourceId(42),
        channel: Channel::try_from(0).unwrap(),
        pitch: Pitch::try_from(70).unwrap(),
    };
    let id = rt
        .state
        .takes
        .note_on(
            take,
            Target::Pad(crate::ui::media::pad_id(0)),
            note_key,
            Velocity::try_from(100).unwrap(),
            OriginalTime::from_nanos(3_000_000_000),
        )
        .unwrap();
    rt.state
        .takes
        .note_off(take, note_key, OriginalTime::from_nanos(3_100_000_000))
        .unwrap();
    rt.state.takes.finish(take).unwrap();
    rt.frame(0.0);
    assert_eq!(rt.state.edit.selected_event, Some(id));
    rt.state
        .takes
        .set_loop_bars(LoopBars::try_from(1).unwrap())
        .unwrap();
    assert!(poll(&mut rt.state, access));
    rt.invalidate();
    rt.frame(0.0);
    assert_eq!(rt.state.edit.selected_event, None);
    assert!(label(&rt, "edit-selection").contains("no events in this loop"));
    assert!(!enabled(&rt, "edit-delete"));
    rt.state
        .takes
        .set_loop_bars(LoopBars::try_from(8).unwrap())
        .unwrap();
    assert!(poll(&mut rt.state, access));
    rt.invalidate();
    rt.frame(0.0);
    assert_eq!(rt.state.edit.selected_event, Some(id));
    let (x, y) = start_drag(&mut rt);
    rt.state.takes.remove(take).unwrap();
    assert!(poll(&mut rt.state, access));
    assert!(!rt.state.edit.model.drag_open());
    assert_eq!(effective(&rt, id).velocity.value(), 100);
    rt.pointer_up(x, y);
    rt.frame(0.0);
    assert_eq!(rt.state.edit.model.undo_depth(), 0);
}

#[test]
fn empty_muted_removed_and_error_scenes_have_accessible_bounded_controls() {
    for kind in [
        SceneKind::Empty,
        SceneKind::Selected,
        SceneKind::Muted,
        SceneKind::Removed,
        SceneKind::Error,
    ] {
        let mut rt = runtime(kind);
        for (target, role) in [
            ("edit-panel", "group"),
            ("edit-selection", "status"),
            ("edit-history", "status"),
            ("edit-undo", "button"),
            ("edit-quantize", "checkbox"),
        ] {
            let element = rt
                .elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some(target))
                .unwrap();
            assert_eq!(element.role, role, "{target}");
            assert!(!element.label.unwrap().is_empty());
        }
        for target in [
            "edit-take-next",
            "edit-event-next",
            "edit-quantize",
            "edit-mute",
            "edit-remove",
            "edit-left",
            "edit-right",
            "edit-delete",
            "edit-velocity-up",
            "edit-velocity",
            "edit-undo",
            "edit-redo",
        ] {
            if rt.find(target).is_some() && enabled(&rt, target) {
                focus(&mut rt, target);
                let ([x, y, w, h], _) = rt.presented(target).unwrap();
                assert!(
                    w > 0.0 && h > 0.0 && x >= 0.0 && y >= 0.0 && x + w <= 820.1 && y + h <= 600.1,
                    "{target}: {x}, {y}, {w}, {h}"
                );
            }
        }
    }
    let mut rt = runtime(SceneKind::Error);
    assert!(label(&rt, "edit-error").contains("UnknownEvent"));
    press(&mut rt, "edit-right");
    assert!(rt.find("edit-error").is_none());
    for kind in [SceneKind::Empty, SceneKind::Removed] {
        let mut rt = runtime(kind);
        assert!(label(&rt, "edit-selection").contains("No committed takes"));
        for target in [
            "edit-quantize",
            "edit-left",
            "edit-right",
            "edit-delete",
            "edit-mute",
            "edit-remove",
        ] {
            assert!(!enabled(&rt, target), "{target}");
        }
        if enabled(&rt, "edit-undo") {
            press(&mut rt, "edit-undo");
            assert!(enabled(&rt, "edit-quantize"));
        }
    }
}
