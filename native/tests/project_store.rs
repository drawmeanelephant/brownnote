#![allow(clippy::unwrap_used, clippy::expect_used)]
use brownnote_native::{
    engine::{Command, Slots},
    kit,
    output::Output,
    pedals::render::{Chain, Stage},
    project::{
        Asset, AssetId, AssetRef, Document, DocumentError, MediaKind, SampleAssignment, media,
        recovery::OpenChoice,
        store::{CommitState, FileSystem, GenerationId, MediaSource, Store, StoreError, SystemFs},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Cursor},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    a: Document,
    b: Document,
    sources_a: Vec<MediaSource>,
    sources_b: Vec<MediaSource>,
}

fn wav(value: i16) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    {
        let mut writer = hound::WavWriter::new(&mut output, spec).unwrap();
        for _ in 0..480 {
            writer.write_sample(value).unwrap();
        }
        writer.finalize().unwrap();
    }
    output.into_inner()
}

impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "brownnote-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let mut sources = Vec::new();
        let mut assets = Vec::new();
        for (index, value) in [2_000, 8_000, -12_000].into_iter().enumerate() {
            let bytes = wav(value);
            let reference = AssetRef::of(&bytes);
            let path = base.join(format!("source-{index}.wav"));
            fs::write(&path, bytes).unwrap();
            sources.push(MediaSource {
                id: reference.id,
                path,
            });
            assets.push(Asset {
                reference,
                kind: if index == 0 {
                    MediaKind::Source
                } else {
                    MediaKind::RetainedRender
                },
            });
        }
        let document = |rendered: usize, gain: f32| {
            let mut document = Document {
                assets: vec![assets[0].clone(), assets[rendered].clone()],
                ..Document::default()
            };
            for pad in &mut document.pads {
                pad.sample = Some(SampleAssignment {
                    source: assets[0].reference.id,
                    retained: Some(assets[rendered].reference.id),
                });
                pad.gain = gain;
                pad.chain = Chain {
                    stages: vec![Stage {
                        effect: "Tape Echo".into(),
                        pots: vec![("Time".into(), 42)],
                        mix: Some(80),
                    }],
                };
            }
            document
        };
        Self {
            root: base.join("project"),
            a: document(1, 0.7),
            b: document(2, 1.2),
            sources_a: sources[..2].to_vec(),
            // A's source is deliberately reused from project media.
            sources_b: vec![sources[2].clone()],
            base,
        }
    }
    fn seed(&self) -> GenerationId {
        Store::new(self.root.clone())
            .save(&self.a, &self.sources_a)
            .unwrap()
            .generation
    }
    fn candidate_sources(&self, existing: bool) -> Vec<MediaSource> {
        if existing {
            self.sources_b.clone()
        } else {
            vec![self.sources_a[0].clone(), self.sources_b[0].clone()]
        }
    }
    fn abandoned(&self) {
        let staging = self.root.join(".brownnote-store/staging");
        for name in [
            "document.tmp".to_owned(),
            "version.tmp".to_owned(),
            format!("media-{}.tmp", AssetId::of(b"abandoned")),
        ] {
            fs::write(staging.join(name), b"incomplete abandoned bytes").unwrap();
        }
        fs::write(staging.join("user-note.txt"), b"keep me").unwrap();
        fs::create_dir(staging.join("unknown-directory")).unwrap();
    }
    fn assert_state(&self, previous: Option<GenerationId>) {
        let loaded = if self.root.exists() {
            Store::new(self.root.clone()).inspect_committed().unwrap()
        } else {
            None
        };
        match loaded {
            Some(loaded) => {
                assert!(loaded.document == self.a || loaded.document == self.b);
                assert_eq!(loaded.bytes, loaded.document.encode().unwrap());
                assert!(media::verify_all(&self.root, &loaded.document.assets).is_empty());
                if previous.is_none() {
                    assert_eq!(loaded.document, self.b);
                }
            }
            None => assert!(previous.is_none()),
        }
        if let Some(previous) = previous {
            let old = Store::new(self.root.clone())
                .inspect_snapshot(previous)
                .unwrap();
            assert_eq!(old.document, self.a);
            assert_eq!(old.bytes, self.a.encode().unwrap());
            for asset in &self.a.assets {
                let original = self
                    .sources_a
                    .iter()
                    .find(|s| s.id == asset.reference.id)
                    .unwrap();
                assert_eq!(
                    fs::read(&original.path).unwrap(),
                    fs::read(self.root.join(asset.reference.id.relative_path())).unwrap()
                );
            }
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Event {
    kind: &'static str,
    path: PathBuf,
    source: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
enum Moment {
    Before,
    After,
    PartialWrite,
}

#[derive(Clone, Copy)]
enum Fault {
    At(usize, Moment),
    Publish,
}

#[derive(Default)]
struct FaultFs {
    inner: SystemFs,
    events: Vec<Event>,
    fault: Option<Fault>,
    fired: bool,
    race_import: bool,
}
struct Writer {
    inner: File,
    path: PathBuf,
}
fn injected() -> io::Error {
    io::Error::other("injected filesystem failure")
}
impl FaultFs {
    fn failing(index: usize, moment: Moment) -> Self {
        Self {
            fault: Some(Fault::At(index, moment)),
            ..Self::default()
        }
    }
    fn event(&mut self, kind: &'static str, path: &Path) -> Option<Moment> {
        let index = self.events.len();
        self.events.push(Event {
            kind,
            path: path.to_path_buf(),
            source: None,
        });
        let moment = match self.fault {
            Some(Fault::At(target, moment)) if index == target => Some(moment),
            Some(Fault::Publish) if kind == "publish" => Some(Moment::Before),
            _ => None,
        };
        self.fired |= moment.is_some();
        moment
    }
    fn run<T>(
        &mut self,
        kind: &'static str,
        path: &Path,
        operation: impl FnOnce(&mut SystemFs) -> io::Result<T>,
    ) -> io::Result<T> {
        let moment = self.event(kind, path);
        if matches!(moment, Some(Moment::Before)) {
            return Err(injected());
        }
        let result = operation(&mut self.inner);
        if matches!(moment, Some(Moment::After)) {
            return Err(injected());
        }
        result
    }
    fn moving(
        &mut self,
        kind: &'static str,
        from: &Path,
        to: &Path,
        operation: impl FnOnce(&mut SystemFs) -> io::Result<()>,
    ) -> io::Result<()> {
        let index = self.events.len();
        let result = self.run(kind, to, operation);
        self.events[index].source = Some(from.to_path_buf());
        result
    }
}

macro_rules! path_operation {
    ($name:ident, $result:ty) => {
        fn $name(&mut self, path: &Path) -> io::Result<$result> {
            self.run(stringify!($name), path, |fs| fs.$name(path))
        }
    };
}
impl FileSystem for FaultFs {
    type Writer = Writer;
    type Lock = File;
    path_operation!(ensure_dir, ());
    path_operation!(check_dir, ());
    path_operation!(lock, File);
    path_operation!(list, Vec<PathBuf>);
    path_operation!(read, Vec<u8>);
    path_operation!(sync_existing, ());
    path_operation!(sync_dir, ());
    path_operation!(remove, ());
    fn create(&mut self, path: &Path) -> io::Result<Writer> {
        self.run("create", path, |fs| fs.create(path))
            .map(|inner| Writer {
                inner,
                path: path.to_path_buf(),
            })
    }
    fn write(&mut self, file: &mut Writer, bytes: &[u8]) -> io::Result<()> {
        match self.event("write", &file.path) {
            Some(Moment::Before) => Err(injected()),
            Some(Moment::PartialWrite) => {
                self.inner
                    .write(&mut file.inner, &bytes[..bytes.len() / 2])?;
                Err(injected())
            }
            Some(Moment::After) => {
                self.inner.write(&mut file.inner, bytes)?;
                Err(injected())
            }
            None => self.inner.write(&mut file.inner, bytes),
        }
    }
    fn flush(&mut self, file: &mut Writer) -> io::Result<()> {
        self.run("flush", &file.path, |fs| fs.flush(&mut file.inner))
    }
    fn sync(&mut self, file: &Writer) -> io::Result<()> {
        self.run("sync", &file.path, |fs| fs.sync(&file.inner))
    }
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        self.moving("rename", from, to, |fs| fs.rename(from, to))
    }
    fn link(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        let race = self.race_import && to.parent().is_some_and(|path| path.ends_with("media"));
        self.moving("link", from, to, |fs| {
            if race {
                // Another immutable publisher wins just before our no-replace link.
                fs.link(from, to)?;
            }
            fs.link(from, to)
        })
    }
    fn publish(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        self.moving("publish", from, to, |fs| fs.publish(from, to))
    }
}

// Independently audit D5, so deleting a required sync cannot make the matrix
// pass merely by shrinking the set of operations it enumerates.
fn assert_sync_contract(events: &[Event]) {
    let mut files = BTreeMap::<PathBuf, (bool, bool)>::new(); // flushed, synced
    let mut dirty_directories = BTreeSet::new();
    let dirty_parent = |set: &mut BTreeSet<PathBuf>, path: &Path| {
        set.insert(path.parent().unwrap().to_path_buf());
    };
    let mut commits = 0;
    for event in events {
        let path = &event.path;
        match event.kind {
            "ensure_dir" | "lock" => dirty_parent(&mut dirty_directories, path),
            "create" => {
                files.insert(path.clone(), (false, false));
                dirty_parent(&mut dirty_directories, path);
            }
            "write" => *files.get_mut(path).unwrap() = (false, false),
            "flush" => files.get_mut(path).unwrap().0 = true,
            "sync" => {
                let state = files.get_mut(path).unwrap();
                assert!(state.0, "file must be flushed before sync: {path:?}");
                state.1 = true;
            }
            "sync_existing" => {
                files.insert(path.clone(), (true, true));
            }
            "sync_dir" => {
                dirty_directories.remove(path);
            }
            "link" | "rename" | "publish" => {
                let from = event.source.as_ref().unwrap();
                assert_eq!(files.get(from), Some(&(true, true)), "{event:?}");
                if event.kind == "publish" {
                    assert!(dirty_directories.is_empty(), "{dirty_directories:?}");
                    commits += 1;
                }
                files.insert(path.clone(), (true, true));
                dirty_parent(&mut dirty_directories, path);
                if event.kind != "link" {
                    files.remove(from);
                    dirty_parent(&mut dirty_directories, from);
                }
            }
            "remove" => {
                files.remove(path);
                dirty_parent(&mut dirty_directories, path);
            }
            _ => {}
        }
    }
    assert_eq!(commits, 1);
    assert!(dirty_directories.is_empty(), "{dirty_directories:?}");
    assert!(files.values().all(|state| *state == (true, true)));
}

fn trace(existing: bool, race_import: bool) -> Vec<Event> {
    let fixture = Fixture::new();
    if existing {
        fixture.seed();
        fixture.abandoned();
    }
    let mut store = Store::with_fs(
        fixture.root.clone(),
        FaultFs {
            race_import,
            ..Default::default()
        },
    );
    store
        .save(&fixture.b, &fixture.candidate_sources(existing))
        .unwrap();
    let events = store.into_fs().events;
    assert_sync_contract(&events);
    events
}

#[test]
fn every_save_boundary_before_after_and_partial_write_is_retryable() {
    let mut tested = 0;
    for (existing, race_import) in [(false, false), (true, false), (true, true)] {
        let baseline = trace(existing, race_import);
        for (index, event) in baseline.iter().enumerate() {
            let mut moments = vec![Moment::Before, Moment::After];
            if event.kind == "write" {
                moments.push(Moment::PartialWrite);
            }
            for moment in moments {
                let fixture = Fixture::new();
                let previous = if existing {
                    let id = fixture.seed();
                    fixture.abandoned();
                    Some(id)
                } else {
                    None
                };
                let sources = fixture.candidate_sources(existing);
                let before = fixture.b.clone();
                let mut store = Store::with_fs(
                    fixture.root.clone(),
                    FaultFs {
                        race_import,
                        ..FaultFs::failing(index, moment)
                    },
                );
                let failure = store
                    .save(&fixture.b, &sources)
                    .expect_err(&format!("{} #{index} {moment:?}", event.kind));
                let fs = store.into_fs();
                assert!(fs.fired);
                assert_eq!(fixture.b, before);
                fixture.assert_state(previous);
                let current = if fixture.root.exists() {
                    Store::new(fixture.root.clone())
                        .inspect_committed()
                        .unwrap()
                } else {
                    None
                };
                match failure.commit {
                    CommitState::Unchanged => {
                        assert_eq!(
                            current.map(|c| c.document),
                            previous.map(|_| fixture.a.clone())
                        );
                    }
                    CommitState::Unknown(_) => assert_eq!(event.kind, "publish"),
                    CommitState::PublishedNotDurable(_) => {
                        // Publication is complete even if later sync/evidence fails.
                        assert!(fs.events.iter().any(|event| event.kind == "publish"));
                        assert_eq!(current.unwrap().document, fixture.b);
                    }
                }
                let options = Store::new(fixture.root.clone()).recovery_options();
                if let Ok(options) = options {
                    for id in options.recoverable {
                        let recovered = Store::new(fixture.root.clone())
                            .open(OpenChoice::Committed(id))
                            .unwrap();
                        if matches!(failure.commit, CommitState::Unchanged) {
                            assert_eq!(Some(id), previous);
                            assert_eq!(recovered.document(), &fixture.a);
                        }
                    }
                }
                if let Some(id) = previous {
                    for event in &fs.events {
                        if ["create", "write", "rename", "link", "remove"].contains(&event.kind) {
                            assert_ne!(event.path, fixture.root.join(id.relative_path()));
                            for asset in &fixture.a.assets {
                                assert_ne!(
                                    event.path,
                                    fixture.root.join(asset.reference.id.relative_path())
                                );
                            }
                        }
                    }
                }
                // A second failure on this *same* partly completed project.
                let mut retry = Store::with_fs(
                    fixture.root.clone(),
                    FaultFs {
                        fault: Some(Fault::Publish),
                        ..Default::default()
                    },
                );
                assert!(matches!(
                    retry.save(&fixture.b, &sources).unwrap_err().commit,
                    CommitState::Unknown(_)
                ));
                fixture.assert_state(previous);
                let receipt = Store::new(fixture.root.clone())
                    .save(&fixture.b, &sources)
                    .unwrap();
                fixture.assert_state(previous);
                assert_eq!(
                    Store::new(fixture.root.clone())
                        .inspect_committed()
                        .unwrap()
                        .unwrap()
                        .generation,
                    receipt.generation
                );
                if existing {
                    assert_eq!(
                        fs::read(fixture.root.join(".brownnote-store/staging/user-note.txt"))
                            .unwrap(),
                        b"keep me"
                    );
                    assert!(
                        fixture
                            .root
                            .join(".brownnote-store/staging/unknown-directory")
                            .is_dir()
                    );
                }
                tested += 1;
            }
        }
        for kind in [
            "ensure_dir",
            "lock",
            "list",
            "read",
            "create",
            "write",
            "flush",
            "sync",
            "sync_dir",
            "link",
            "rename",
            "publish",
            "remove",
        ] {
            assert!(baseline.iter().any(|event| event.kind == kind), "{kind}");
        }
        if existing {
            assert!(baseline.iter().any(|event| event.kind == "sync_existing"));
        }
    }
    println!("exercised {tested} save fault cases, each with a second failed retry and success");
}

#[test]
fn every_inspection_boundary_fails_read_only_without_applying_state() {
    let fixture = Fixture::new();
    let generation = fixture.seed();
    for snapshot in [false, true] {
        let inspect = |store: &mut Store<FaultFs>| {
            if snapshot {
                store.inspect_snapshot(generation).map(Some)
            } else {
                store.inspect_committed()
            }
        };
        let mut baseline = Store::with_fs(fixture.root.clone(), FaultFs::default());
        inspect(&mut baseline).unwrap();
        let count = baseline.into_fs().events.len();
        for index in 0..count {
            for moment in [Moment::Before, Moment::After] {
                let mut store =
                    Store::with_fs(fixture.root.clone(), FaultFs::failing(index, moment));
                assert!(inspect(&mut store).is_err());
                let fs = store.into_fs();
                assert!(fs.fired);
                assert!(
                    fs.events
                        .iter()
                        .all(|event| ["check_dir", "read"].contains(&event.kind))
                );
                fixture.assert_state(Some(generation));
                assert_eq!(
                    Store::new(fixture.root.clone())
                        .inspect_committed()
                        .unwrap()
                        .unwrap()
                        .document,
                    fixture.a
                );
            }
        }
    }
}

#[test]
fn real_filesystem_relocation_shared_media_retained_wav_without_bench() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    let receipt = Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap();
    assert_eq!(receipt.previous, Some(first));
    let relocated = fixture.base.join("relocated-project");
    fs::rename(&fixture.root, &relocated).unwrap();
    for path in fixture
        .sources_a
        .iter()
        .chain(fixture.sources_b.iter())
        .map(|source| &source.path)
    {
        fs::remove_file(path).unwrap();
    }
    let mut store = Store::new(relocated.clone());
    let prepared = store.open(OpenChoice::Current).unwrap();
    let loaded = prepared.inspection();
    assert_eq!(loaded.document, fixture.b);
    let previous = store.open(OpenChoice::Committed(first)).unwrap();
    assert_eq!(previous.document(), &fixture.a);
    assert!(!fixture.root.exists());
    // Two generations share one source and retain both authoritative renders.
    assert_eq!(fs::read_dir(relocated.join("media")).unwrap().count(), 3);
    for pad in &loaded.document.pads {
        let audible = pad.sample.as_ref().unwrap().audible();
        let reference = &loaded
            .document
            .assets
            .iter()
            .find(|asset| asset.reference.id == audible)
            .unwrap()
            .reference;
        let path = media::resolve_verified(&relocated, reference).unwrap();
        let sample = kit::load_wav(&path).unwrap();
        let owned = kit::decode_wav(
            Cursor::new(&prepared.media()[&audible][..]),
            Path::new("relocated-retained.wav"),
        )
        .unwrap();
        assert_eq!(&*owned.data, &*sample.data);
        assert_eq!(sample.rate, 48_000);
        assert_eq!(sample.data.len(), 480);
        assert!(sample.data.iter().all(|&v| v == -12_000.0 / 32768.0));
        let mut output = Output::virtual_driver(
            Slots::from_samples([sample]),
            "store relocation test".into(),
            false,
        );
        assert!(output.send(Command::Trigger {
            pad: 0,
            slot: 0,
            group: 0,
            velocity: 1.0,
            gain: pad.gain,
            pan: pad.pan,
            semitones: pad.semitones,
        }));
        output.advance(0.005);
        assert!(f32::from_bits(output.meters.peak.load(Ordering::Relaxed)) > 0.0);
    }
    // No bench, cache or process interface exists in the storage API.
    let same = store.save(&loaded.document, &[]).unwrap();
    assert_eq!(same.generation, receipt.generation);
    assert_eq!(same.previous, Some(receipt.generation));
}

// One deterministic byte-level witness for all read-only failure/retry checks.
fn tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if path.is_dir() {
                result.insert(relative, Vec::new());
                visit(root, &path, result);
            } else {
                result.insert(relative, fs::read(path).unwrap());
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn complete_candidates_and_abandoned_staging_are_not_committed_history() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    let mut failed = Store::with_fs(
        fixture.root.clone(),
        FaultFs {
            fault: Some(Fault::Publish),
            ..Default::default()
        },
    );
    let CommitState::Unknown(candidate) = failed
        .save(&fixture.b, &fixture.sources_b)
        .unwrap_err()
        .commit
    else {
        panic!("fixture must fail just before publish")
    };
    // The old API quite deliberately allows this complete, uncommitted snapshot.
    assert_eq!(
        Store::new(fixture.root.clone())
            .inspect_snapshot(candidate)
            .unwrap()
            .document,
        fixture.b
    );
    fixture.abandoned();
    let before = tree(&fixture.root);
    let mut store = Store::new(fixture.root.clone());
    let options = store.recovery_options().unwrap();
    assert_eq!(options.current, Ok(Some(first)));
    assert_eq!(options.recoverable, vec![first]);
    assert_eq!(options.unconfirmed, vec![candidate]);
    assert!(options.rejected.is_empty());
    assert_eq!(
        store.open(OpenChoice::Committed(candidate)).unwrap_err(),
        StoreError::UncommittedGeneration(candidate)
    );
    assert_eq!(
        store.open(OpenChoice::Current).unwrap().document(),
        &fixture.a
    );
    assert_eq!(store.recovery_options().unwrap(), options);
    assert_eq!(tree(&fixture.root), before);
}

#[test]
fn corrupt_current_offers_explicit_complete_recovery_without_rewriting_anything() {
    for corruption in ["document", "missing-render", "changed-render"] {
        let fixture = Fixture::new();
        let first = fixture.seed();
        let latest = Store::new(fixture.root.clone())
            .save(&fixture.b, &fixture.sources_b)
            .unwrap()
            .generation;
        let rendered = fixture
            .root
            .join(fixture.b.assets[1].reference.id.relative_path());
        match corruption {
            "document" => fs::write(fixture.root.join("project.json"), b"{broken").unwrap(),
            "missing-render" => fs::remove_file(&rendered).unwrap(),
            _ => {
                let mut bytes = fs::read(&rendered).unwrap();
                bytes[44] ^= 1;
                fs::write(&rendered, bytes).unwrap();
            }
        }
        let before = tree(&fixture.root);
        let mut active = fixture.b.clone();
        let mut store = Store::new(fixture.root.clone());
        let result = store.open(OpenChoice::Current);
        if let Ok(prepared) = &result {
            active = prepared.document().clone();
        }
        assert!(result.is_err());
        let options = store.recovery_options().unwrap();
        assert!(options.current.is_err());
        assert!(options.recoverable.contains(&first));
        if corruption == "document" {
            assert!(options.recoverable.contains(&latest));
        } else {
            assert!(!options.recoverable.contains(&latest));
            assert!(options.rejected.iter().any(|(id, _)| *id == latest));
            assert!(store.open(OpenChoice::Committed(latest)).is_err());
        }
        // Nothing changes until a caller explicitly chooses a recovery identity.
        assert_eq!(active, fixture.b);
        let selected = store.open(OpenChoice::Committed(first)).unwrap();
        assert_eq!(selected.document(), &fixture.a);
        for asset in &fixture.a.assets {
            assert_eq!(
                &*selected.media()[&asset.reference.id],
                fs::read(fixture.root.join(asset.reference.id.relative_path())).unwrap()
            );
        }
        assert_eq!(active, fixture.b);
        assert_eq!(store.recovery_options().unwrap(), options);
        assert_eq!(tree(&fixture.root), before);
    }
}

#[test]
fn every_open_and_recovery_read_fault_preserves_active_project_and_source_bytes() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    fixture.abandoned();
    let before = tree(&fixture.root);
    let mut cases = 0;
    for choice in [OpenChoice::Current, OpenChoice::Committed(first)] {
        let mut baseline = Store::with_fs(fixture.root.clone(), FaultFs::default());
        baseline.open(choice).unwrap();
        let events = baseline.into_fs().events;
        for index in 0..events.len() {
            for moment in [Moment::Before, Moment::After] {
                let mut active = fixture.b.clone();
                let mut store =
                    Store::with_fs(fixture.root.clone(), FaultFs::failing(index, moment));
                // Exercise the caller's apply-on-success pattern with an existing kit.
                let result = store.open(choice);
                if let Ok(prepared) = &result {
                    active = prepared.document().clone();
                }
                assert!(result.is_err());
                assert_eq!(active, fixture.b);
                let fs = store.into_fs();
                assert!(fs.fired);
                assert!(
                    fs.events
                        .iter()
                        .all(|e| ["check_dir", "read"].contains(&e.kind))
                );
                assert_eq!(tree(&fixture.root), before);
                assert_eq!(
                    Store::new(fixture.root.clone())
                        .open(choice)
                        .unwrap()
                        .document(),
                    &fixture.a
                );
                cases += 1;
            }
        }
    }
    let mut baseline = Store::with_fs(fixture.root.clone(), FaultFs::default());
    baseline.recovery_options().unwrap();
    let events = baseline.into_fs().events;
    for index in 0..events.len() {
        for moment in [Moment::Before, Moment::After] {
            let mut store = Store::with_fs(fixture.root.clone(), FaultFs::failing(index, moment));
            let result = store.recovery_options();
            match result {
                Err(_) => {}
                Ok(options) => assert!(
                    options.current.is_err() || !options.rejected.is_empty(),
                    "fault must not disappear from recovery diagnostics"
                ),
            }
            let fs = store.into_fs();
            assert!(fs.fired);
            assert!(
                fs.events
                    .iter()
                    .all(|e| ["check_dir", "list", "read"].contains(&e.kind))
            );
            assert_eq!(tree(&fixture.root), before);
            cases += 1;
        }
    }
    println!("exercised {cases} open/recovery read fault cases");
}

