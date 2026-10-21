//! Prepared frame storage shared by the existing queued transport.
use crate::Frame;
use std::sync::{
	Arc, Mutex,
	atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// Lightweight stream facts that survive PCM retirement without retaining its worker.
pub struct StreamStatus {
	length: AtomicUsize,
	failed: AtomicBool,
	error: Mutex<Option<String>>,
}
impl StreamStatus {
	/// Create facts with the known length, or usize::MAX until EOF.
	pub fn new(length: usize) -> Self {
		Self {
			length: AtomicUsize::new(length),
			failed: AtomicBool::new(false),
			error: Mutex::new(None),
		}
	}
	/// Known frame count, or usize::MAX before EOF.
	pub fn len(&self) -> usize {
		self.length.load(Ordering::Acquire)
	}
	/// Whether the known source is empty.
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}
	/// Publish the source's actual EOF on the worker.
	pub fn set_len(&self, length: usize) {
		self.length.store(length, Ordering::Release);
	}
	/// Publish a worker failure; never called on the audio callback.
	pub fn fail(&self, error: String) {
		*self.error.lock().unwrap() = Some(error);
		self.failed.store(true, Ordering::Release);
	}
	/// Lock-free terminal-failure query for the audio callback.
	pub fn failed(&self) -> bool {
		self.failed.load(Ordering::Acquire)
	}
	/// Read detailed failure on the control side only.
	pub fn error(&self) -> Option<String> {
		self.error.lock().unwrap().clone()
	}
}

/// A source-local, nonblocking PCM window. Methods used by the mixer must not
/// allocate, lock, perform IO, or wait. Source ownership retires off the mixer.
pub trait FrameSource: Send + Sync {
	/// Unknown lengths use usize::MAX until EOF publishes the actual length.
	fn len(&self) -> usize;
	/// Optional independent status shared with transport controls.
	fn status(&self) -> Option<Arc<StreamStatus>> {
		None
	}
	/// Whether EOF established an empty source.
	fn is_empty(&self) -> bool {
		self.len() == 0
	}
	/// Return a cached frame, or None while its worker prepares it.
	fn frame(&self, index: usize) -> Option<Frame>;
	/// Request a source-local seek without waiting for its worker.
	fn seek(&self, _index: usize) {}
	/// Whether preparation has terminated with an error.
	fn failed(&self) -> bool {
		false
	}
	/// Called on the control side only.
	fn error(&self) -> Option<String> {
		None
	}
}

/// Immutable PCM or a bounded asynchronous source, using one transport.
#[derive(Clone)]
pub enum FrameStorage {
	/// Fully decoded immutable frames.
	Static(Arc<[Frame]>),
	/// A worker-backed nonblocking frame source.
	Stream(Arc<dyn FrameSource>),
}
impl FrameStorage {
	/// Current frame count, or usize::MAX until an unknown stream reaches EOF.
	pub fn len(&self) -> usize {
		match self {
			Self::Static(frames) => frames.len(),
			Self::Stream(source) => source.len(),
		}
	}
	/// Whether the source has no frames.
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}
	/// Read one frame without waiting for stream preparation.
	pub fn frame(&self, index: usize) -> Option<Frame> {
		match self {
			Self::Static(frames) => frames.get(index).copied(),
			Self::Stream(source) => source.frame(index),
		}
	}
	/// Request a stream seek; static storage needs no preparation.
	pub fn seek(&self, index: usize) {
		if let Self::Stream(source) = self {
			source.seek(index);
		}
	}
	/// Whether the backing source failed.
	pub fn failed(&self) -> bool {
		matches!(self, Self::Stream(source) if source.failed())
	}
	/// Read the backing error on the control side only.
	pub fn error(&self) -> Option<String> {
		match self {
			Self::Stream(source) => source.error(),
			_ => None,
		}
	}
	/// Access the immutable allocation for static storage.
	pub fn static_frames(&self) -> Option<&Arc<[Frame]>> {
		match self {
			Self::Static(frames) => Some(frames),
			_ => None,
		}
	}
}
impl PartialEq for FrameStorage {
	fn eq(&self, other: &Self) -> bool {
		match (self, other) {
			(Self::Static(a), Self::Static(b)) => a == b,
			(Self::Stream(a), Self::Stream(b)) => Arc::ptr_eq(a, b),
			_ => false,
		}
	}
}
impl From<Vec<Frame>> for FrameStorage {
	fn from(frames: Vec<Frame>) -> Self {
		Self::Static(frames.into())
	}
}
impl From<Arc<[Frame]>> for FrameStorage {
	fn from(frames: Arc<[Frame]>) -> Self {
		Self::Static(frames)
	}
}
impl<const N: usize> From<Arc<[Frame; N]>> for FrameStorage {
	fn from(frames: Arc<[Frame; N]>) -> Self {
		Self::Static(frames)
	}
}
impl FromIterator<Frame> for FrameStorage {
	fn from_iter<T: IntoIterator<Item = Frame>>(iter: T) -> Self {
		Self::Static(iter.into_iter().collect())
	}
}
