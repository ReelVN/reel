//! Process-owned save IO. Storage workers never receive a VM or native device.
use crate::aside_session::AsideSessionUi;
use crate::assets::worker::Worker;
use reel_core::{budget::QueueLimits, identity::Priority, wake::WakeSignal};
use reel_engine::aside::{
    AdapterError,
    data::DataDocument,
    saves::SaveRequest,
    snapshot::{OwnedSessionRestore, SessionCapture, SnapshotBudget},
};
use reel_engine::aside::{
    saves::{SaveRequestId, SaveSlot},
    snapshot::SessionSnapshot,
};
use std::{
    fs,
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

type CaptureCursor<'a> = Option<SessionCapture<'a>>;
self_cell::self_cell! {
    struct CaptureCell {
        owner: AsideSessionUi,
        #[not_covariant]
        dependent: CaptureCursor,
    }
}
pub(crate) struct Capture {
    cell: CaptureCell,
    pub bytes: u64,
}
impl Capture {
    // Admission returns the original live driver without an extra allocation.
    #[allow(clippy::result_large_err)]
    pub fn new(driver: AsideSessionUi) -> Result<Self, (AsideSessionUi, AdapterError)> {
        CaptureCell::try_new_or_recover(driver, |driver| driver.snapshot_begin().map(Some))
            .map(|cell| Self { cell, bytes: 0 })
    }
    pub fn owner(&self) -> &AsideSessionUi {
        self.cell.borrow_owner()
    }
    pub fn cancel(self) -> AsideSessionUi {
        self.cell.into_owner()
    }
    pub fn step(&mut self, budget: SnapshotBudget) -> Result<bool, AdapterError> {
        let progress = self
            .cell
            .with_dependent_mut(|_, cursor| cursor.as_mut().unwrap().step(budget))?;
        self.bytes += progress.bytes;
        Ok(progress.complete)
    }
    pub fn finish(mut self) -> Result<(AsideSessionUi, SessionSnapshot), AdapterError> {
        let snapshot = self
            .cell
            .with_dependent_mut(|_, cursor| cursor.take().unwrap().finish())?;
        Ok((self.cell.into_owner(), snapshot))
    }
}

/// Storage is injected independently of the window and audio backend.
pub struct RuntimeSaves {
    pub(crate) service: SaveService,
    pub(crate) max_snapshot_bytes: u64,
    pub(crate) pending: Option<SaveRequest>,
    pub(crate) restore: Option<OwnedSessionRestore>,
    pub(crate) candidate: Option<Candidate>,
    pub(crate) next_epoch: u64,
}
impl RuntimeSaves {
    pub fn new(service: SaveService, max_snapshot_bytes: u64) -> Self {
        Self {
            service,
            max_snapshot_bytes,
            pending: None,
            restore: None,
            candidate: None,
            next_epoch: 0,
        }
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some()
            || self.restore.is_some()
            || self.candidate.is_some()
            || !self.service.is_idle()
    }
}
pub(crate) struct Candidate {
    pub driver: AsideSessionUi,
    pub bridge: reel_ui::bridge::PresentationBridge,
    pub root: reel_ui::core::WidgetId,
}
impl Candidate {
    pub fn new(driver: AsideSessionUi) -> Result<Self, Box<dyn std::error::Error>> {
        use reel_ui::{
            app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
            core::Widget,
            dpi::PhysicalSize,
        };
        let widget = driver.widget().prepare();
        let root = widget.id();
        let [width, height] = driver
            .adapter()
            .game_settings()
            .canvas_size
            .unwrap_or([1280, 720]);
        let tree = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(Default::default()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(width, height),
                scale_factor: 1.,
                default_font: None,
            },
        );
        let bridge = reel_ui::bridge::PresentationBridge::new(
            tree,
            reel_core::identity::SurfaceEpoch(1),
            reel_ui::bridge::BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 8 << 20 },
                max_candidates: 1,
                max_bridge_bytes: 1 << 20,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .map_err(|error| format!("restored stage layout: {error:?}"))?;
        Ok(Self {
            driver,
            bridge,
            root,
        })
    }
}

