//! Worker-owned SceAudiodec state. Raw pointers never leave the stable SDK box.
#![allow(unsafe_code)]
use super::*;
use reel_core::budget::BudgetTicket;
use std::{
    io::BufReader,
    sync::{Mutex, atomic::Ordering},
};
use vitasdk_sys::*;

const ATRAC9_CODEC: u32 = 0x1003;
const NATIVE_ALLOWANCE: u64 = 512 << 10;
static ACTIVE_DECODERS: Mutex<u32> = Mutex::new(0);
fn sdk_result(code: i32, operation: &str) -> Result<()> {
    if code >= 0 {
        Ok(())
    } else {
        Err(format!("SceAudiodec {operation} failed: {code:#010x}"))
    }
}
struct RuntimeLease;
impl RuntimeLease {
    fn acquire() -> Result<Self> {
        let mut active = ACTIVE_DECODERS.lock().unwrap();
        if *active == 0 {
            let mut parameters = SceAudiodecInitParam {
                at9: SceAudiodecInitChParam {
                    size: size_of::<SceAudiodecInitParam>() as u32,
                    totalCh: 16,
                },
            };
            // SAFETY: ATRAC9_CODEC selects the initialized at9 union member.
            // Its stack storage outlives the synchronous call, and the mutex
            // prevents initialization while another thread terminates the library.
            sdk_result(
                unsafe { sceAudiodecInitLibrary(ATRAC9_CODEC, &mut parameters) },
                "initialize",
            )?;
        }
        *active += 1;
        Ok(Self)
    }
}
impl Drop for RuntimeLease {
    fn drop(&mut self) {
        let mut active = ACTIVE_DECODERS.lock().unwrap();
        *active -= 1;
        if *active == 0 {
            // SAFETY: every decoder deletes its SDK handle before dropping this
            // lease. The last lease and mutex exclude all other SDK users here.
            let code = unsafe { sceAudiodecTermLibrary(ATRAC9_CODEC) };
            if code < 0 {
                eprintln!("SceAudiodec termination failed: {code:#010x}");
            }
        }
    }
}

// Both arrays begin at multiples of 256 bytes. One Box keeps the arrays,
// metadata, and control block stationary from creation through deletion.
#[repr(C, align(256))]
struct NativeState {
    encoded: [u8; 4096],
    decoded: [i16; 2048],
    info: SceAudiodecInfo,
    control: SceAudiodecCtrl,
}
struct HardwareDecoder {
    state: Box<NativeState>,
    alive: bool,
    packet_bytes: u32,
    packet_frames: u32,
    output_bytes: u32,
    _runtime: RuntimeLease,
    _memory: BudgetTicket,
}
// SAFETY: only this owner can access the SDK pointers, all of which point into
// its stable Box. Moving the owner transfers exclusive access after synchronous
// calls have returned; the boxed state itself is never moved or exposed.
unsafe impl Send for HardwareDecoder {}
impl HardwareDecoder {
    fn create(header: Header, budget: BudgetPool) -> Result<Self> {
        // The SDK handle has a conservative allowance. The Rust-side state is
        // charged by its exact allocation extent, not a whole decode-pool limit.
        let memory = budget
            .reserve(NATIVE_ALLOWANCE + size_of::<NativeState>() as u64)
            .map_err(|e| e.message)?;
        let runtime = RuntimeLease::acquire()?;
        let mut state = Box::new(NativeState {
            encoded: [0; 4096],
            decoded: [0; 2048],
            info: SceAudiodecInfo {
                at9: SceAudiodecInfoAt9 {
                    size: size_of::<SceAudiodecInfoAt9>() as u32,
                    configData: header.config,
                    ch: 0,
                    bitRate: 0,
                    samplingRate: 0,
                    superFrameSize: 0,
                    framesInSuperFrame: 0,
                },
            },
            control: SceAudiodecCtrl {
                size: size_of::<SceAudiodecCtrl>() as u32,
                handle: -1,
                pEs: std::ptr::null_mut(),
                inputEsSize: 0,
                maxEsSize: 0,
                pPcm: std::ptr::null_mut(),
                outputPcmSize: 0,
                maxPcmSize: 0,
                wordLength: 16,
                pInfo: std::ptr::null_mut(),
            },
        });
        state.control.pEs = state.encoded.as_mut_ptr();
        state.control.pPcm = state.decoded.as_mut_ptr().cast();
        state.control.pInfo = &mut state.info;
        let mut decoder = Self {
            state,
            alive: false,
            packet_bytes: header.block_bytes,
            packet_frames: header.block_frames,
            output_bytes: header.block_samples() * header.format.channels * 2,
            _runtime: runtime,
            _memory: memory,
        };
        // SAFETY: control points only into the initialized, aligned NativeState
        // Box. Creation is synchronous and its successful handle is retained by
        // decoder until deletion; the Box and runtime lease remain alive.
        let status = unsafe { sceAudiodecCreateDecoder(&mut decoder.state.control, ATRAC9_CODEC) };
        sdk_result(status, "create")?;
        decoder.alive = true;
        // SAFETY: successful ATRAC9 creation populated this union's at9 member.
        let negotiated = unsafe { decoder.state.info.at9 };
        let control = &decoder.state.control;
        if negotiated.ch != header.format.channels
            || negotiated.samplingRate != header.codec_rate
            || negotiated.superFrameSize != header.block_bytes
            || negotiated.framesInSuperFrame != header.block_frames
            || control
                .maxEsSize
                .checked_mul(header.block_frames)
                .is_none_or(|n| n as usize > decoder.state.encoded.len())
            || control
                .maxPcmSize
                .checked_mul(header.block_frames)
                .is_none_or(|n| n as usize > decoder.state.decoded.len() * size_of::<i16>())
        {
            return Err("SceAudiodec negotiated an incompatible ATRAC9 layout".into());
        }
        Ok(decoder)
    }
    fn reset(&mut self) -> Result<()> {
        // SAFETY: alive owns this exclusive handle; all prior operations returned
        // and its complete control/buffer state remains in the stationary Box.
        sdk_result(
            unsafe { sceAudiodecClearContext(&mut self.state.control) },
            "reset",
        )
    }
    fn decode(&mut self, source: &mut dyn Read) -> Result<()> {
        source
            .read_exact(&mut self.state.encoded[..self.packet_bytes as usize])
            .map_err(|e| format!("ATRAC9 packet read failed: {e}"))?;
        self.state.control.inputEsSize = 0;
        self.state.control.outputPcmSize = 0;
        // SAFETY: one complete validated superframe fills encoded. Negotiated
        // maxima fit both aligned arrays; packet_frames is one or four. The call
        // is synchronous and exclusive access prevents concurrent SDK mutation.
        let status =
            unsafe { sceAudiodecDecodeNFrames(&mut self.state.control, self.packet_frames) };
        sdk_result(status, "decode")?;
        if self.state.control.inputEsSize != self.packet_bytes
            || self.state.control.outputPcmSize != self.output_bytes
        {
            return Err("SceAudiodec did not consume and produce the declared superframe".into());
        }
        Ok(())
    }
    fn pcm(&self) -> &[i16] {
        &self.state.decoded[..self.output_bytes as usize / size_of::<i16>()]
    }
}
impl Drop for HardwareDecoder {
    fn drop(&mut self) {
        if self.alive {
            // SAFETY: deletion is the last use of the owned handle. No decode
            // remains in flight; state and runtime are dropped only afterwards.
            let status = unsafe { sceAudiodecDeleteDecoder(&mut self.state.control) };
            if status < 0 {
                eprintln!("SceAudiodec deletion failed: {status:#010x}");
            }
        }
    }
}