#[test]
fn legacy_snapshots_stay_unconfirmed_until_observed_as_current() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    // This is the real first-slice layout, which had snapshots but no evidence.
    fs::remove_dir_all(fixture.root.join(".brownnote-store/committed")).unwrap();
    let mut store = Store::new(fixture.root.clone());
    assert_eq!(
        store.open(OpenChoice::Current).unwrap().document(),
        &fixture.a
    );
    let before = tree(&fixture.root);
    assert!(store.recovery_options().unwrap().recoverable.is_empty());
    assert_eq!(tree(&fixture.root), before);
    store.save(&fixture.b, &fixture.sources_b).unwrap();
    fs::write(fixture.root.join("project.json"), b"{broken").unwrap();
    assert_eq!(
        store.open(OpenChoice::Committed(first)).unwrap().document(),
        &fixture.a
    );
}

#[test]
fn invalid_evidence_or_snapshot_never_becomes_a_recovery_choice() {
    for corrupt in ["evidence", "snapshot", "missing-snapshot"] {
        let fixture = Fixture::new();
        let first = fixture.seed();
        match corrupt {
            "evidence" => fs::write(
                fixture
                    .root
                    .join(".brownnote-store/committed")
                    .join(first.to_string()),
                b"partial or foreign marker",
            )
            .unwrap(),
            "snapshot" => fs::write(fixture.root.join(first.relative_path()), b"{}").unwrap(),
            _ => fs::remove_file(fixture.root.join(first.relative_path())).unwrap(),
        }
        let before = tree(&fixture.root);
        let mut store = Store::new(fixture.root.clone());
        let options = store.recovery_options().unwrap();
        assert!(options.recoverable.is_empty());
        assert_eq!(options.rejected.len(), 1);
        assert_eq!(options.rejected[0].0, first);
        assert!(store.open(OpenChoice::Committed(first)).is_err());
        assert_eq!(
            store.open(OpenChoice::Current).unwrap().document(),
            &fixture.a
        );
        assert_eq!(tree(&fixture.root), before);
    }
}