pub type StorageError = Box<dyn std::error::Error + Send + Sync>;

/// Storage implementations run on the worker. A successful write has published
/// the whole slot; errors before publication preserve the preceding slot.
pub trait SaveStorage: Send + Sync + 'static {
    fn write(
        &self,
        slot: SaveSlot,
        snapshot: &SessionSnapshot,
        thumbnail: Option<&[u8]>,
    ) -> Result<(), StorageError>;
    fn thumbnail(&self, slot: &str) -> Result<Option<Vec<u8>>, StorageError>;
    fn read(&self, slot: &str) -> Result<SessionSnapshot, StorageError>;
    /// Copy a committed slot to `destination`, preserving its snapshot, title,
    /// timestamp and thumbnail. Publish replacements atomically. Equal identifiers
    /// validate the source and succeed without writing; a missing source fails.
    fn copy(&self, source: &str, destination: &str) -> Result<(), StorageError>;
    fn delete(&self, slot: &str) -> Result<(), StorageError>;
    fn list(&self) -> Result<Vec<SaveSlot>, StorageError>;
    fn write_data(&self, key: &str, data: &DataDocument) -> Result<(), StorageError>;
    fn read_data(&self, key: &str) -> Result<Option<DataDocument>, StorageError>;
    fn delete_data(&self, key: &str) -> Result<(), StorageError>;
}

/// Host-selected storage directory, independent of the game's read-only assets.
pub struct DirectorySaves {
    root: PathBuf,
    max_snapshot_bytes: u64,
}
impl DirectorySaves {
    pub fn new(root: impl Into<PathBuf>, max_snapshot_bytes: u64) -> Self {
        Self {
            root: root.into(),
            max_snapshot_bytes,
        }
    }
    fn path(&self, slot: &str) -> PathBuf {
        // Slot names are logical keys, including Unicode and slashes. They never
        // become paths supplied by script or escape the host-selected directory.
        self.root
            .join(format!("{}.save", blake3::hash(slot.as_bytes()).to_hex()))
    }
    fn data_path(&self, key: &str) -> PathBuf {
        self.root
            .join(format!("{}.data", blake3::hash(key.as_bytes()).to_hex()))
    }
    fn metadata(&self, file: &mut fs::File) -> Result<SaveSlot, StorageError> {
        let mut magic = [0; 8];
        file.read_exact(&mut magic)?;
        if &magic != b"REELSLOT" {
            return Err("unsupported slot format".into());
        }
        let mut len = [0; 8];
        file.read_exact(&mut len)?;
        let len = u64::from_le_bytes(len);
        if len > self.max_snapshot_bytes {
            return Err("slot metadata exceeds storage budget".into());
        }
        let mut bytes = Vec::new();
        file.take(len).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != len {
            return Err("truncated slot metadata".into());
        }
        let ((id, title, saved_at), tail) = postcard::take_from_bytes(&bytes)?;
        if !tail.is_empty() {
            return Err("trailing slot metadata".into());
        }
        Ok(SaveSlot {
            id,
            title,
            saved_at,
            has_thumbnail: self.thumbnail_range(file)?.is_some(),
        })
    }
    // An optional indexed attachment follows the gzip stream. Listing only reads
    // metadata and this fixed-size footer; loading the snapshot never reads PNG data.
    fn thumbnail_range(&self, file: &mut fs::File) -> Result<Option<(u64, u64)>, StorageError> {
        let position = file.stream_position()?;
        let footer = file.seek(SeekFrom::End(-16))?;
        let mut index = [0; 16];
        file.read_exact(&mut index)?;
        file.seek(SeekFrom::Start(position))?;
        if &index[8..] != b"REELTHMB" {
            return Ok(None);
        }
        let len = u64::from_le_bytes(index[..8].try_into().unwrap());
        if len > self.max_snapshot_bytes {
            return Err("thumbnail exceeds storage budget".into());
        }
        let start = footer
            .checked_sub(len)
            .filter(|start| *start >= position)
            .ok_or("invalid thumbnail range")?;
        Ok(Some((start, len)))
    }
}
impl SaveStorage for DirectorySaves {
    fn copy(&self, source: &str, destination: &str) -> Result<(), StorageError> {
        let mut input = reel_storage::open(&self.path(source))?;
        let slot = self.metadata(&mut input)?;
        if slot.id != source {
            return Err("slot identity mismatch".into());
        }
        if source == destination {
            return Ok(());
        }
        let metadata = postcard::to_allocvec(&(destination, &slot.title, slot.saved_at))?;
        if metadata.len() as u64 > self.max_snapshot_bytes {
            return Err("slot metadata exceeds storage budget".into());
        }
        let published = reel_storage::publish(&self.path(destination), true, |output| {
            output.write_all(b"REELSLOT")?;
            output.write_all(&(metadata.len() as u64).to_le_bytes())?;
            output.write_all(&metadata)?;
            let mut scratch = [0; 64 << 10];
            loop {
                let read = input.read(&mut scratch)?;
                if read == 0 {
                    break;
                }
                output.write_all(&scratch[..read])?;
            }
            Ok(())
        })?;
        publication_warnings(published);
        Ok(())
    }

