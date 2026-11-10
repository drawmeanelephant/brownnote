#![allow(clippy::unwrap_used, clippy::expect_used)]
use brownnote_native::{
    engine::{Command, Slots},
    import::{MediaPublisher, ProjectMedia, SessionMedia},
    kit,
    output::Output,
    pedals::render::{Chain, Stage},
    project::{
        Asset, AssetId, AssetRef, Document, DocumentError, MediaKind, SampleAssignment, media,
        recovery::OpenChoice,
        session::{self, Snapshot},
        store::{
            CommitState, FileSystem, GenerationId, MediaSource, RetentionPolicy, Store, StoreError,
            SystemFs, UnconfirmedSnapshots,
        },
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Cursor},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
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
    path_operation!(size, u64);
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
                    .all(|e| ["check_dir", "list", "read", "size"].contains(&e.kind))
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

/// An owned capture as the UI worker makes it, without a live registry.
fn recovery_snapshot(document: Document, sources: &[MediaSource]) -> Snapshot {
    let media = SessionMedia::default();
    for source in sources {
        let bytes = fs::read(&source.path).unwrap();
        media.publish(&AssetRef::of(&bytes), &bytes).unwrap();
    }
    Snapshot {
        document,
        media: Arc::new(media),
        freeze: None,
    }
}

#[test]
fn autosave_copy_uses_a_separate_machine_root_and_leaves_the_saved_store_untouched() {
    let fixture = Fixture::new();
    let saved = fixture.seed();
    let base = fixture.base.join("recovery");
    fs::create_dir(&base).unwrap();
    let before = tree(&fixture.root);
    let mut edited = fixture.a.clone();
    edited.pads[0].gain = 0.25;
    let receipt = session::autosave_copy(
        &base,
        &fixture.root,
        saved,
        recovery_snapshot(edited.clone(), &fixture.sources_a),
    )
    .unwrap();

    let copy_root = session::recovery_copy_root(&base, &fixture.root).unwrap();
    assert_ne!(copy_root, fixture.root);
    assert!(copy_root.starts_with(&base));
    assert_eq!(
        session::recovery_copy_root(&base, &fixture.root).unwrap(),
        copy_root,
        "the copy directory must be stable for one project"
    );
    assert_eq!(receipt.saved_generation, saved);
    assert_eq!(
        tree(&fixture.root),
        before,
        "autosave touched the saved project"
    );
    assert_eq!(
        Store::new(fixture.root.clone())
            .inspect_committed()
            .unwrap()
            .unwrap()
            .generation,
        saved
    );

    let copy = Store::new(copy_root.clone())
        .open(OpenChoice::Current)
        .unwrap();
    assert_eq!(copy.document(), &edited, "the copy holds the unsaved edit");
    let record = session::read_autosave(&copy_root).unwrap();
    assert_eq!(
        record.project,
        fixture.root.canonicalize().unwrap().to_string_lossy()
    );
    assert_eq!(record.saved_generation, saved.to_string());
    assert_eq!(record.copy_generation, receipt.generation.to_string());
    assert!(record.written_ms > 0);
    assert!(
        !fixture.root.join(session::AUTOSAVE_FILE).exists(),
        "recovery metadata belongs to the machine copy only"
    );
}

#[test]
fn failed_autosave_keeps_the_previous_recovery_copy_and_the_saved_project() {
    let fixture = Fixture::new();
    let saved = fixture.seed();
    let base = fixture.base.join("recovery");
    fs::create_dir(&base).unwrap();
    let mut edited = fixture.a.clone();
    edited.pads[1].gain = 0.3;
    let first = session::autosave_copy(
        &base,
        &fixture.root,
        saved,
        recovery_snapshot(edited.clone(), &fixture.sources_a),
    )
    .unwrap();
    let copy_root = session::recovery_copy_root(&base, &fixture.root).unwrap();
    let saved_before = tree(&fixture.root);

    // A declared asset with no bytes anywhere fails before any publish.
    let absent = b"never published bytes";
    let mut broken = edited.clone();
    broken.pads[2].gain = 0.4;
    broken.assets.push(Asset {
        reference: AssetRef::of(absent),
        kind: MediaKind::Source,
    });
    let failure = session::autosave_copy(
        &base,
        &fixture.root,
        saved,
        recovery_snapshot(broken, &fixture.sources_a),
    )
    .unwrap_err();
    match failure {
        session::Error::Save(error) => assert_eq!(error.commit, CommitState::Unchanged),
        other => panic!("unexpected autosave failure: {other}"),
    }

    // The previous complete copy generation and its record are intact.
    let copy = Store::new(copy_root.clone())
        .open(OpenChoice::Current)
        .unwrap();
    assert_eq!(copy.document(), &edited);
    assert_eq!(
        session::read_autosave(&copy_root).unwrap().copy_generation,
        first.generation.to_string()
    );
    assert_eq!(tree(&fixture.root), saved_before);
    let offer = session::recovery_copy(&base, &fixture.root, saved)
        .unwrap()
        .unwrap();
    assert_eq!(offer.offered, Some(first.generation));
}

#[test]
fn recovery_copy_is_offered_only_for_the_same_project_and_saved_generation() {
    let fixture = Fixture::new();
    let saved = fixture.seed();
    let base = fixture.base.join("recovery");
    fs::create_dir(&base).unwrap();
    let copy_root = session::recovery_copy_root(&base, &fixture.root).unwrap();

    // A copy identical to the saved version is never offered.
    let identical = session::autosave_copy(
        &base,
        &fixture.root,
        saved,
        recovery_snapshot(fixture.a.clone(), &fixture.sources_a),
    )
    .unwrap();
    assert_eq!(identical.generation, saved);
    let report = session::recovery_copy(&base, &fixture.root, saved)
        .unwrap()
        .unwrap();
    assert_eq!(report.offered, None);
    assert!(
        report.note.contains("matches the saved version"),
        "{}",
        report.note
    );

    // A differing copy of the same saved generation is offered, not applied.
    let mut edited = fixture.a.clone();
    edited.pads[0].gain = 0.123;
    let newer = session::autosave_copy(
        &base,
        &fixture.root,
        saved,
        recovery_snapshot(edited, &fixture.sources_a),
    )
    .unwrap();
    let report = session::recovery_copy(&base, &fixture.root, saved)
        .unwrap()
        .unwrap();
    assert_eq!(report.offered, Some(newer.generation));
    assert_eq!(
        Store::new(fixture.root.clone())
            .inspect_committed()
            .unwrap()
            .unwrap()
            .generation,
        saved,
        "offering a copy must not apply it"
    );

    // A later save makes the copy older, so it is not offered.
    let second = Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap()
        .generation;
    let report = session::recovery_copy(&base, &fixture.root, second)
        .unwrap()
        .unwrap();
    assert_eq!(report.offered, None);
    assert!(
        report.note.contains("older than the saved version"),
        "{}",
        report.note
    );

    // A directory holding another project's record is never offered.
    let other = fixture.base.join("other");
    let other_saved = Store::new(other.clone())
        .save(&fixture.a, &fixture.sources_a)
        .unwrap()
        .generation;
    let other_copy = session::recovery_copy_root(&base, &other).unwrap();
    fs::rename(&copy_root, &other_copy).unwrap();
    let report = session::recovery_copy(&base, &other, other_saved)
        .unwrap()
        .unwrap();
    assert_eq!(report.offered, None);
    assert!(report.note.contains("another project"), "{}", report.note);
}

#[test]
fn recovery_copy_with_missing_or_corrupt_media_is_rejected_not_offered() {
    for corrupt in [false, true] {
        let fixture = Fixture::new();
        let saved = fixture.seed();
        let base = fixture.base.join("recovery");
        fs::create_dir(&base).unwrap();
        let mut edited = fixture.a.clone();
        edited.pads[0].gain = 0.2;
        let copy = session::autosave_copy(
            &base,
            &fixture.root,
            saved,
            recovery_snapshot(edited.clone(), &fixture.sources_a),
        )
        .unwrap();
        let copy_root = session::recovery_copy_root(&base, &fixture.root).unwrap();
        let reference = edited
            .assets
            .iter()
            .find(|asset| asset.kind == MediaKind::RetainedRender)
            .unwrap()
            .reference
            .clone();
        let path = copy_root.join(reference.id.relative_path());
        if corrupt {
            fs::write(&path, vec![0; reference.bytes as usize]).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }

        let report = session::recovery_copy(&base, &fixture.root, saved)
            .unwrap()
            .unwrap();
        assert_eq!(report.offered, None);
        assert!(report.options.current.is_err());
        assert!(!report.options.recoverable.contains(&copy.generation));
        assert!(
            !report.options.rejected.is_empty(),
            "an invalid copy must stay visible as rejected"
        );
        // The saved project still opens on its own.
        let opened = Store::new(fixture.root.clone())
            .open(OpenChoice::Current)
            .unwrap();
        assert_eq!(opened.document(), &fixture.a);
    }
}

fn evidence_path(root: &Path, generation: GenerationId) -> PathBuf {
    root.join(".brownnote-store/committed")
        .join(generation.to_string())
}

/// Every evidence marker still has the snapshot it witnesses.
fn assert_evidence_has_snapshot(root: &Path) {
    let committed = root.join(".brownnote-store/committed");
    let Ok(entries) = fs::read_dir(&committed) else {
        return;
    };
    for entry in entries {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            root.join(".brownnote-store/versions")
                .join(format!("{name}.json"))
                .exists(),
            "evidence without a snapshot: {name}"
        );
    }
}