#[test]
fn prepared_open_owns_verified_media_instead_of_reopening_paths() {
    let fixture = Fixture::new();
    fixture.seed();
    let prepared = Store::new(fixture.root.clone())
        .open(OpenChoice::Current)
        .unwrap();
    fs::remove_dir_all(&fixture.root).unwrap();
    for source in &fixture.sources_a {
        fs::remove_file(&source.path).unwrap();
    }
    for asset in &fixture.a.assets {
        assert_eq!(
            AssetRef::of(&prepared.media()[&asset.reference.id]),
            asset.reference
        );
    }
    let audible = prepared.document().pads[0]
        .sample
        .as_ref()
        .unwrap()
        .audible();
    let sample = kit::decode_wav(
        Cursor::new(&prepared.media()[&audible][..]),
        Path::new("owned-retained.wav"),
    )
    .unwrap();
    assert!(sample.data.iter().all(|&value| value == 8_000.0 / 32768.0));
}

#[test]
fn absent_current_is_not_silently_recovered_and_missing_source_is_identified() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    fs::remove_file(fixture.root.join("project.json")).unwrap();
    let before = tree(&fixture.root);
    let mut store = Store::new(fixture.root.clone());
    assert_eq!(
        store.open(OpenChoice::Current).unwrap_err(),
        StoreError::NoCommittedProject
    );
    let options = store.recovery_options().unwrap();
    assert_eq!(options.current, Ok(None));
    assert_eq!(options.recoverable, vec![first]);
    assert_eq!(tree(&fixture.root), before);
    let source = fixture.a.assets[0].reference.id;
    fs::remove_file(fixture.root.join(source.relative_path())).unwrap();
    let before = tree(&fixture.root);
    assert_eq!(
        store.open(OpenChoice::Committed(first)).unwrap_err(),
        StoreError::Media(media::MediaError::Missing(source))
    );
    assert!(store.recovery_options().unwrap().recoverable.is_empty());
    assert_eq!(tree(&fixture.root), before);
}