    fn write_data(&self, key: &str, data: &DataDocument) -> Result<(), StorageError> {
        fs::create_dir_all(&self.root)?;
        let published = reel_storage::publish(&self.data_path(key), true, |file| {
            file.write_all(b"REELDATA\x01")?;
            let compressed = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut writer = LimitedWriter {
                inner: BufWriter::new(compressed),
                remaining: self.max_snapshot_bytes,
            };
            data.write_to(&mut writer).map_err(io::Error::other)?;
            writer
                .inner
                .into_inner()
                .map_err(|e| e.into_error())?
                .finish()?;
            Ok(())
        })?;
        publication_warnings(published);
        Ok(())
    }
    fn read_data(&self, key: &str) -> Result<Option<DataDocument>, StorageError> {
        let mut file = match reel_storage::open(&self.data_path(key)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut magic = [0; 9];
        file.read_exact(&mut magic)?;
        if &magic != b"REELDATA\x01" {
            return Err("unsupported system record format".into());
        }
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(file)
            .take(self.max_snapshot_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > self.max_snapshot_bytes {
            return Err("system record exceeds storage budget".into());
        }
        let data = DataDocument::decode(&bytes)?;
        if data.retained_bytes() > self.max_snapshot_bytes {
            return Err("system data exceeds storage budget".into());
        }
        Ok(Some(data))
    }
    fn delete_data(&self, key: &str) -> Result<(), StorageError> {
        match reel_storage::remove(&self.data_path(key)) {
            Ok(published) => {
                publication_warnings(published);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    fn write(
        &self,
        slot: SaveSlot,
        snapshot: &SessionSnapshot,
        thumbnail: Option<&[u8]>,
    ) -> Result<(), StorageError> {
        if thumbnail.is_some_and(|bytes| !bytes.starts_with(b"\x89PNG\r\n\x1a\n")) {
            return Err("save thumbnail requires PNG data".into());
        }
        let thumbnail = thumbnail.unwrap_or_default();
        if thumbnail.len() as u64 > self.max_snapshot_bytes {
            return Err("thumbnail exceeds storage budget".into());
        }
        fs::create_dir_all(&self.root)?;
        let metadata = postcard::to_allocvec(&(&slot.id, &slot.title, slot.saved_at))?;
        let published = reel_storage::publish(&self.path(&slot.id), true, |file| {
            file.write_all(b"REELSLOT")?;
            file.write_all(&(metadata.len() as u64).to_le_bytes())?;
            file.write_all(&metadata)?;
            let compressed = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut writer = LimitedWriter {
                // Postcard emits small scalar writes. Batch those before compression,
                // while charging the original uncompressed byte limit.
                inner: BufWriter::new(compressed),
                remaining: self.max_snapshot_bytes,
            };
            snapshot.write_to(&mut writer).map_err(io::Error::other)?;
            let file = writer
                .inner
                .into_inner()
                .map_err(|e| e.into_error())?
                .finish()?;
            if !thumbnail.is_empty() {
                file.write_all(thumbnail)?;
                file.write_all(&(thumbnail.len() as u64).to_le_bytes())?;
                file.write_all(b"REELTHMB")?;
            }
            Ok(())
        })?;
        publication_warnings(published);
        Ok(())
    }
    fn thumbnail(&self, slot: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let mut file = reel_storage::open(&self.path(slot))?;
        if self.metadata(&mut file)?.id != slot {
            return Err("slot identity mismatch".into());
        }
        let Some((start, len)) = self.thumbnail_range(&mut file)? else {
            return Ok(None);
        };
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![0; len as usize];
        file.read_exact(&mut bytes)?;
        Ok(Some(bytes))
    }
    fn read(&self, slot: &str) -> Result<SessionSnapshot, StorageError> {
        let mut file = reel_storage::open(&self.path(slot))?;
        if self.metadata(&mut file)?.id != slot {
            return Err("slot identity mismatch".into());
        }
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(file)
            .take(self.max_snapshot_bytes.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > self.max_snapshot_bytes {
            return Err("snapshot exceeds storage budget".into());
        }
        Ok(SessionSnapshot::decode(&bytes)?)
    }
    fn delete(&self, slot: &str) -> Result<(), StorageError> {
        match reel_storage::remove(&self.path(slot)) {
            Ok(published) => {
                publication_warnings(published);
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    fn list(&self) -> Result<Vec<SaveSlot>, StorageError> {
        let entries = match reel_storage::entries(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut slots = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.extension().is_some_and(|e| e == "save") {
                slots.push(self.metadata(&mut reel_storage::open(&entry)?)?);
            }
        }
        slots.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(slots)
    }
}
fn publication_warnings(published: reel_storage::Published) {
    for warning in [published.durability_warning, published.cleanup_warning]
        .into_iter()
        .flatten()
    {
        eprintln!("{warning}");
    }
}
struct LimitedWriter<W> {
    inner: W,
    remaining: u64,
}
impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other("stored data exceeds storage budget"));
        }
        let n = self.inner.write(bytes)?;
        self.remaining -= n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub enum StorageJob {
    /// Stream an existing slot into an atomically published destination.
    Copy {
        source: String,
        destination: String,
    },
    Save {
        slot: String,
        title: String,
        thumbnail: Option<Arc<DataDocument>>,
        snapshot: Box<SessionSnapshot>,
    },
    Load(String),
    Delete(String),
    List,
    Thumbnail(String),
    ReadData(String),
    WriteData(String, Arc<DataDocument>),
    DeleteData(String),
}
pub enum StorageResult {
    Saved,
    Loaded(Box<SessionSnapshot>),
    Deleted,
    Slots(Vec<SaveSlot>),
    Data(Option<Arc<DataDocument>>),
}
pub struct StorageEvent {
    pub request: SaveRequestId,
    pub result: Result<StorageResult, String>,
}
/// One ordered lane ensures writes, deletes and reads observe submission order.
/// Keep this process service alive across session replacement. Drain accepted
/// operations before shutting down; dropping it cancels unstarted work.
pub struct SaveService {
    worker: Worker<(SaveRequestId, StorageJob), StorageEvent>,
    next: u64,
    wake: WakeSignal,
}
impl SaveService {
    pub fn new(storage: Arc<dyn SaveStorage>, limits: QueueLimits, wake: WakeSignal) -> Self {
        let worker = Worker::new(
            limits,
            wake.clone(),
            move |(request, job): (SaveRequestId, StorageJob), _| {
                let result = (|| -> Result<StorageResult, StorageError> {
                    Ok(match job {
                        StorageJob::Save {
                            slot,
                            title,
                            thumbnail,
                            mut snapshot,
                        } => {
                            snapshot.commit_save(request)?;
                            let saved_at = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                            storage.write(
                                SaveSlot {
                                    id: slot,
                                    title,
                                    saved_at,
                                    has_thumbnail: thumbnail.is_some(),
                                },
                                &snapshot,
                                thumbnail.as_ref().and_then(|data| data.as_bytes()),
                            )?;
                            StorageResult::Saved
                        }
                        StorageJob::Load(slot) => {
                            StorageResult::Loaded(Box::new(storage.read(&slot)?))
                        }
                        StorageJob::Copy {
                            source,
                            destination,
                        } => {
                            storage.copy(&source, &destination)?;
                            StorageResult::Saved
                        }
                        StorageJob::Delete(slot) => {
                            storage.delete(&slot)?;
                            StorageResult::Deleted
                        }
                        StorageJob::List => StorageResult::Slots(storage.list()?),
                        StorageJob::Thumbnail(slot) => StorageResult::Data(
                            storage
                                .thumbnail(&slot)?
                                .map(|bytes| Arc::new(DataDocument::from_bytes(bytes))),
                        ),
                        StorageJob::ReadData(key) => {
                            StorageResult::Data(storage.read_data(&key)?.map(Arc::new))
                        }
                        StorageJob::WriteData(key, data) => {
                            storage.write_data(&key, &data)?;
                            StorageResult::Saved
                        }
                        StorageJob::DeleteData(key) => {
                            storage.delete_data(&key)?;
                            StorageResult::Deleted
                        }
                    })
                })()
                .map_err(|error| error.to_string());
                StorageEvent { request, result }
            },
            |event| event.result = Err("storage service closed".into()),
        );
        Self {
            worker,
            next: 0,
            wake,
        }
    }
    pub fn submit(
        &mut self,
        request: SaveRequestId,
        job: StorageJob,
        retained_bytes: u64,
    ) -> Result<(), reel_core::result::Rejected<(SaveRequestId, StorageJob)>> {
        self.next += 1;
        self.worker
            .accept(self.next, (request, job), retained_bytes, Priority::Normal)
    }
    pub fn drain(&mut self, events: &mut Vec<StorageEvent>, limit: usize) {
        self.wake.acknowledge();
        self.worker.drain(events, limit);
    }
    pub fn is_idle(&self) -> bool {
        self.worker.is_idle()
    }
}

/// Explicit game identity prevents two installed games from sharing slots.
/// The shell chooses the platform's user-data root, never the asset directory.
pub fn game_save_directory(user_data: &Path, game_id: &str) -> PathBuf {
    user_data
        .join("Reel")
        .join(blake3::hash(game_id.as_bytes()).to_hex().as_str())
        .join("saves")
}

/// Resolve a game's default save directory from its asset-root identity.
/// Existing roots retain their canonical identity. Missing path components are
/// appended to the nearest existing canonical ancestor without creating them;
/// other filesystem errors are returned to the caller.
pub fn default_save_directory(user_data: &Path, asset_root: &Path) -> io::Result<PathBuf> {
    let mut ancestor = std::path::absolute(asset_root)?;
    let mut missing = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut identity) => {
                if !missing.is_empty() && !fs::metadata(&identity)?.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotADirectory,
                        "asset root ancestor is not a directory",
                    ));
                }
                for component in missing.iter().rev() {
                    identity.push(component);
                }
                return Ok(game_save_directory(user_data, &identity.to_string_lossy()));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(component) = ancestor.file_name() else {
                    return Err(error);
                };
                missing.push(component.to_os_string());
                ancestor.pop();
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary() -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/save-path-tests")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn snapshot() -> SessionSnapshot {
        let mut driver = AsideSessionUi::from_source(
            reel_engine::Session::new(reel_engine::SessionId(1), reel_engine::Epoch(1)).unwrap(),
            "fn main() { narrator(\"checkpoint\"); }",
            "main",
            1,
            "main",
            "en",
            Default::default(),
        )
        .unwrap();
        for _ in 0..1000 {
            driver.tick(Default::default(), 256, || 0).unwrap();
            if driver.ui().session().view().interaction.is_some() {
                break;
            }
        }
        assert!(driver.ui().session().view().interaction.is_some());
        let mut capture = driver.snapshot_begin().unwrap();
        while !capture
            .step(SnapshotBudget { units: 256 })
            .unwrap()
            .complete
        {}
        capture.finish().unwrap()
    }

    #[test]
    fn slot_copy_preserves_snapshot_metadata_thumbnail_and_source() {
        let root = temporary();
        let storage = DirectorySaves::new(&root, 16 << 20);
        let snapshot = snapshot();
        let png = include_bytes!("../../../examples/assets/panels.png");
        let source = SaveSlot {
            id: "chapter/一".into(),
            title: "Original chapter".into(),
            saved_at: 42,
            has_thumbnail: true,
        };
        storage.write(source.clone(), &snapshot, Some(png)).unwrap();
        storage
            .write(
                SaveSlot {
                    id: "backup".into(),
                    title: "Old backup".into(),
                    saved_at: 99,
                    has_thumbnail: false,
                },
                &snapshot,
                None,
            )
            .unwrap();
        let original = fs::read(storage.path(&source.id)).unwrap();
        storage.copy(&source.id, "backup").unwrap();
        assert_eq!(fs::read(storage.path(&source.id)).unwrap(), original);
        assert_eq!(
            storage.read("backup").unwrap().encode().unwrap(),
            snapshot.encode().unwrap()
        );
        assert_eq!(
            storage.thumbnail("backup").unwrap().as_deref(),
            Some(png.as_slice())
        );
        let mut expected = source.clone();
        expected.id = "backup".into();
        assert_eq!(storage.list().unwrap(), [expected, source.clone()]);

        let mut input = fs::File::open(storage.path(&source.id)).unwrap();
        let mut output = fs::File::open(storage.path("backup")).unwrap();
        storage.metadata(&mut input).unwrap();
        storage.metadata(&mut output).unwrap();
        let mut source_payload = Vec::new();
        let mut copied_payload = Vec::new();
        input.read_to_end(&mut source_payload).unwrap();
        output.read_to_end(&mut copied_payload).unwrap();
        assert_eq!(copied_payload, source_payload);

        let copied = fs::read(storage.path("backup")).unwrap();
        storage.copy("backup", "backup").unwrap();
        assert_eq!(fs::read(storage.path("backup")).unwrap(), copied);
        assert!(storage.copy("missing", "backup").is_err());
        assert!(storage.copy("missing", "missing").is_err());
        assert_eq!(fs::read(storage.path("backup")).unwrap(), copied);
        fs::write(storage.path("corrupt"), b"invalid slot").unwrap();
        assert!(storage.copy("corrupt", "backup").is_err());
        assert_eq!(fs::read(storage.path("backup")).unwrap(), copied);

        let destination = "x".repeat(128);
        storage.copy("backup", &destination).unwrap();
        let original = fs::read(storage.path(&destination)).unwrap();
        let constrained = DirectorySaves::new(&root, 64);
        assert!(constrained.copy("backup", &destination).is_err());
        assert_eq!(fs::read(storage.path(&destination)).unwrap(), original);
    }

    #[test]
    fn missing_asset_root_keeps_default_save_identity_when_created() {
        let root = temporary();
        let assets = root.join("project/resources");
        let user_data = root.join("user-data");
        let before = default_save_directory(&user_data, &assets).unwrap();
        assert!(!assets.exists());
        assert!(!user_data.exists());

        fs::create_dir_all(&assets).unwrap();
        let expected = game_save_directory(
            &user_data,
            &assets.canonicalize().unwrap().to_string_lossy(),
        );
        assert_eq!(before, expected);
        assert_eq!(default_save_directory(&user_data, &assets).unwrap(), before);
        assert!(!user_data.exists());
    }

    #[test]
    fn existing_asset_root_retains_canonical_save_identity() {
        let root = temporary();
        let assets = root.join("resources");
        fs::create_dir(&assets).unwrap();
        let user_data = root.join("user-data");
        assert_eq!(
            default_save_directory(&user_data, &assets.join(".")).unwrap(),
            game_save_directory(
                &user_data,
                &assets.canonicalize().unwrap().to_string_lossy()
            )
        );
    }

    #[test]
    fn asset_root_below_file_reports_not_a_directory() {
        let root = temporary();
        let file = root.join("file");
        fs::write(&file, b"asset").unwrap();
        let error =
            default_save_directory(&root.join("user-data"), &file.join("resources")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory);
    }
}
