//! File-backed stage images. Only the worker touches files or decodes pixels.
#[cfg(test)]
mod tests;
use image::{DynamicImage, ImageDecoder, ImageReader};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, ContentVersion, ResourceVersion},
};
use reel_ui::{
    peniko::{ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceLease, ResourceRegistry},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
};

/// Resource directory and memory policy, chosen by the player host.
#[derive(Clone, Debug)]
pub struct ImageOptions {
    pub root: PathBuf,
    /// Shared RGBA storage, including pixels retained by old submitted frames.
    pub resident_bytes: u64,
    /// One decoder's output/conversion allowance and image crate scratch limit.
    pub decode_bytes: u64,
}
impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            root: ".".into(),
            resident_bytes: 256 << 20,
            decode_bytes: 256 << 20,
        }
    }
}

struct Pending {
    source: String,
    cancelled: Arc<AtomicBool>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

struct DecodeRequest {
    path: PathBuf,
    cancelled: Arc<AtomicBool>,
}
struct Worker {
    submit: SyncSender<DecodeRequest>,
    results: Receiver<Result<ResourceLease, String>>,
}

pub(crate) struct Images {
    options: ImageOptions,
    pub(crate) pool: BudgetPool,
    work: BudgetPool,
    pub(crate) loaded: BTreeMap<String, ResourceLease>,
    wanted: BTreeMap<String, Option<String>>,
    displayed: BTreeSet<String>,
    pending: Option<Pending>,
    worker: Option<Worker>,
    failure: Option<String>,
    wake: Arc<dyn Fn() + Send + Sync>,
    closed: bool,
    #[cfg(test)]
    pub(crate) loads: u64,
}
impl Images {
    pub fn new(options: ImageOptions) -> Self {
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: u32::MAX,
                max_bytes: options.resident_bytes,
            },
        );
        let work = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 1,
                max_bytes: options.decode_bytes,
            },
        );
        Self {
            options,
            pool,
            work,
            loaded: BTreeMap::new(),
            wanted: BTreeMap::new(),
            displayed: BTreeSet::new(),
            pending: None,
            worker: None,
            failure: None,
            wake: Arc::new(|| {}),
            closed: false,
            #[cfg(test)]
            loads: 0,
        }
    }
    pub fn set_waker(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.wake = wake;
    }
    #[cfg(feature = "window")]
    pub fn has_result_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn request(&mut self, wanted: BTreeMap<String, Option<String>>) {
        if self.closed {
            return;
        }
        if let Some(pending) = &self.pending
            && !wanted.contains_key(&pending.source)
        {
            pending.cancelled.store(true, Ordering::Release);
        }
        self.wanted = wanted;
        self.failure = None;
        self.loaded.retain(|source, lease| {
            let retain = self.wanted.contains_key(source) || self.displayed.contains(source);
            if !retain {
                lease.prohibit_new_leases();
            }
            retain
        });
    }
    /// Returns true when all images for the current scene can be published.
    pub fn poll(&mut self) -> Result<bool, String> {
        if self.closed {
            return Ok(false);
        }
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        if self.pending.is_some() {
            let result = match self.worker.as_ref().unwrap().results.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("image worker terminated".into())),
            };
            if let Some(result) = result {
                let pending = self.pending.take().unwrap();
                if !pending.cancelled.load(Ordering::Acquire)
                    && let Some(origin) = self.wanted.get(&pending.source)
                {
                    match result {
                        Ok(lease) => {
                            self.loaded.insert(pending.source.clone(), lease);
                        }
                        Err(error) => {
                            let failure = format!(
                                "{}: image {:?}: {error}",
                                origin.as_deref().unwrap_or("stage"),
                                pending.source
                            );
                            self.failure = Some(failure.clone());
                            return Err(failure);
                        }
                    }
                }
            }
        }
        let missing = self
            .wanted
            .keys()
            .find(|source| !self.loaded.contains_key(*source));
        if let Some(source) = missing {
            if self.pending.is_none() {
                let source = source.clone();
                let path = self.options.root.join(&source);
                let cancelled = Arc::new(AtomicBool::new(false));
                if self.worker.is_none() {
                    self.start_worker()?;
                }
                self.worker
                    .as_ref()
                    .unwrap()
                    .submit
                    .try_send(DecodeRequest {
                        path,
                        cancelled: cancelled.clone(),
                    })
                    .map_err(|error| error.to_string())?;
                self.pending = Some(Pending { source, cancelled });
                #[cfg(test)]
                {
                    self.loads += 1;
                }
            }
            Ok(false)
        } else {
            Ok(true)
        }
    }
    fn start_worker(&mut self) -> Result<(), String> {
        let (submit, requests) = mpsc::sync_channel::<DecodeRequest>(1);
        let (send, results) = mpsc::sync_channel(1);
        let pool = self.pool.clone();
        let work = self.work.clone();
        let decode_bytes = self.options.decode_bytes;
        let wake = self.wake.clone();
        std::thread::Builder::new()
            .name("reel-image".into())
            .spawn(move || {
                // Also wake on a decoder panic or normal disconnect, so a live
                // host can observe the closed completion channel.
                struct WakeOnExit {
                    send: Option<SyncSender<Result<ResourceLease, String>>>,
                    wake: Arc<dyn Fn() + Send + Sync>,
                }
                impl Drop for WakeOnExit {
                    fn drop(&mut self) {
                        self.send = None;
                        (self.wake)();
                    }
                }
                let output = WakeOnExit {
                    send: Some(send),
                    wake: wake.clone(),
                };
                while let Ok(request) = requests.recv() {
                    let result =
                        decode(request.path, &request.cancelled, &pool, &work, decode_bytes)
                            .map_err(|error| error.to_string());
                    if output.send.as_ref().unwrap().send(result).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .map_err(|error| error.to_string())?;
        self.worker = Some(Worker { submit, results });
        Ok(())
    }
    /// Retire obsolete producer ownership only after the replacement is ready.
    pub fn commit(&mut self) {
        self.displayed = self.wanted.keys().cloned().collect();
        self.loaded.retain(|source, lease| {
            let retain = self.wanted.contains_key(source);
            if !retain {
                lease.prohibit_new_leases();
            }
            retain
        });
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.pending = None;
        self.worker = None;
        self.wanted.clear();
        self.commit();
    }
}
impl Drop for Images {
    fn drop(&mut self) {
        self.close();
    }
}

// Cancellation is checked at the file boundary, including decoder seeks.
// Codec CPU work already in progress finishes off-thread and cannot publish.
struct ImageFile<'a> {
    file: File,
    cancelled: &'a AtomicBool,
}
impl ImageFile<'_> {
    fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(io::Error::other("image load cancelled"))
        } else {
            Ok(())
        }
    }
}
impl Read for ImageFile<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        self.file.read(bytes)
    }
}
impl Seek for ImageFile<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.file.seek(position)
    }
}

