//! Standalone allocator observer: unsafe is confined to this review executable.
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::{AssetKey, ContentVersion, ResourceVersion, Scope},
    wake::WakeSignal,
};
use reel_media::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct Observer;
#[global_allocator]
static ALLOCATOR: Observer = Observer;
static ENABLED: AtomicBool = AtomicBool::new(false);
static OBSERVED: Mutex<Option<(BudgetPool, AudioActivityRetirement)>> = Mutex::new(None);
static CHECKS: AtomicUsize = AtomicUsize::new(0);
static BAD_ORDER: AtomicUsize = AtomicUsize::new(0);
static PCM_FREES: AtomicUsize = AtomicUsize::new(0);
static BAD_THREAD: AtomicUsize = AtomicUsize::new(0);
const PCM_BYTES: usize = MAX_DECODED_FRAMES * 8;
const CHARGE: u64 = (MAX_DECODED_FRAMES as u64) * 16;
// StaticSoundData uses Arc<[Frame]>: its exact allocation includes two counters.
const PCM_LAYOUT: usize = PCM_BYTES + 2 * size_of::<usize>();
thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static FREES: Cell<usize> = const { Cell::new(0) };
    static LARGEST_FREE: Cell<usize> = const { Cell::new(0) };
    static REENTRY: Cell<bool> = const { Cell::new(false) };
}
fn count_alloc() {
    if MEASURING.try_with(Cell::get).unwrap_or(false) {
        ALLOCS.set(ALLOCS.get() + 1);
    }
}
fn check_order() {
    // Never assert/format/panic inside GlobalAlloc. Record failures for main.
    let correct = OBSERVED.lock().is_ok_and(|observed| {
        observed.as_ref().is_some_and(|(pool, receipt)| {
            let usage = pool.usage();
            usage.tickets == 1 && usage.bytes == CHARGE && !receipt.resources_released()
        })
    });
    CHECKS.fetch_add(1, Ordering::AcqRel);
    if !correct {
        BAD_ORDER.fetch_add(1, Ordering::Release);
    }
}
// SAFETY: all original allocator pointers and layouts are forwarded exactly once.
unsafe impl GlobalAlloc for Observer {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_alloc();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_alloc();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_alloc();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            FREES.set(FREES.get() + 1);
            LARGEST_FREE.set(LARGEST_FREE.get().max(layout.size()));
        }
        let observe = ENABLED.load(Ordering::Acquire)
            && layout.size() == PCM_LAYOUT
            && !REENTRY.replace(true);
        if observe {
            check_order();
        }
        unsafe { System.dealloc(ptr, layout) };
        if observe {
            check_order();
            if std::thread::current().name() != Some("reel-audio-reclaim") {
                BAD_THREAD.fetch_add(1, Ordering::Release);
            }
            PCM_FREES.fetch_add(1, Ordering::Release);
            REENTRY.set(false);
        }
    }
}
fn measured(action: impl FnOnce()) -> (usize, usize, usize) {
    ALLOCS.set(0);
    FREES.set(0);
    LARGEST_FREE.set(0);
    MEASURING.set(true);
    action();
    MEASURING.set(false);
    (ALLOCS.get(), FREES.get(), LARGEST_FREE.get())
}
fn wait(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "owner probe completion timeout");
        std::thread::park_timeout(Duration::from_millis(1));
    }
}
fn main() {
    let wake = WakeSignal::new(|| {});
    let mut reclaim = AudioReclaimService::new(ReclaimLimits::default(), wake.clone()).unwrap();
    let format = AudioFormat::new(48_000, SampleFormat::F32, ChannelLayout::Stereo, 512).unwrap();
    let mixer = KiraMixer::new(
        &format,
        MixerLimits {
            max_sounds: 1,
            internal_buffer_frames: 128,
        },
        &reclaim,
    )
    .unwrap();
    let mut service = AudioActivityService::new(
        mixer,
        QueueLimits {
            max_requests: 1,
            max_bytes: AudioActivityService::slot_bytes(1) + 64,
        },
        MediaEpoch(7),
        wake,
    )
    .ok()
    .unwrap();
    let owner = service.register_owner(Scope::Process).unwrap();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 1,
            max_bytes: CHARGE,
        },
    );
    let sound = DecodedSound::new(
        48_000,
        vec![[0.25, -0.25]; MAX_DECODED_FRAMES],
        pool.reserve(CHARGE).unwrap(),
    )
    .unwrap()
    .looping();
    let id = service
        .play(AudioActivityRequest {
            epoch: MediaEpoch(7),
            owner: owner.clone(),
            group: AudioGroup::Voice,
            resource: ResourceVersion {
                key: AssetKey("allocator-probe".into()),
                version: ContentVersion([1; 32]),
            },
            sound,
        })
        .unwrap();
    let receipt = service.retirement(id).unwrap();
    *OBSERVED.lock().unwrap() = Some((pool.clone(), receipt.clone()));
    ENABLED.store(true, Ordering::Release);
    wait(|| reclaim.status().completed_accepted_data == 1);
    let mut fill = service.take_fill().unwrap();
    let mut block = [0.0; 1024];
    let first_fill = measured(|| fill(&mut block));
    assert_eq!(first_fill, (0, 0, 0));
    assert!(block.iter().any(|sample| (*sample - 0.25f32).abs() < 1e-6));
    let mut events = Vec::new();
    let cancellation = measured(|| {
        service.retire_owner(&owner, 1).unwrap();
        service.drain(&mut events, 1, |_, _, _| false);
    });
    assert_eq!(cancellation.0, 0);
    assert!(cancellation.2 < PCM_BYTES);
    assert!(receipt.descriptor_released());
    assert!(!receipt.resources_released());
    assert!(!owner.is_retired());
    let stop_fill = measured(|| {
        fill(&mut block);
        fill(&mut block);
    });
    assert_eq!(stop_fill, (0, 0, 0));
    let transfer = measured(|| {
        assert_eq!(service.maintain(1, 1).reclaim.transferred_sounds, 1);
    });
    assert_eq!(transfer, (0, 0, 0));
    wait(|| reclaim.status().completed_sounds == 1);
    assert!(receipt.resources_released());
    assert!(receipt.is_retired());
    assert!(owner.is_retired());
    assert_eq!(pool.usage().tickets, 0);
    assert_eq!(pool.usage().bytes, 0);
    assert_eq!(CHECKS.load(Ordering::Acquire), 2);
    assert_eq!(PCM_FREES.load(Ordering::Acquire), 1);
    assert_eq!(BAD_ORDER.load(Ordering::Acquire), 0);
    assert_eq!(BAD_THREAD.load(Ordering::Acquire), 0);
    ENABLED.store(false, Ordering::Release);
    *OBSERVED.lock().unwrap() = None;
    drop(fill);
    drop(service);
    let status = reclaim.drain_and_join().unwrap();
    assert_eq!(status.worker_errors, 0);
    println!(
        "owner_retirement_allocator actual_pcm_bytes={PCM_BYTES} physical_frees=1 System_dealloc_before_after_checks=2 receipt_pending_and_budget_charged=true wrong_thread_frees=0 final_receipt_retired=true final_budget_bytes=0 PCM_fill_alloc_realloc_dealloc=0 control_transfer_alloc_realloc_dealloc=0 control_cancel_alloc_realloc=0 largest_control_cancel_free={}",
        cancellation.2
    );
}
