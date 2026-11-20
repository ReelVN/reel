use super::worker::{Worker, check_completion};
use image::DynamicImage;
use reel_core::texture::{TextureCodec, TextureFormat, TextureHeader};
use reel_core::{
    budget::{BudgetCategory, OwnedBytes, QueueLimits},
    identity::WorkId,
    queue::{charge, invalid},
    request_ids::RequestIds,
    result::{RejectCode, RejectReason, Rejected, Terminal},
    wake::WakeSignal,
    work::*,
};
use std::{io::Cursor, sync::atomic::Ordering};
/// Fixed-model decode-work allowance for bc-crunch 0.1.2. range::Model uses
/// fixed CDF/count/lookup arrays; alpha::Models plus a 256-entry u64 dictionary
/// and color::decode's fixed models/256-entry u32 dictionary do not grow with
/// image size. This allowance is independent of the separately owned worker
/// thread stack; it is not a measurement of that stack or the output block buffer.
pub(super) const BC_CRUNCH_WORKSPACE_BYTES: u64 = 64 * 1024;
pub(super) const MAX_IMAGE_WORKERS: usize = if cfg!(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "macos"
)) {
    3
} else {
    1
};

const IMAGE_PROBE_NAME: &str = "image.probe";

pub fn image_probe() -> WorkKind {
    WorkKind {
        name: IMAGE_PROBE_NAME.into(),
        version: 1,
        operation: WorkOperation::Decode,
    }
}
pub fn image_decode() -> WorkKind {
    WorkKind {
        name: "image.rgba8".into(),
        version: 1,
        operation: WorkOperation::Decode,
    }
}
pub struct ImageExecutor {
    worker: Worker<(WorkId, WorkRequest), WorkEvent>,
    ids: RequestIds<WorkId>,
    limits: QueueLimits,
}
impl ImageExecutor {
    pub fn new(limits: QueueLimits, wake: WakeSignal) -> Self {
        let parallelism = if MAX_IMAGE_WORKERS > 1 {
            std::thread::available_parallelism()
                .map_or(1, |threads| threads.get().min(MAX_IMAGE_WORKERS))
        } else {
            1
        };
        Self {
            worker: Worker::new(
                limits,
                wake,
                |(id, request): (WorkId, WorkRequest), cancelled| {
                    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Decode);
                    let WorkRequest {
                        kind,
                        input,
                        scope,
                        reservations,
                        ..
                    } = request;
                    let WorkReservations {
                        mut output,
                        temporary,
                        completion,
                    } = reservations;
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if cancelled.load(Ordering::Acquire) {
                            return Err("decode cancelled".into());
                        }
                        let probe = kind.name == IMAGE_PROBE_NAME;
                        if input.as_slice().starts_with(TextureHeader::MAGIC) {
                            let (header, payload) = TextureHeader::parse(input.as_slice())
                                .map_err(|e| e.to_string())?;
                            let data = if probe {
                                header.encode().to_vec()
                            } else {
                                let descriptor = header.descriptor();
                                let length = descriptor.byte_len().map_err(|e| e.to_string())?;
                                if length > output.bytes() {
                                    return Err("texture decode reservation too small".into());
                                }
                                match header.codec() {
                                    TextureCodec::Raw => payload.to_vec(),
                                    TextureCodec::BcCrunch => {
                                        if temporary.bytes() < BC_CRUNCH_WORKSPACE_BYTES {
                                            return Err(
                                                "BC decoder workspace reservation too small".into(),
                                            );
                                        }
                                        let mut blocks = vec![0; length as usize];
                                        let format = match descriptor.format {
                                            TextureFormat::Bc1Rgb => bc_crunch::Format::Bc1,
                                            TextureFormat::Bc3Rgba => bc_crunch::Format::Bc3,
                                            _ => return Err("bc-crunch requires BC1 or BC3".into()),
                                        };
                                        bc_crunch::decompress_into(
                                            descriptor.layout.storage_size[0],
                                            descriptor.layout.storage_size[1],
                                            format,
                                            payload,
                                            &mut blocks,
                                        )
                                        .map_err(|e| e.to_string())?;
                                        blocks
                                    }
                                }
                            };
                            output
                                .resize(data.capacity() as u64)
                                .map_err(|e| e.message)?;
                            return OwnedBytes::new(data, output).map_err(|e| e.reason.message);
                        }
                        let data = decode_artwork(
                            input.as_slice(),
                            probe,
                            temporary.bytes(),
                            output.bytes(),
                        )?;
                        output
                            .resize(data.capacity() as u64)
                            .map_err(|e| e.message)?;
                        OwnedBytes::new(data, output).map_err(|e| e.reason.message)
                    }))
                    .unwrap_or_else(|_| Err("image decoder panicked".into()));
                    WorkEvent {
                        id,
                        scope,
                        completion,
                        result: match result {
                            Ok(data) => Terminal::Completed(data),
                            Err(message) => Terminal::Failed(WorkError {
                                code: WorkErrorCode::OperationFailed,
                                message,
                            }),
                        },
                    }
                },
                |event| event.result = Terminal::Cancelled,
            )
            .with_parallelism(parallelism),
            ids: RequestIds::default(),
            limits,
        }
    }
}