fn decode(
    path: PathBuf,
    cancelled: &AtomicBool,
    pool: &BudgetPool,
    work: &BudgetPool,
    decode_bytes: u64,
) -> Result<ResourceLease, Box<dyn std::error::Error>> {
    let input = ImageFile {
        file: File::open(&path)?,
        cancelled,
    };
    input.check()?;
    let mut reader = ImageReader::new(BufReader::new(input)).with_guessed_format()?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Err("expected PNG or JPEG image".into());
    }
    let mut limits = image::Limits::no_limits();
    limits.max_alloc = Some(decode_bytes);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    let rgba_bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(4))
        .ok_or("image dimensions overflow")?;
    let output_bytes = decoder.total_bytes();
    let _working = work
        .reserve(
            output_bytes
                .checked_add(rgba_bytes)
                .ok_or("image size overflow")?,
        )
        .map_err(|error| format!("image decode budget: {error:?}"))?;
    let mut reservation = pool
        .reserve(rgba_bytes)
        .map_err(|error| format!("image resident budget: {error:?}"))?;
    let pixels = DynamicImage::from_decoder(decoder)?.into_rgba8().into_raw();
    if cancelled.load(Ordering::Acquire) {
        return Err("image load cancelled".into());
    }
    reservation
        .resize(pixels.capacity() as u64)
        .map_err(|error| format!("image resident budget: {error:?}"))?;
    let pixels =
        OwnedBytes::new(pixels, reservation).map_err(|error| format!("image pixels: {error:?}"))?;
    let mut hash = blake3::Hasher::new();
    hash.update(&width.to_le_bytes());
    hash.update(&height.to_le_bytes());
    hash.update(pixels.as_slice());
    let version = ResourceVersion {
        key: AssetKey(path.to_string_lossy().into_owned()),
        version: ContentVersion(*hash.finalize().as_bytes()),
    };
    // This immutable producer needs one identity, with descriptor storage sized
    // from its path. Scene size is governed by memory, not a fixed slot count.
    let mut registry = ResourceRegistry::new(1, 1024 + version.key.0.capacity() as u64)
        .map_err(|error| format!("image descriptor: {error:?}"))?;
    let handle = registry
        .register_image(
            version,
            pixels,
            [width, height],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .map_err(|error| format!("image resource: {error:?}"))?;
    registry
        .acquire(handle)
        .map_err(|error| format!("image lease: {error:?}").into())
}