#[test]
fn retention_plan_is_read_only_and_collects_only_unreferenced_media() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    let second = Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap()
        .generation;

    // Two publishes stand in for imports: one is never referenced, one is
    // referenced by the next retained document.
    let orphan = wav(4_444);
    let orphan_ref = AssetRef::of(&orphan);
    ProjectMedia::new(fixture.root.clone())
        .publish(&orphan_ref, &orphan)
        .unwrap();
    let kept = wav(5_555);
    let kept_ref = AssetRef::of(&kept);
    ProjectMedia::new(fixture.root.clone())
        .publish(&kept_ref, &kept)
        .unwrap();
    let mut with_import = fixture.b.clone();
    with_import.assets.push(Asset {
        reference: kept_ref.clone(),
        kind: MediaKind::RetainedRender,
    });
    let current = Store::new(fixture.root.clone())
        .save(&with_import, &[])
        .unwrap()
        .generation;

    fs::write(fixture.root.join("notes.txt"), b"keep root note").unwrap();
    fs::create_dir(fixture.root.join("media/sub")).unwrap();
    fs::write(fixture.root.join("media/notes.txt"), b"keep media note").unwrap();
    fs::write(
        fixture.root.join("media/not-a-hash.wav"),
        b"keep media bytes",
    )
    .unwrap();
    fs::write(
        fixture.root.join("media/.import-1-2"),
        b"keep import staging",
    )
    .unwrap();
    fs::write(
        fixture.root.join(".brownnote-store/versions/user.json"),
        b"keep version note",
    )
    .unwrap();
    fs::write(
        fixture.root.join(".brownnote-store/committed/not-a-hash"),
        b"keep evidence note",
    )
    .unwrap();

    let before = tree(&fixture.root);
    let policy = RetentionPolicy::default();
    let plan = Store::new(fixture.root.clone())
        .retention_plan(&policy)
        .unwrap();
    let again = Store::new(fixture.root.clone())
        .retention_plan(&policy)
        .unwrap();
    assert_eq!(plan, again, "planning must be repeatable");
    assert_eq!(tree(&fixture.root), before, "planning must be read-only");

    assert_eq!(plan.usage.snapshots, 3);
    assert_eq!(plan.usage.evidence, 3);
    assert_eq!(plan.usage.media, 5);
    assert_eq!(plan.unknown.len(), 7);
    assert!(plan.evidence.is_empty());
    assert!(plan.snapshots.is_empty());
    assert_eq!(plan.media.len(), 1);
    assert_eq!(
        plan.media[0].path,
        fixture.root.join(orphan_ref.id.relative_path())
    );
    assert_eq!(plan.reclaimable_bytes, orphan.len() as u64);
    assert!(!plan.over_budget);
    assert!(!plan.media_refused);
    assert_eq!(plan.retained.len(), 3);
    assert!(plan.retained.contains(&first));
    assert!(plan.retained.contains(&second));
    assert!(plan.retained.contains(&current));
    assert!(fixture.root.join(kept_ref.id.relative_path()).exists());
    assert!(
        !plan
            .media
            .iter()
            .any(|removal| removal.path.ends_with("project.json"))
    );

    let receipt = Store::new(fixture.root.clone()).collect(&policy).unwrap();
    assert_eq!(receipt.removed_media, 1);
    assert_eq!(receipt.removed_evidence, 0);
    assert_eq!(receipt.removed_snapshots, 0);
    assert_eq!(receipt.bytes, orphan.len() as u64);
    assert!(!receipt.over_budget);
    assert!(!receipt.media_refused);
    assert!(!fixture.root.join(orphan_ref.id.relative_path()).exists());
    assert!(fixture.root.join(kept_ref.id.relative_path()).exists());
    assert_eq!(
        fs::read(fixture.root.join("notes.txt")).unwrap(),
        b"keep root note"
    );
    assert_eq!(
        fs::read(fixture.root.join("media/not-a-hash.wav")).unwrap(),
        b"keep media bytes"
    );
    assert_eq!(
        fs::read(fixture.root.join(".brownnote-store/committed/not-a-hash")).unwrap(),
        b"keep evidence note"
    );
    assert!(fixture.root.join("media/sub").is_dir());
    for id in [first, second, current] {
        assert!(
            Store::new(fixture.root.clone())
                .open(OpenChoice::Committed(id))
                .is_ok()
        );
    }

    let converged = Store::new(fixture.root.clone())
        .retention_plan(&policy)
        .unwrap();
    assert_eq!(converged.reclaimable_bytes, 0);
    assert!(converged.media.is_empty());
    let second_run = Store::new(fixture.root.clone()).collect(&policy).unwrap();
    assert_eq!(second_run.bytes, 0);
    assert_eq!(second_run.removed_media, 0);
}

