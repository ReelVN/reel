//! Software MP4 H.264 Baseline and AAC-LC decoder, owned by one worker.
use super::{
    AudioFrames, DecodedPacket, MediaRead, VideoDecoder, VideoDecoderFactory, VideoFrame,
    VideoInfo, Yuv420Layout, YuvColor, YuvMatrix, YuvTransfer,
};
use h264_reader::nal::{
    Nal, RefNal,
    sps::{ChromaFormat, FrameMbsFlags, SeqParameterSet},
};
use mp4::{Mp4Reader, Mp4Track, TrackType};
use openh264::{decoder::Decoder, formats::YUVSource};
use reel_core::budget::{BudgetPool, BudgetTicket};
use std::{
    io::{BufReader, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use symphonia::core::{
    audio::{Audio, AudioBuffer, GenericAudioBufferRef, conv::FromSample, sample::Sample},
    codecs::audio::{AudioCodecParameters, AudioDecoder, well_known::CODEC_ID_AAC},
    packet::Packet,
    units::{Duration, Timestamp},
};

/// Opens the existing MP4 software decoder on a preparation worker.
#[derive(Debug, Default)]
pub struct SoftwareVideoDecoderFactory;
impl VideoDecoderFactory for SoftwareVideoDecoderFactory {
    fn open(
        &self,
        source: Box<dyn MediaRead>,
        budget: BudgetPool,
        stopped: Arc<AtomicBool>,
    ) -> Result<Box<dyn VideoDecoder>, String> {
        Ok(Box::new(MovieDecoder::with_stop(source, budget, stopped)?))
    }
}

const READER_BYTES: usize = 8 * 1024;

/// Reusable adapter storage; growth reserves both allocations before copying.
struct WorkspaceBytes {
    data: Vec<u8>,
    storage: BudgetTicket,
}
impl WorkspaceBytes {
    fn new(budget: &BudgetPool) -> Result<Self, String> {
        Ok(Self {
            data: Vec::new(),
            storage: budget.reserve(0).map_err(|e| e.message)?,
        })
    }
    fn extend_from_slice(&mut self, bytes: &[u8]) -> Result<(), String> {
        let needed = self
            .data
            .len()
            .checked_add(bytes.len())
            .ok_or("video workspace overflow")?;
        if needed > self.data.capacity() {
            let old = self.data.capacity();
            let capacity = needed.max(old.saturating_mul(2));
            let overlap = old
                .checked_add(capacity)
                .ok_or("video workspace overflow")?;
            self.storage.resize(overlap as u64).map_err(|e| e.message)?;
            let mut replacement = Vec::new();
            if let Err(error) = replacement.try_reserve_exact(capacity) {
                self.storage
                    .resize(old as u64)
                    .expect("release failed allocation reservation");
                return Err(error.to_string());
            }
            let actual = old
                .checked_add(replacement.capacity())
                .ok_or("video workspace overflow")?;
            if let Err(error) = self.storage.resize(actual as u64) {
                drop(replacement);
                self.storage
                    .resize(old as u64)
                    .expect("release failed allocation reservation");
                return Err(error.message);
            }
            replacement.extend_from_slice(&self.data);
            drop(std::mem::replace(&mut self.data, replacement));
            self.storage
                .resize(self.data.capacity() as u64)
                .expect("release replaced video buffer");
        }
        self.data.extend_from_slice(bytes);
        Ok(())
    }
    fn as_slice(&self) -> &[u8] {
        &self.data
    }
    fn clear(&mut self) {
        self.data.clear();
    }
}

struct AudioTrack {
    cursor: TrackCursor,
    rate: u32,
    decoder: Box<dyn AudioDecoder>,
}
struct TrackCursor {
    id: u32,
    next: u32,
    count: u32,
    scale: f64,
    start: f64,
    offset: f64,
    end: f64,
}
impl TrackCursor {
    fn new(track: &Mp4Track, movie_scale: u32) -> Result<Self, String> {
        let scale = f64::from(track.timescale());
        if scale == 0. || movie_scale == 0 {
            return Err("MP4 timescale is zero".into());
        }
        let mut start = 0.;
        let mut offset = 0.;
        let mut end = track.duration().as_secs_f64();
        if let Some(edits) = track.trak.edts.as_ref().and_then(|e| e.elst.as_ref()) {
            let mut content = false;
            for edit in &edits.entries {
                if edit.media_rate != 1 || edit.media_rate_fraction != 0 {
                    return Err("MP4 playback-rate edits are unsupported".into());
                }
                let empty = edit.media_time
                    == if edits.version == 0 {
                        u32::MAX as u64
                    } else {
                        u64::MAX
                    };
                let duration = edit.segment_duration as f64 / f64::from(movie_scale);
                if empty && !content {
                    start += duration;
                } else if !empty && !content {
                    offset = start - edit.media_time as f64 / scale;
                    end = start + duration;
                    content = true;
                } else {
                    return Err("MP4 requires a continuous track edit".into());
                }
            }
        }
        Ok(Self {
            id: track.track_id(),
            next: 1,
            count: track.sample_count(),
            scale,
            start,
            offset,
            end,
        })
    }
    fn time(&self, track: &Mp4Track) -> f64 {
        if self.next > self.count {
            return f64::INFINITY;
        }
        sample_time(track, self.next) as f64 / self.scale + self.offset
    }
    fn sample_at(&self, track: &Mp4Track, seconds: f64) -> u32 {
        let ticks = ((seconds - self.offset).max(0.) * self.scale) as u64;
        let mut time = 0;
        let mut sample = 1;
        for entry in &track.trak.mdia.minf.stbl.stts.entries {
            let end = time + u64::from(entry.sample_count) * u64::from(entry.sample_delta);
            if ticks < end {
                return sample + ((ticks - time) / u64::from(entry.sample_delta)) as u32;
            }
            time = end;
            sample += entry.sample_count;
        }
        self.count + 1
    }
}
fn sample_time(track: &Mp4Track, index: u32) -> u64 {
    let mut remaining = index - 1;
    let mut time = 0;
    for entry in &track.trak.mdia.minf.stbl.stts.entries {
        let n = remaining.min(entry.sample_count);
        time += u64::from(n) * u64::from(entry.sample_delta);
        remaining -= n;
        if remaining == 0 {
            break;
        }
    }
    time
}

/// Reads one encoded sample at a time. MP4 sample tables remain resident;
/// encoded payload and decoded picture/sound residency do not grow with duration.
/// The caller's pool charges the reader, parameter-set/Annex-B buffers and encoded
/// samples. MP4 tables and third-party codec internals require separate measurement;
/// the playback worker separately accounts returned pictures and PCM.
pub struct MovieDecoder {
    format: Mp4Reader<BufReader<Box<dyn MediaRead>>>,
    cursor: TrackCursor,
    video: Decoder,
    config: AvcConfig,
    annex_b: WorkspaceBytes,
    audio: Option<AudioTrack>,
    info: VideoInfo,
    seek_position: f64,
    stopped: Arc<AtomicBool>,
    budget: BudgetPool,
    _reader_storage: BudgetTicket,
}
impl MovieDecoder {
    /// Open the software decoder using the caller's actual CPU resource pool.
    pub fn new(source: Box<dyn MediaRead>, budget: BudgetPool) -> Result<Self, String> {
        Self::with_stop(source, budget, Arc::new(AtomicBool::new(false)))
    }
    fn with_stop(
        mut source: Box<dyn MediaRead>,
        budget: BudgetPool,
        stopped: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        check_stopped(&stopped)?;
        let size = source.seek(SeekFrom::End(0)).map_err(error)?;
        source.seek(SeekFrom::Start(0)).map_err(error)?;
        let mut reader_storage = budget.reserve(READER_BYTES as u64).map_err(|e| e.message)?;
        let reader = BufReader::with_capacity(READER_BYTES, source);
        reader_storage
            .resize(reader.capacity() as u64)
            .map_err(|e| e.message)?;
        let format = Mp4Reader::read_header(reader, size).map_err(error)?;
        if format.is_fragmented() {
            return Err("this MP4 decoder requires a non-fragmented movie".into());
        }
        let track = format
            .tracks()
            .values()
            .filter(|t| t.track_type().ok() == Some(TrackType::Video))
            .min_by_key(|t| t.track_id())
            .ok_or("media has no video track")?;
        let avc = &track
            .trak
            .mdia
            .minf
            .stbl
            .stsd
            .avc1
            .as_ref()
            .ok_or("this decoder supports MP4 H.264 Baseline video")?
            .avcc;
        if avc.avc_profile_indication != 66 {
            return Err("this decoder requires H.264 Baseline (profile 66)".into());
        }
        let mut headers = WorkspaceBytes::new(&budget)?;
        for nal in avc
            .sequence_parameter_sets
            .iter()
            .chain(&avc.picture_parameter_sets)
        {
            if nal.bytes.first().is_some_and(|b| b & 31 == 7) && nal.bytes.get(1) != Some(&66) {
                return Err("H.264 sequence requires an unsupported profile".into());
            }
            headers.extend_from_slice(&[0, 0, 0, 1])?;
            headers.extend_from_slice(&nal.bytes)?;
        }
        let sps = avc
            .sequence_parameter_sets
            .first()
            .ok_or("AVC sequence parameter set missing")?;
        let color = sequence_color(&sps.bytes)?;
        let config = AvcConfig {
            color,
            nal_length: usize::from(avc.length_size_minus_one & 3) + 1,
            headers,
        };
        let cursor = TrackCursor::new(track, format.timescale())?;
        let (width, height) = (u32::from(track.width()), u32::from(track.height()));
        let audio = format
            .tracks()
            .values()
            .filter(|t| t.track_type().ok() == Some(TrackType::Audio))
            .min_by_key(|t| t.track_id())
            .map(|track| -> Result<_, String> {
                let desc = &track
                    .trak
                    .mdia
                    .minf
                    .stbl
                    .stsd
                    .mp4a
                    .as_ref()
                    .and_then(|a| a.esds.as_ref())
                    .ok_or("this MP4 decoder supports AAC-LC audio")?
                    .es_desc
                    .dec_config
                    .dec_specific;
                if desc.profile != 2 || !(1..=2).contains(&desc.chan_conf) || desc.freq_index >= 13
                {
                    return Err("this MP4 decoder supports mono/stereo AAC-LC audio".into());
                }
                let rate = track.sample_freq_index().map_err(error)?.freq();
                let _parameters_storage = budget.reserve(2).map_err(|e| e.message)?;
                let params = AudioCodecParameters {
                    codec: CODEC_ID_AAC,
                    sample_rate: Some(rate),
                    extra_data: Some(
                        vec![
                            (desc.profile << 3) | (desc.freq_index >> 1),
                            (desc.freq_index << 7) | (desc.chan_conf << 3),
                        ]
                        .into_boxed_slice(),
                    ),
                    ..Default::default()
                };
                Ok(AudioTrack {
                    cursor: TrackCursor::new(track, format.timescale())?,
                    rate,
                    decoder: symphonia::default::get_codecs()
                        .make_audio_decoder(&params, &Default::default())
                        .map_err(error)?,
                })
            })
            .transpose()?;
        let info = VideoInfo {
            width,
            height,
            duration: Some(cursor.end.max(audio.as_ref().map_or(0., |a| a.cursor.end))),
            audio_sample_rate: audio.as_ref().map(|a| a.rate),
        };
        let mut video = Decoder::new().map_err(error)?;
        video.decode(config.headers.as_slice()).map_err(error)?;
        let annex_b = WorkspaceBytes::new(&budget)?;
        Ok(Self {
            format,
            cursor,
            video,
            config,
            annex_b,
            audio,
            info,
            seek_position: 0.,
            stopped,
            budget,
            _reader_storage: reader_storage,
        })
    }
    pub fn info(&self) -> &VideoInfo {
        &self.info
    }
    pub fn audio_finished(&self) -> bool {
        self.audio
            .as_ref()
            .is_none_or(|a| a.cursor.next > a.cursor.count)
    }
    pub fn set_end(&mut self, seconds: f64) {
        self.cursor.end = self.cursor.end.min(seconds);
        if let Some(audio) = &mut self.audio {
            audio.cursor.end = audio.cursor.end.min(seconds);
        }
        self.info.duration = Some(self.info.duration.unwrap_or(seconds).min(seconds));
    }
    /// Decode only the picture covering a saved transport position. No AAC
    /// packets or output stream are needed for a stopped/completed poster.
    pub fn prepare_poster(&mut self, seconds: f64) -> Result<(), String> {
        self.audio = None;
        self.info.audio_sample_rate = None;
        let position = seconds.min((self.cursor.end - 1. / self.cursor.scale).max(0.));
        self.seek(position)
    }
    pub fn next_packet(&mut self) -> Result<Option<DecodedPacket>, String> {
        loop {
            check_stopped(&self.stopped)?;
            if self.cursor.time(&self.format.tracks()[&self.cursor.id]) >= self.cursor.end {
                self.cursor.next = self.cursor.count + 1;
            }
            if let Some(audio) = &mut self.audio
                && audio.cursor.time(&self.format.tracks()[&audio.cursor.id]) >= audio.cursor.end
            {
                audio.cursor.next = audio.cursor.count + 1;
            }
            let vt = self.cursor.time(&self.format.tracks()[&self.cursor.id]);
            let at = self.audio.as_ref().map_or(f64::INFINITY, |a| {
                a.cursor.time(&self.format.tracks()[&a.cursor.id])
            });
            if vt.is_infinite() && at.is_infinite() {
                return Ok(None);
            }
            if vt <= at {
                let _sample_storage = reserve_sample(
                    &self.budget,
                    &self.format.tracks()[&self.cursor.id],
                    self.cursor.next,
                )?;
                let sample = self
                    .format
                    .read_sample(self.cursor.id, self.cursor.next)
                    .map_err(error)?
                    .ok_or("missing video sample")?;
                self.cursor.next += 1;
                if sample.rendering_offset != 0 {
                    return Err("H.264 frame reordering requires another video decoder".into());
                }
                self.config
                    .convert_packet(&sample.bytes, &mut self.annex_b)?;
                let frame = self
                    .video
                    .decode(self.annex_b.as_slice())
                    .map_err(error)?
                    .ok_or("H.264 Baseline sample did not produce a picture")?;
                let duration = f64::from(sample.duration) / self.cursor.scale;
                if vt + duration <= self.seek_position
                    || vt + duration <= self.cursor.start
                    || vt >= self.cursor.end
                {
                    continue;
                }
                let (width, height) = frame.dimensions();
                let (ys, us, vs) = frame.strides();
                let copy = |source: &[u8], stride: usize, w: usize, h: usize| {
                    let mut data = Vec::with_capacity(w * h);
                    for row in source.chunks(stride).take(h) {
                        data.extend_from_slice(&row[..w]);
                    }
                    data
                };
                return Ok(Some(DecodedPacket::Video(VideoFrame {
                    pts: vt.max(self.cursor.start),
                    duration,
                    width: width as u32,
                    height: height as u32,
                    layout: Yuv420Layout::Planar,
                    row_strides: [
                        width as u32,
                        width.div_ceil(2) as u32,
                        width.div_ceil(2) as u32,
                    ],
                    color: self.config.color,
                    planes: [
                        copy(frame.y(), ys, width, height),
                        copy(frame.u(), us, width.div_ceil(2), height.div_ceil(2)),
                        copy(frame.v(), vs, width.div_ceil(2), height.div_ceil(2)),
                    ],
                })));
            }
            let audio = self.audio.as_mut().unwrap();
            let _sample_storage = reserve_sample(
                &self.budget,
                &self.format.tracks()[&audio.cursor.id],
                audio.cursor.next,
            )?;
            let sample = self
                .format
                .read_sample(audio.cursor.id, audio.cursor.next)
                .map_err(error)?
                .ok_or("missing audio sample")?;
            audio.cursor.next += 1;
            let mut packet_storage = self
                .budget
                .reserve(sample.bytes.len() as u64)
                .map_err(|e| e.message)?;
            let encoded = sample.bytes.to_vec();
            packet_storage
                .resize(encoded.capacity() as u64)
                .map_err(|e| e.message)?;
            let packet = Packet::new(
                audio.cursor.id,
                Timestamp::new(sample.start_time as i64),
                Duration::new(u64::from(sample.duration)),
                encoded,
            );
            let decoded = audio.decoder.decode(&packet).map_err(error)?;
            let mut frames = stereo(&decoded)?;
            let rate = f64::from(audio.rate);
            let first = ((audio.cursor.start.max(self.seek_position) - at) * rate)
                .round()
                .max(0.) as usize;
            let last = ((audio.cursor.end - at) * rate).round().max(0.) as usize;
            let end = frames.len().min(last);
            if first >= end {
                continue;
            }
            frames.truncate(end);
            frames.drain(..first);
            return Ok(Some(DecodedPacket::Audio(AudioFrames {
                pts: at + first as f64 / rate,
                sample_rate: audio.rate,
                frames,
            })));
        }
    }
    /// Seek to the preceding IDR, decode reference frames and trim AAC preroll.
    pub fn seek(&mut self, seconds: f64) -> Result<(), String> {
        check_stopped(&self.stopped)?;
        if !seconds.is_finite() || seconds < 0. {
            return Err("seek position must be finite and nonnegative".into());
        }
        let track = &self.format.tracks()[&self.cursor.id];
        let target = self.cursor.sample_at(track, seconds);
        self.cursor.next = track
            .trak
            .mdia
            .minf
            .stbl
            .stss
            .as_ref()
            .map_or(target, |sync| {
                let n = sync.entries.partition_point(|&s| s <= target);
                if n == 0 { 1 } else { sync.entries[n - 1] }
            });
        self.video = Decoder::new().map_err(error)?;
        self.video
            .decode(self.config.headers.as_slice())
            .map_err(error)?;
        if let Some(audio) = &mut self.audio {
            audio.cursor.next = audio
                .cursor
                .sample_at(&self.format.tracks()[&audio.cursor.id], seconds)
                .saturating_sub(1)
                .max(1);
            audio.decoder.reset();
        }
        self.seek_position = seconds;
        Ok(())
    }
}
fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn reserve_sample(
    budget: &BudgetPool,
    track: &Mp4Track,
    sample: u32,
) -> Result<BudgetTicket, String> {
    // Fragmented input was rejected at admission; stsz is the allocation size
    // used by mp4::read_sample for this supported non-fragmented source.
    let sizes = &track.trak.mdia.minf.stbl.stsz;
    let bytes = if sizes.sample_size != 0 {
        sizes.sample_size
    } else {
        *sizes
            .sample_sizes
            .get(sample as usize - 1)
            .ok_or("missing MP4 sample size")?
    };
    budget.reserve(u64::from(bytes)).map_err(|e| e.message)
}
fn sequence_color(nal: &[u8]) -> Result<YuvColor, String> {
    let sps = SeqParameterSet::from_bits(RefNal::new(nal, &[], true).rbsp_bits())
        .map_err(|e| format!("H.264 SPS: {e:?}"))?;
    if sps.frame_mbs_flags != FrameMbsFlags::Frames
        || sps.chroma_info.chroma_format != ChromaFormat::YUV420
        || sps.chroma_info.bit_depth_luma_minus8 != 0
        || sps.chroma_info.bit_depth_chroma_minus8 != 0
    {
        return Err("this video decoder requires progressive 8-bit YUV420".into());
    }
    let (_, height) = sps
        .pixel_dimensions()
        .map_err(|e| format!("H.264 dimensions: {e:?}"))?;
    let signal = sps
        .vui_parameters
        .as_ref()
        .and_then(|v| v.video_signal_type.as_ref());
    let matrix = match signal
        .and_then(|s| s.colour_description.as_ref())
        .map(|c| c.matrix_coefficients)
    {
        Some(1) => YuvMatrix::Bt709,
        Some(5 | 6) => YuvMatrix::Bt601,
        None | Some(2) => {
            if height >= 720 {
                YuvMatrix::Bt709
            } else {
                YuvMatrix::Bt601
            }
        }
        Some(matrix) => return Err(format!("unsupported H.264 colour matrix {matrix}")),
    };
    let transfer = match signal
        .and_then(|s| s.colour_description.as_ref())
        .map(|c| c.transfer_characteristics)
    {
        Some(13) => YuvTransfer::Srgb,
        None | Some(1 | 2 | 6) => YuvTransfer::Bt709,
        Some(transfer) => {
            return Err(format!(
                "unsupported H.264 transfer characteristic {transfer}"
            ));
        }
    };
    Ok(YuvColor {
        matrix,
        transfer,
        full_range: signal.is_some_and(|s| s.video_full_range_flag),
    })
}
struct AvcConfig {
    nal_length: usize,
    headers: WorkspaceBytes,
    color: YuvColor,
}
impl AvcConfig {
    fn convert_packet(
        &mut self,
        mut data: &[u8],
        output: &mut WorkspaceBytes,
    ) -> Result<(), String> {
        output.clear();
        while !data.is_empty() {
            let length = data
                .get(..self.nal_length)
                .ok_or("truncated AVC sample length")?;
            let size = length.iter().fold(0usize, |n, &b| (n << 8) | b as usize);
            data = &data[self.nal_length..];
            let nal = data.get(..size).ok_or("truncated AVC sample")?;
            if nal.first().is_some_and(|b| b & 31 == 7) && nal.get(1) != Some(&66) {
                return Err("H.264 sequence requires an unsupported profile".into());
            }
            if nal.first().is_some_and(|b| b & 31 == 7) {
                self.color = sequence_color(nal)?;
            }
            output.extend_from_slice(&[0, 0, 0, 1])?;
            output.extend_from_slice(nal)?;
            data = &data[size..];
        }
        Ok(())
    }
}

fn stereo(buffer: &GenericAudioBufferRef<'_>) -> Result<Vec<[f32; 2]>, String> {
    fn convert<S: Sample>(buffer: &AudioBuffer<S>) -> Result<Vec<[f32; 2]>, String>
    where
        f32: FromSample<S>,
    {
        match buffer.num_planes() {
            1 => Ok(buffer
                .plane(0)
                .unwrap()
                .iter()
                .map(|&a| [f32::from_sample(a); 2])
                .collect()),
            2 => Ok(buffer
                .plane(0)
                .unwrap()
                .iter()
                .zip(buffer.plane(1).unwrap())
                .map(|(&a, &b)| [f32::from_sample(a), f32::from_sample(b)])
                .collect()),
            _ => Err("video audio must be mono or stereo".into()),
        }
    }
    match buffer {
        GenericAudioBufferRef::U8(b) => convert(b),
        GenericAudioBufferRef::U16(b) => convert(b),
        GenericAudioBufferRef::U24(b) => convert(b),
        GenericAudioBufferRef::U32(b) => convert(b),
        GenericAudioBufferRef::S8(b) => convert(b),
        GenericAudioBufferRef::S16(b) => convert(b),
        GenericAudioBufferRef::S24(b) => convert(b),
        GenericAudioBufferRef::S32(b) => convert(b),
        GenericAudioBufferRef::F32(b) => convert(b),
        GenericAudioBufferRef::F64(b) => convert(b),
    }
}

impl VideoDecoder for MovieDecoder {
    fn info(&self) -> &VideoInfo {
        MovieDecoder::info(self)
    }
    fn audio_finished(&self) -> bool {
        MovieDecoder::audio_finished(self)
    }
    fn set_end(&mut self, seconds: f64) {
        MovieDecoder::set_end(self, seconds);
    }
    fn prepare_poster(&mut self, seconds: f64) -> Result<(), String> {
        MovieDecoder::prepare_poster(self, seconds)
    }
    fn next_packet(&mut self) -> Result<Option<DecodedPacket>, String> {
        MovieDecoder::next_packet(self)
    }
    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        MovieDecoder::seek(self, seconds)
    }
}

fn check_stopped(stopped: &AtomicBool) -> Result<(), String> {
    if stopped.load(Ordering::Acquire) {
        Err("video decoding cancelled".into())
    } else {
        Ok(())
    }
}