#[test]
fn invalid_settings_references_and_unknown_fields_fail_before_caller_apply() {
    let fixture = Fixture::new();
    fixture.seed();
    let value = serde_json::to_value(&fixture.a).unwrap();
    for mutate in [
        |v: &mut serde_json::Value| v["pads"][0]["gain"] = 2.0.into(),
        |v: &mut serde_json::Value| v["pads"][0]["sample"]["retained"] = serde_json::Value::Null,
        |v: &mut serde_json::Value| {
            v["assets"].as_array_mut().unwrap().pop();
        },
        |v: &mut serde_json::Value| v["future_state"] = true.into(),
    ] {
        let mut invalid = value.clone();
        mutate(&mut invalid);
        fs::write(
            fixture.root.join("project.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        let before = tree(&fixture.root);
        let mut active = fixture.b.clone();
        let result = Store::new(fixture.root.clone()).open(OpenChoice::Current);
        if let Ok(prepared) = &result {
            active = prepared.document().clone();
        }
        assert!(matches!(result, Err(StoreError::Document(_))));
        assert_eq!(active, fixture.b);
        assert_eq!(tree(&fixture.root), before);
    }
}

#[test]
fn historical_v1_opens_exact_bytes_and_unsupported_versions_never_migrate_or_overwrite() {
    let fixture = Fixture::new();
    fs::create_dir(&fixture.root).unwrap();
    fs::create_dir(fixture.root.join("media")).unwrap();
    let v1 = include_bytes!("fixtures/project-v1/project.json");
    let current = fixture.root.join("project.json");
    fs::write(&current, v1).unwrap();
    let mut store = Store::new(fixture.root.clone());
    let prepared = store.open(OpenChoice::Current).unwrap();
    assert_eq!(prepared.document(), &Document::default());
    assert_eq!(prepared.inspection().bytes, v1);
    assert!(prepared.media().is_empty());
    assert!(!fixture.root.join(".brownnote-store").exists());
    // v0 is deliberately not a historical fixture: no native v0 ever shipped.
    for (bytes, expected) in [
        (
            b"{\"version\":0}".as_slice(),
            DocumentError::OlderVersion(0),
        ),
        (
            b"{\"version\":2,\"unknown\":true}".as_slice(),
            DocumentError::NewerVersion(2),
        ),
        (
            b"{\"version\":4294967295}".as_slice(),
            DocumentError::NewerVersion(u32::MAX),
        ),
        (
            b"{\"version\":4294967296}".as_slice(),
            DocumentError::InvalidDocument,
        ),
        (b"{}".as_slice(), DocumentError::InvalidDocument),
        (b"{".as_slice(), DocumentError::MalformedJson),
    ] {
        fs::write(&current, bytes).unwrap();
        let before = tree(&fixture.root);
        let mut active = fixture.a.clone();
        let result = store.open(OpenChoice::Current);
        if let Ok(prepared) = &result {
            active = prepared.document().clone();
        }
        assert_eq!(result.unwrap_err(), StoreError::Document(expected.clone()));
        assert_eq!(active, fixture.a);
        assert_eq!(
            store.recovery_options().unwrap().current,
            Err(StoreError::Document(expected))
        );
        assert_eq!(tree(&fixture.root), before);
    }
}

#[test]
fn invalid_or_vanished_sources_leave_a_and_caller_data_untouched() {
    for corruption in ["missing", "truncated", "same-size"] {
        let fixture = Fixture::new();
        let previous = fixture.seed();
        let path = &fixture.sources_b[0].path;
        match corruption {
            "missing" => fs::remove_file(path).unwrap(),
            "truncated" => fs::write(path, b"partial").unwrap(),
            _ => {
                let mut bytes = fs::read(path).unwrap();
                bytes[44] ^= 1;
                fs::write(path, bytes).unwrap();
            }
        }
        let before = fixture.b.clone();
        let failure = Store::new(fixture.root.clone())
            .save(&fixture.b, &fixture.sources_b)
            .unwrap_err();
        assert_eq!(failure.commit, CommitState::Unchanged);
        assert_eq!(before, fixture.b);
        fixture.assert_state(Some(previous));
        assert_eq!(
            Store::new(fixture.root.clone())
                .inspect_committed()
                .unwrap()
                .unwrap()
                .document,
            fixture.a
        );
    }
}

#[test]
fn corrupt_current_is_not_overwritten_and_complete_snapshot_is_inspectable() {
    let fixture = Fixture::new();
    let previous = fixture.seed();
    let current = fixture.root.join("project.json");
    for bytes in [
        b"{".as_slice(),
        b"{\"version\":0}".as_slice(),
        b"{\"version\":2}".as_slice(),
    ] {
        fs::write(&current, bytes).unwrap();
        let mut store = Store::new(fixture.root.clone());
        assert_eq!(
            store.inspect_snapshot(previous).unwrap().document,
            fixture.a
        );
        assert!(store.inspect_committed().is_err());
        assert_eq!(
            store
                .save(&fixture.b, &fixture.sources_b)
                .unwrap_err()
                .commit,
            CommitState::Unchanged
        );
        assert_eq!(fs::read(&current).unwrap(), bytes);
    }
}

#[test]
fn corrupt_immutable_candidate_is_rejected_instead_of_replaced() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    let asset = &fixture.b.assets[1].reference;
    let destination = fixture.root.join(asset.id.relative_path());
    let mut corrupt = fs::read(&fixture.sources_b[0].path).unwrap();
    corrupt[44] ^= 1;
    fs::write(&destination, &corrupt).unwrap();
    let failure = Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap_err();
    assert_eq!(failure.commit, CommitState::Unchanged);
    assert!(matches!(*failure.error, StoreError::Media(_)));
    assert_eq!(fs::read(destination).unwrap(), corrupt);
    fixture.assert_state(Some(first));
}

#[test]
fn adopting_contract_project_preserves_exact_noncanonical_saved_bytes() {
    let fixture = Fixture::new();
    fs::create_dir(&fixture.root).unwrap();
    fs::create_dir(fixture.root.join("media")).unwrap();
    for source in &fixture.sources_a {
        fs::copy(&source.path, fixture.root.join(source.id.relative_path())).unwrap();
    }
    let original = serde_json::to_vec(&fixture.a).unwrap();
    assert_ne!(original, fixture.a.encode().unwrap());
    fs::write(fixture.root.join("project.json"), &original).unwrap();
    let mut store = Store::new(fixture.root.clone());
    let previous = store.inspect_committed().unwrap().unwrap().generation;
    let receipt = store.save(&fixture.b, &fixture.sources_b).unwrap();
    assert_eq!(receipt.previous, Some(previous));
    assert_eq!(store.inspect_snapshot(previous).unwrap().bytes, original);
    assert_eq!(
        store.inspect_committed().unwrap().unwrap().document,
        fixture.b
    );
}

#[test]
fn invalid_documents_and_source_sets_fail_before_any_io() {
    let fixture = Fixture::new();
    let mut store = Store::with_fs(fixture.root.clone(), FaultFs::default());
    let mut invalid = fixture.a.clone();
    invalid.version = 2;
    assert_eq!(
        *store.save(&invalid, &fixture.sources_a).unwrap_err().error,
        StoreError::Document(DocumentError::NewerVersion(2))
    );
    let sources = vec![fixture.sources_a[0].clone(); 2];
    assert_eq!(
        *store.save(&fixture.a, &sources).unwrap_err().error,
        StoreError::InvalidSources
    );
    assert!(store.into_fs().events.is_empty());
    assert!(!fixture.root.exists());
}

#[test]
fn new_project_parent_sync_and_writer_exclusion_are_real() {
    let fixture = Fixture::new();
    assert!(!fixture.root.exists());
    let first = fixture.seed();
    let mut fs = SystemFs;
    let lock = fs
        .lock(&fixture.root.join(".brownnote-store/lock"))
        .unwrap();
    let failure = Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap_err();
    assert_eq!(failure.commit, CommitState::Unchanged);
    assert_eq!(*failure.error, StoreError::Io(io::ErrorKind::WouldBlock));
    fixture.assert_state(Some(first));
    drop(lock);
    Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap();
}

#[cfg(unix)]
#[test]
fn symlinked_media_or_private_directories_cannot_escape_or_be_cleaned() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    let outside = fixture.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("document.tmp"), b"not ours").unwrap();
    let staging = fixture.root.join(".brownnote-store/staging");
    fs::remove_dir(&staging).unwrap();
    std::os::unix::fs::symlink(&outside, &staging).unwrap();
    assert!(
        Store::new(fixture.root.clone())
            .save(&fixture.b, &fixture.sources_b)
            .is_err()
    );
    assert_eq!(fs::read(outside.join("document.tmp")).unwrap(), b"not ours");
    fixture.assert_state(Some(first));
    fs::remove_file(staging).unwrap();
    let media = fixture.root.join("media");
    let moved = fixture.base.join("external-media");
    fs::rename(&media, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &media).unwrap();
    assert!(
        Store::new(fixture.root.clone())
            .inspect_committed()
            .is_err()
    );
}