#[test]
fn retention_removes_stale_evidence_and_a_pruned_snapshot_with_its_evidence() {
    // Evidence whose snapshot is missing is stale and is removed alone.
    let fixture = Fixture::new();
    let first = fixture.seed();
    let current = Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap()
        .generation;
    fs::remove_file(fixture.root.join(first.relative_path())).unwrap();
    assert_eq!(
        Store::new(fixture.root.clone())
            .inspect_committed()
            .unwrap()
            .unwrap()
            .generation,
        current
    );
    let before = tree(&fixture.root);
    let policy = RetentionPolicy::default();
    let plan = Store::new(fixture.root.clone())
        .retention_plan(&policy)
        .unwrap();
    assert_eq!(plan.evidence.len(), 1);
    assert_eq!(plan.evidence[0].path, evidence_path(&fixture.root, first));
    assert!(plan.snapshots.is_empty());
    assert_eq!(tree(&fixture.root), before);
    let receipt = Store::new(fixture.root.clone()).collect(&policy).unwrap();
    assert_eq!(receipt.removed_evidence, 1);
    assert!(!evidence_path(&fixture.root, first).exists());
    assert_evidence_has_snapshot(&fixture.root);
    assert_eq!(
        Store::new(fixture.root.clone())
            .open(OpenChoice::Current)
            .unwrap()
            .document(),
        &fixture.b
    );
    assert_eq!(
        Store::new(fixture.root.clone())
            .retention_plan(&policy)
            .unwrap()
            .reclaimable_bytes,
        0
    );
    assert_eq!(
        Store::new(fixture.root.clone())
            .collect(&policy)
            .unwrap()
            .bytes,
        0
    );

    // A bounded plan retires a validated generation's evidence and snapshot
    // together; the evidence is deleted first.
    let fixture = Fixture::new();
    let first = fixture.seed();
    let current = Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap()
        .generation;
    let bounded = RetentionPolicy {
        history_bytes: 0,
        ..RetentionPolicy::default()
    };
    let plan = Store::new(fixture.root.clone())
        .retention_plan(&bounded)
        .unwrap();
    assert!(
        plan.snapshots
            .iter()
            .any(|r| r.path == fixture.root.join(first.relative_path()))
    );
    assert!(
        plan.evidence
            .iter()
            .any(|r| r.path == evidence_path(&fixture.root, first))
    );
    let receipt = Store::new(fixture.root.clone()).collect(&bounded).unwrap();
    assert!(receipt.removed_evidence >= 1);
    assert!(receipt.removed_snapshots >= 1);
    assert!(!fixture.root.join(first.relative_path()).exists());
    assert!(!evidence_path(&fixture.root, first).exists());
    assert_evidence_has_snapshot(&fixture.root);
    assert_eq!(
        Store::new(fixture.root.clone())
            .open(OpenChoice::Committed(current))
            .unwrap()
            .document(),
        &fixture.b
    );
    assert_eq!(
        Store::new(fixture.root.clone())
            .retention_plan(&bounded)
            .unwrap()
            .reclaimable_bytes,
        0
    );
}