struct Native {
    handle: Option<HardwareDecoder>,
    input: BufReader<Box<dyn MediaRead>>,
    _buffer: BudgetTicket,
    header: Header,
    budget: BudgetPool,
    stopped: Arc<AtomicBool>,
    position: u64,
    next_block: u64,
    range: std::ops::Range<usize>,
}
pub(super) fn open(
    source: Box<dyn MediaRead>,
    header: Header,
    budget: BudgetPool,
    stopped: Arc<AtomicBool>,
) -> Result<Box<dyn AudioDecoder>> {
    let capacity = header.data_bytes.min(16 << 10) as usize;
    let buffer = budget.reserve(capacity as u64).map_err(|e| e.message)?;
    let mut decoder = Native {
        handle: None,
        input: BufReader::with_capacity(capacity, source),
        _buffer: buffer,
        header,
        budget,
        stopped,
        position: 0,
        next_block: 0,
        range: 0..0,
    };
    decoder.seek(0)?;
    Ok(Box::new(decoder))
}
impl AudioDecoder for Native {
    fn info(&self) -> AudioInfo {
        AudioInfo {
            sample_rate: self.header.format.rate,
            frames: Some(self.header.format.frames),
        }
    }
    fn seek(&mut self, frame: u64) -> Result<u64> {
        let (offset, _) = self.header.seek(frame)?;
        self.handle = None;
        self.input
            .seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        self.position = frame;
        self.next_block = (offset - self.header.data_offset) / u64::from(self.header.block_bytes);
        self.range = 0..0;
        Ok(frame)
    }
    fn decode(&mut self, cancelled: &AtomicBool) -> Result<bool> {
        while self.position < self.header.format.frames {
            if cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
                return Err("audio preparation cancelled".into());
            }
            if self.next_block >= self.header.data_bytes / u64::from(self.header.block_bytes) {
                return Err("ATRAC9 ended before its audible timeline".into());
            }
            self.range = self.header.pcm_range(self.position, self.next_block)?;
            if self.handle.is_none() {
                let mut handle = HardwareDecoder::create(self.header, self.budget.clone())?;
                handle.reset()?;
                self.handle = Some(handle);
            }
            self.handle.as_mut().unwrap().decode(&mut self.input)?;
            self.next_block += 1;
            let available = self.range.len() as u64;
            if available != 0 {
                self.position += available;
                return Ok(true);
            }
        }
        self.range = 0..0;
        self.handle = None;
        Ok(false)
    }
    fn pcm(&self) -> AudioPcm<'_> {
        let Some(handle) = &self.handle else {
            return AudioPcm::MonoI16(&[]);
        };
        let pcm = handle.pcm();
        if self.header.format.channels == 1 {
            AudioPcm::MonoI16(&pcm[self.range.clone()])
        } else {
            AudioPcm::StereoI16(&pcm.as_chunks::<2>().0[self.range.clone()])
        }
    }
}