fn decode_artwork(
    input: &[u8],
    probe: bool,
    temporary: u64,
    output: u64,
) -> Result<Vec<u8>, String> {
    if !probe && let Some(decoded) = crate::artwork::decode_rgb_png_rows(input, temporary, output) {
        return decoded;
    }
    let (decoder, workspace) =
        crate::artwork::decoder(Cursor::new(input), input.len() as u64, temporary)?;
    let (width, height) = decoder.dimensions();
    let rgba = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(4))
        .ok_or("image size overflow")?;
    let decoded = decoder.total_bytes() + workspace;
    if probe {
        let mut data = Vec::with_capacity(24);
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&rgba.to_le_bytes());
        data.extend_from_slice(&decoded.to_le_bytes());
        return Ok(data);
    }
    let full_image_fits = decoded
        .checked_add(rgba)
        .is_some_and(|bytes| bytes <= temporary);
    let direct_fits = !full_image_fits
        && crate::artwork::direct_png_workspace(width) <= temporary
        && crate::artwork::is_bounded_rgba8_png(input);
    if rgba > output || !(full_image_fits || direct_fits) {
        return Err("image decode reservation too small".into());
    }
    Ok(DynamicImage::from_decoder(decoder)
        .map_err(|e| e.to_string())?
        .into_rgba8()
        .into_raw())
}

#[allow(
    clippy::result_large_err,
    reason = "rejection returns the owning request without allocating"
)]
impl WorkExecutor for ImageExecutor {
    fn capabilities(&self) -> WorkCaps {
        WorkCaps {
            mode: WorkMode::Threaded,
            kinds: vec![image_probe(), image_decode()],
            queue: self.limits,
        }
    }
    fn submit(&mut self, request: WorkRequest) -> Result<WorkId, Rejected<WorkRequest>> {
        if request.kind != image_probe() && request.kind != image_decode() {
            return Err(Rejected {
                request,
                reason: RejectReason::new(RejectCode::Unsupported, "unsupported image operation"),
            });
        }
        let checks = check_completion(&request.reservations.completion).and_then(|_| {
            if request.input.ticket().category() != BudgetCategory::CpuInput
                || request.reservations.output.category() != BudgetCategory::CpuOutput
                || request.reservations.temporary.category() != BudgetCategory::Temporary
                || (request.kind == image_probe() && request.reservations.output.bytes() < 24)
            {
                return Err(invalid("image input/output/temporary reservations invalid"));
            }
            charge(&[
                request.input.ticket().bytes(),
                request.reservations.output.bytes(),
                request.reservations.temporary.bytes(),
                request.reservations.completion.bytes(),
                request.kind.name.capacity() as u64,
                size_of::<WorkRequest>() as u64,
            ])
        });
        let bytes = match checks {
            Ok(bytes) => bytes,
            Err(reason) => return Err(Rejected { request, reason }),
        };
        self.ids.accept(request, |id, request| {
            let priority = request.priority;
            self.worker
                .accept(id.0, (id, request), bytes, priority)
                .map_err(|error| Rejected {
                    request: error.request.1,
                    reason: error.reason,
                })
        })
    }
    fn promote_visible(&mut self, id: WorkId) {
        self.worker.promote_visible(id.0);
    }
    fn cancel(&mut self, id: WorkId) {
        self.worker.cancel(id.0);
    }
    fn advance(&mut self, _: WorkBudget) -> WorkProgress {
        WorkProgress::default()
    }
    fn drain(&mut self, out: &mut Vec<WorkEvent>, limit: usize) {
        self.worker.drain(out, limit);
    }
}