#[test]
fn interrupted_collection_leaves_the_current_project_and_evidence_consistent() {
    let policy = RetentionPolicy {
        history_bytes: 0,
        ..RetentionPolicy::default()
    };
    let prepare = || {
        let fixture = Fixture::new();
        fixture.seed();
        let current = Store::new(fixture.root.clone())
            .save(&fixture.b, &fixture.sources_b)
            .unwrap()
            .generation;
        let orphan = wav(4_444);
        let orphan_ref = AssetRef::of(&orphan);
        ProjectMedia::new(fixture.root.clone())
            .publish(&orphan_ref, &orphan)
            .unwrap();
        (fixture, current)
    };

    let (fixture, _) = prepare();
    let mut baseline = Store::with_fs(fixture.root.clone(), FaultFs::default());
    baseline.collect(&policy).unwrap();
    let events = baseline.into_fs().events;
    let removes: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.kind == "remove")
        .map(|(index, _)| index)
        .collect();
    assert!(removes.len() >= 2, "fixture must exercise several removals");

    for index in removes {
        for moment in [Moment::Before, Moment::After] {
            let (fixture, current) = prepare();
            let current_bytes = fs::read(fixture.root.join("project.json")).unwrap();
            let mut store = Store::with_fs(fixture.root.clone(), FaultFs::failing(index, moment));
            assert!(store.collect(&policy).is_err());
            let fs = store.into_fs();
            assert!(fs.fired);
            assert_eq!(
                fs::read(fixture.root.join("project.json")).unwrap(),
                current_bytes
            );
            let opened = Store::new(fixture.root.clone())
                .open(OpenChoice::Current)
                .unwrap();
            assert_eq!(opened.document(), &fixture.b);
            assert!(
                media::verify_all(&fixture.root, &opened.inspection().document.assets).is_empty()
            );
            assert_evidence_has_snapshot(&fixture.root);
            assert!(
                Store::new(fixture.root.clone())
                    .open(OpenChoice::Committed(current))
                    .is_ok()
            );

            // Retrying on the same directory is safe and the project still opens.
            Store::new(fixture.root.clone())
                .collect(&RetentionPolicy {
                    unconfirmed: UnconfirmedSnapshots::Remove,
                    ..policy
                })
                .unwrap();
            assert_eq!(
                Store::new(fixture.root.clone())
                    .open(OpenChoice::Current)
                    .unwrap()
                    .document(),
                &fixture.b
            );
            assert_evidence_has_snapshot(&fixture.root);
        }
    }
}

