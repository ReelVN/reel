//! Bounded, preallocated ownership transfers for queued static PCM.
use super::StaticSoundData;
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::{
	Arc,
	atomic::{AtomicBool, AtomicU64, Ordering},
};

/// An already prepared segment and the reservation that owns its PCM.
pub struct QueuedStaticSound {
	/// Monotonically increasing segment identity, starting at one.
	pub index: u64,
	/// PCM and source-local transport parameters.
	pub data: StaticSoundData,
	/// Host-owned residency, released after the PCM on the reclaim worker.
	pub retention: Option<Arc<dyn Send + Sync>>,
}

/// The renderer has closed the queue's tail to further announcements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueClosed;

impl std::fmt::Display for QueueClosed {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str("the audio queue has ended")
	}
}

impl std::error::Error for QueueClosed {}

/// Control-side endpoints. Drop this on the host's reclaim worker, because an
/// output failure can leave accepted PCM in either bounded ring.
pub struct StaticSoundQueue {
	incoming: Producer<QueuedStaticSound>,
	retired: Consumer<QueuedStaticSound>,
	tail: Arc<AtomicU64>,
	finished: Arc<AtomicBool>,
	started: Arc<AtomicBool>,
}

pub(super) struct QueueRenderer {
	pub incoming: Consumer<QueuedStaticSound>,
	pub retired: Producer<QueuedStaticSound>,
	pub tail: Arc<AtomicU64>,
	pub finished: Arc<AtomicBool>,
	pub started: Arc<AtomicBool>,
}

pub(super) fn channel(capacity: usize) -> (StaticSoundQueue, QueueRenderer) {
	let (incoming, consume) = RingBuffer::new(capacity);
	let (retire, retired) = RingBuffer::new(capacity + 1);
	let tail = Arc::new(AtomicU64::new(0));
	let finished = Arc::new(AtomicBool::new(false));
	let started = Arc::new(AtomicBool::new(false));
	(
		StaticSoundQueue {
			incoming,
			retired,
			tail: tail.clone(),
			finished: finished.clone(),
			started: started.clone(),
		},
		QueueRenderer {
			incoming: consume,
			retired: retire,
			tail,
			finished,
			started,
		},
	)
}

impl StaticSoundQueue {
	/// Release the finite initial preparation batch before the first PCM frame.
	pub fn start(&self) {
		self.started.store(true, Ordering::Release);
	}
	/// The renderer has reached its unique terminal state.
	pub fn is_finished(&self) -> bool {
		self.finished.load(Ordering::Acquire)
	}
	/// Announce a submitted segment before its asynchronous decode finishes.
	/// The caller reserves capacity before announcing; indices are consecutive.
	pub fn announce(&self, index: u64) -> Result<(), QueueClosed> {
		self.tail
			.try_update(Ordering::AcqRel, Ordering::Acquire, |tail| {
				(tail != u64::MAX).then_some(index)
			})
			.map(|_| ())
			.map_err(|_| QueueClosed)
	}

	/// Transfer prepared PCM in announcement order, returning it on backpressure.
	#[expect(
		clippy::result_large_err,
		reason = "backpressure returns the original PCM and residency owner without allocating"
	)]
	pub fn push(&mut self, item: QueuedStaticSound) -> Result<(), QueuedStaticSound> {
		self.incoming
			.push(item)
			.map_err(|rtrb::PushError::Full(item)| item)
	}

	/// Transfer a consumed segment to the existing background reclaimer.
	pub fn take_retired(&mut self) -> Option<QueuedStaticSound> {
		self.retired.pop().ok()
	}
}