#[test]
fn retention_keeps_unconfirmed_snapshots_by_default_and_refuses_an_unmet_bound() {
    let fixture = Fixture::new();
    fixture.seed();
    let mut failed = Store::with_fs(
        fixture.root.clone(),
        FaultFs {
            fault: Some(Fault::Publish),
            ..FaultFs::default()
        },
    );
    let CommitState::Unknown(candidate) = failed
        .save(&fixture.b, &fixture.sources_b)
        .unwrap_err()
        .commit
    else {
        panic!("fixture must fail just before publish")
    };
    assert!(fixture.root.join(candidate.relative_path()).exists());

    let policy = RetentionPolicy {
        history_bytes: 0,
        ..RetentionPolicy::default()
    };
    let plan = Store::new(fixture.root.clone())
        .retention_plan(&policy)
        .unwrap();
    assert!(plan.over_budget);
    assert!(plan.retained.contains(&candidate));
    assert!(
        !plan
            .snapshots
            .iter()
            .any(|r| r.path == fixture.root.join(candidate.relative_path()))
    );
    assert!(plan.media.is_empty(), "kept legacy media stays protected");
    let before = tree(&fixture.root);
    assert_eq!(
        Store::new(fixture.root.clone())
            .collect(&RetentionPolicy {
                require_bound: true,
                ..policy
            })
            .unwrap_err(),
        StoreError::RetentionBoundUnmet
    );
    assert_eq!(tree(&fixture.root), before);
    let receipt = Store::new(fixture.root.clone()).collect(&policy).unwrap();
    assert_eq!(receipt.bytes, 0);
    assert!(receipt.over_budget);
    assert!(fixture.root.join(candidate.relative_path()).exists());

    // The explicit decision permits removing the unconfirmed snapshot and its
    // exclusively referenced media; the current document is untouched.
    let remove = RetentionPolicy {
        unconfirmed: UnconfirmedSnapshots::Remove,
        ..policy
    };
    let plan = Store::new(fixture.root.clone())
        .retention_plan(&remove)
        .unwrap();
    assert!(!plan.over_budget);
    assert!(
        plan.snapshots
            .iter()
            .any(|r| r.path == fixture.root.join(candidate.relative_path()))
    );
    let receipt = Store::new(fixture.root.clone()).collect(&remove).unwrap();
    assert!(receipt.removed_snapshots >= 1);
    assert!(!fixture.root.join(candidate.relative_path()).exists());
    assert_eq!(
        Store::new(fixture.root.clone())
            .open(OpenChoice::Current)
            .unwrap()
            .document(),
        &fixture.a
    );
}

#[test]
fn retention_protects_the_d6_recovery_root_and_its_record() {
    let fixture = Fixture::new();
    let saved = fixture.seed();
    let base = fixture.base.join("recovery");
    fs::create_dir(&base).unwrap();
    let mut first_edit = fixture.a.clone();
    first_edit.pads[0].gain = 0.21;
    let first = session::autosave_copy(
        &base,
        &fixture.root,
        saved,
        recovery_snapshot(first_edit.clone(), &fixture.sources_a),
    )
    .unwrap();
    let mut second_edit = fixture.a.clone();
    second_edit.pads[0].gain = 0.22;
    let second = session::autosave_copy(
        &base,
        &fixture.root,
        saved,
        recovery_snapshot(second_edit.clone(), &fixture.sources_a),
    )
    .unwrap();
    let copy_root = session::recovery_copy_root(&base, &fixture.root).unwrap();
    let copy_before = tree(&copy_root);
    let saved_before = tree(&fixture.root);

    // Collection on the saved project never reaches the machine copy root.
    let receipt = Store::new(fixture.root.clone())
        .collect(&RetentionPolicy::default())
        .unwrap();
    assert_eq!(receipt.bytes, 0);
    assert_eq!(tree(&copy_root), copy_before);
    assert_eq!(tree(&fixture.root), saved_before);

    // The copy root is a store of its own: its current copy and autosave record
    // survive, while older autosave generations are bounded.
    let bounded = RetentionPolicy {
        history_bytes: 0,
        ..RetentionPolicy::default()
    };
    let plan = Store::new(copy_root.clone())
        .retention_plan(&bounded)
        .unwrap();
    assert!(plan.retained.contains(&second.generation));
    assert!(!plan.retained.contains(&first.generation));
    assert!(
        plan.snapshots
            .iter()
            .any(|r| r.path == copy_root.join(first.generation.relative_path()))
    );
    assert!(
        !plan
            .snapshots
            .iter()
            .any(|r| r.path == copy_root.join(second.generation.relative_path()))
    );
    assert!(
        plan.unknown
            .iter()
            .any(|path| path == &copy_root.join(session::AUTOSAVE_FILE))
    );
    let receipt = Store::new(copy_root.clone()).collect(&bounded).unwrap();
    assert!(receipt.removed_snapshots >= 1);
    assert!(copy_root.join(session::AUTOSAVE_FILE).exists());
    assert_eq!(
        Store::new(copy_root.clone())
            .inspect_committed()
            .unwrap()
            .unwrap()
            .generation,
        second.generation
    );
    let offer = session::recovery_copy(&base, &fixture.root, saved)
        .unwrap()
        .unwrap();
    assert_eq!(offer.offered, Some(second.generation));
    assert_eq!(tree(&fixture.root), saved_before);
}

#[test]
fn collection_is_refused_while_the_writer_lock_is_held() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    let mut fs = SystemFs;
    let lock = fs
        .lock(&fixture.root.join(".brownnote-store/lock"))
        .unwrap();
    assert_eq!(
        Store::new(fixture.root.clone())
            .collect(&RetentionPolicy::default())
            .unwrap_err(),
        StoreError::Io(io::ErrorKind::WouldBlock)
    );
    assert_eq!(
        Store::new(fixture.root.clone())
            .inspect_committed()
            .unwrap()
            .unwrap()
            .generation,
        first
    );
    drop(lock);
    let receipt = Store::new(fixture.root.clone())
        .collect(&RetentionPolicy::default())
        .unwrap();
    assert_eq!(receipt.bytes, 0);
}

fn take_completion(job: &session::Job) -> Result<session::Completion, session::Error> {
    loop {
        if let Some(result) = job.take() {
            return result;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn project_worker_reports_the_retention_plan_and_reclaimed_bytes() {
    let fixture = Fixture::new();
    fixture.seed();
    Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap();
    let orphan = wav(4_444);
    let orphan_ref = AssetRef::of(&orphan);
    ProjectMedia::new(fixture.root.clone())
        .publish(&orphan_ref, &orphan)
        .unwrap();

    let service = session::Service::new().unwrap();
    let policy = RetentionPolicy::default();
    let job = service
        .start(session::Request::Retention {
            root: fixture.root.clone(),
            policy,
            apply: false,
        })
        .unwrap();
    let session::Completion::Retained { plan, receipt } = take_completion(&job).unwrap() else {
        panic!("unexpected completion")
    };
    assert!(receipt.is_none());
    assert_eq!(plan.reclaimable_bytes, orphan.len() as u64);

    let job = service
        .start(session::Request::Retention {
            root: fixture.root.clone(),
            policy,
            apply: true,
        })
        .unwrap();
    let session::Completion::Retained { receipt, .. } = take_completion(&job).unwrap() else {
        panic!("unexpected completion")
    };
    let receipt = receipt.unwrap();
    assert_eq!(receipt.bytes, orphan.len() as u64);
    assert!(!fixture.root.join(orphan_ref.id.relative_path()).exists());
}

#[test]
fn retention_keeps_every_offered_generation_openable_and_retires_only_the_rest() {
    let fixture = Fixture::new();
    let first = fixture.seed();
    let second = Store::new(fixture.root.clone())
        .save(&fixture.b, &fixture.sources_b)
        .unwrap()
        .generation;
    let mut third_document = fixture.b.clone();
    third_document.pads[0].gain = 0.42;
    let third = Store::new(fixture.root.clone())
        .save(&third_document, &[])
        .unwrap()
        .generation;

    let policy = RetentionPolicy::default();
    let options = Store::new(fixture.root.clone())
        .recovery_options_with(&policy)
        .unwrap();
    let mut expected = vec![first, second, third];
    expected.sort_by_key(ToString::to_string);
    assert_eq!(options.recoverable, expected);
    let plan = Store::new(fixture.root.clone())
        .retention_plan(&policy)
        .unwrap();
    assert!(plan.media.is_empty(), "every byte is referenced");
    assert_eq!(plan.reclaimable_bytes, 0);
    assert_eq!(
        Store::new(fixture.root.clone())
            .collect(&policy)
            .unwrap()
            .bytes,
        0
    );
    for id in &options.recoverable {
        assert!(
            Store::new(fixture.root.clone())
                .open(OpenChoice::Committed(*id))
                .is_ok()
        );
    }

    let bounded = RetentionPolicy {
        history_bytes: 0,
        ..RetentionPolicy::default()
    };
    let offered = Store::new(fixture.root.clone())
        .recovery_options_with(&bounded)
        .unwrap();
    assert_eq!(offered.recoverable, vec![third]);
    let plan = Store::new(fixture.root.clone())
        .retention_plan(&bounded)
        .unwrap();
    assert!(
        plan.snapshots
            .iter()
            .any(|r| r.path == fixture.root.join(first.relative_path()))
    );
    assert!(
        plan.snapshots
            .iter()
            .any(|r| r.path == fixture.root.join(second.relative_path()))
    );
    let receipt = Store::new(fixture.root.clone()).collect(&bounded).unwrap();
    assert!(receipt.removed_snapshots >= 2);
    assert_evidence_has_snapshot(&fixture.root);
    for id in &offered.recoverable {
        assert!(
            Store::new(fixture.root.clone())
                .open(OpenChoice::Committed(*id))
                .is_ok()
        );
    }
    assert!(
        Store::new(fixture.root.clone())
            .open(OpenChoice::Current)
            .is_ok()
    );
}
