//! Bounded replay of one accepted frame through the ordinary GXM backend.
use super::Error;
use crate::performance::capture::{CachePolicy, FrameCaptureOptions, Selection};
use reel_render::{PresentOutcome, RenderBackend, gxm::GxmBackend};
use reel_ui::frame::RenderFrame;
use std::{
    io::{BufWriter, Write},
    time::{Duration, Instant},
};
use vita_native::graphics::{MemoryUsage, SubmissionTiming};

macro_rules! report { ($out:expr, $($args:tt)*) => {{ writeln!($out, $($args)*)?; }} }

const DEADLINE: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_micros(100);

pub(super) struct FrameProbe {
    selection: Selection,
    options: FrameCaptureOptions,
    report_path: std::path::PathBuf,
    completed: bool,
}

impl FrameProbe {
    pub(super) fn new(options: FrameCaptureOptions, report_path: std::path::PathBuf) -> Self {
        Self {
            selection: Selection::new(options.clone()),
            options,
            report_path,
            completed: false,
        }
    }

    pub(super) fn should_capture(&mut self, title: &str, frame: u64) -> bool {
        self.selection.observe(title, frame)
    }

    pub(super) fn finish(&self) -> Result<(), Error> {
        if self.completed {
            Ok(())
        } else {
            Err(format!(
                "fixed-frame probe did not complete: title={:?} accepted_frame={}",
                self.options.label, self.options.accepted_frame
            )
            .into())
        }
    }

    pub(super) fn run(
        &mut self,
        backend: &mut GxmBackend,
        frame: &RenderFrame,
        assets: &crate::assets::Assets,
    ) -> Result<(), Error> {
        let mut events = Vec::with_capacity(32);
        drain(backend, Instant::now(), &mut events)?;
        if let Some(parent) = self
            .report_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut output =
            BufWriter::with_capacity(64 * 1024, std::fs::File::create(&self.report_path)?);
        // The wire representation omits live recording pointers. This identity
        // covers commands and masks, not resource pixel contents or GPU results.
        let masks: Vec<_> = frame
            .masks()
            .iter()
            .map(|(id, mask)| (*id, mask.as_ref()))
            .collect();
        let command_bytes =
            postcard::to_allocvec(&(frame.info().physical_size, frame.commands(), masks))?;
        let command_id = blake3::hash(&command_bytes);
        drop(command_bytes);
        report!(&mut output, "[reel-frozen-report] begin");
        let mode_count = if self.options.measure_passes { 2 } else { 1 };
        report!(
            &mut output,
            "frozen_metadata\tversion=1\tmode=fixed_frame\tcache={}\tframe_id={}\taccepted_index={}\trepetitions={}\tcommand_id={}\tcommand_id_scope=commands_masks_surface\tresource_pixels_identity=unavailable\tgpu_timestamp=false\tcompletion=all_submitted_fragment_fences_observed\tpoll_us=100\tdeadline_ms=10000\tserial_comparison={}",
            self.options.cache.label(),
            frame.frame_id().0,
            self.options.accepted_frame,
            self.options.repetitions * mode_count,
            command_id,
            self.options.measure_passes
        );
        report!(&mut output, "frozen_title\t{}", self.options.label);
        report!(
            &mut output,
            "frozen_workload\tcommands={}\tresources={}\twidth={}\theight={}\tframe_bytes={}",
            frame.commands().len(),
            frame.resources().leases().len(),
            frame.info().physical_size[0],
            frame.info().physical_size[1],
            frame.accounted_bytes()
        );
        for lease in frame.resources().leases() {
            report!(
                &mut output,
                "frozen_resource\thandle={:?}\tversion={:?}",
                lease.handle(),
                lease.version()
            );
        }
        report!(
            &mut output,
            "frozen_columns\trepetition\tmode\tattempts\tsubmit_wall_us\tcompletion_tail_us\ttotal_wall_us\tpolls"
        );
        let previous_serial = reel_render::profiling::serial_effect_timing();
        let result = (|| -> Result<(), Error> {
            for repetition in 0..self.options.repetitions * mode_count {
                let serial = repetition >= self.options.repetitions;
                let mode = if serial { "serial" } else { "batched" };
                reel_render::profiling::set_serial_effect_timing(serial);
                if self.options.cache == CachePolicy::Replay {
                    backend.clear_effect_results_for_probe()?;
                }
                let before = backend.native_memory_usage()?;
                let phases = reel_core::profile::Sample::snapshot();
                memory(&mut output, "before", repetition, backend, &before)?;
                reel_render::profiling::set_trace_enabled(true);
                let thread_before = reel_core::profile::ThreadSnapshot::capture();
                let measured = replay(backend, frame, &mut events);
                let thread_after = reel_core::profile::ThreadSnapshot::capture();
                let after_phases = reel_core::profile::Sample::snapshot();
                reel_render::profiling::set_trace_enabled(false);
                let traced = reel_render::profiling::write_trace(&mut output);
                let (attempts, submit, tail, total, polls) = measured?;
                traced?;
                report!(
                    &mut output,
                    "frozen_sample\t{repetition}\t{mode}\t{attempts}\t{submit}\t{tail}\t{total}\t{polls}"
                );
                if let Some(delta) = thread_after.checked_delta(&thread_before) {
                    for (phase, cost) in reel_core::profile::THREAD_PHASES.iter().zip(delta.costs) {
                        for (metric, value) in
                            reel_core::profile::THREAD_METRICS.iter().zip(cost.values)
                        {
                            report!(
                                &mut output,
                                "frozen_thread\t{repetition}\t{}\t{metric}\t{value}",
                                reel_core::profile::NAMES[*phase as usize]
                            );
                        }
                    }
                } else {
                    report!(&mut output, "frozen_thread_unavailable\t{repetition}");
                }
                for (index, name) in reel_core::profile::NAMES.iter().enumerate() {
                    let calls = after_phases.calls[index].checked_sub(phases.calls[index]);
                    let micros = after_phases.micros[index].checked_sub(phases.micros[index]);
                    if let (Some(calls), Some(micros)) = (calls, micros)
                        && (calls != 0 || micros != 0)
                    {
                        report!(
                            &mut output,
                            "frozen_phase\t{repetition}\t{name}\t{calls}\t{micros}"
                        );
                    }
                }
                let after = backend.native_memory_usage()?;
                native_delta(
                    &mut output,
                    repetition,
                    before.submission_timing,
                    after.submission_timing,
                )?;
                memory(&mut output, "after", repetition, backend, &after)?;
            }
            Ok(())
        })();
        reel_render::profiling::set_serial_effect_timing(previous_serial);
        report!(
            &mut output,
            "frozen_result\t{}",
            if result.is_ok() { "complete" } else { "failed" }
        );
        report!(&mut output, "[reel-frozen-report] end");
        let result = result
            .and(output.flush().map_err(Into::into))
            .and_then(|()| {
                if self.options.save_frame {
                    crate::performance::frame_archive::save(
                        frame,
                        assets,
                        &self.report_path.with_file_name("frame.postcard"),
                    )?;
                }
                Ok(())
            });
        self.completed = result.is_ok();
        eprintln!(
            "[reel-frozen] report={} complete={}",
            self.report_path.display(),
            self.completed
        );
        result
    }
}

fn drain(
    backend: &mut GxmBackend,
    started: Instant,
    events: &mut Vec<reel_render::RenderEvent<reel_render::gxm::GxmError>>,
) -> Result<u64, Error> {
    let mut polls = 0;
    while backend.has_pending() {
        events.clear();
        backend.poll(events, 32)?;
        for event in events.iter() {
            if let reel_render::RenderEvent::DeviceLost(error) = event {
                return Err(format!("fixed-frame device lost: {error}").into());
            }
        }
        polls += 1;
        if !backend.has_pending() {
            break;
        }
        if started.elapsed() >= DEADLINE {
            return Err("fixed-frame completion timed out".into());
        }
        std::thread::sleep(POLL);
    }
    Ok(polls)
}

fn replay(
    backend: &mut GxmBackend,
    frame: &RenderFrame,
    events: &mut Vec<reel_render::RenderEvent<reel_render::gxm::GxmError>>,
) -> Result<(u64, u128, u128, u128, u64), Error> {
    let started = Instant::now();
    let mut attempts = 0;
    let mut polls = 0;
    loop {
        attempts += 1;
        match backend.render_present(frame, || {})? {
            PresentOutcome::PresentAccepted(_) => break,
            PresentOutcome::Preparing => {
                polls += drain(backend, started, events)?;
                if started.elapsed() >= DEADLINE {
                    return Err("fixed-frame preparation timed out".into());
                }
            }
            outcome => return Err(format!("fixed-frame submission ended with {outcome:?}").into()),
        }
    }
    let submit = started.elapsed();
    let waiting = Instant::now();
    polls += drain(backend, started, events)?;
    let tail = waiting.elapsed();
    Ok((
        attempts,
        submit.as_micros(),
        tail.as_micros(),
        started.elapsed().as_micros(),
        polls,
    ))
}

fn memory(
    mut output: &mut impl Write,
    phase: &str,
    repetition: u32,
    backend: &GxmBackend,
    usage: &MemoryUsage,
) -> Result<(), Error> {
    let state = backend.native_memory_state();
    let logical = backend.usage();
    report!(
        &mut output,
        "frozen_memory\t{repetition}\t{phase}\tmain_committed={}\tcdram_committed={}\tpixels_reserved={}\tpixels_used={}\tpixels_idle={}\tpixel_blocks={}\tpixel_block_allocations={}\tdriver_bytes={}\tdriver_targets={}\trenderer_targets={}\trenderer_resources={}\trenderer_transient={}",
        state.main.committed_bytes,
        state.cdram.committed_bytes,
        usage.pixels.reserved_bytes,
        usage.pixels.used_bytes,
        usage.pixels.idle_bytes,
        usage.pixels.blocks,
        usage.pixels.block_allocations,
        usage.render_target_driver_bytes,
        usage.render_targets,
        logical.targets,
        logical.resources,
        logical.transient
    );
    Ok(())
}

fn native_delta(
    mut output: &mut impl Write,
    repetition: u32,
    before: SubmissionTiming,
    after: SubmissionTiming,
) -> Result<(), Error> {
    for (name, a, b) in [
        ("scene_begin", before.scene_begin, after.scene_begin),
        ("scene_end", before.scene_end, after.scene_end),
        (
            "uniform_reserve",
            before.uniform_reserve,
            after.uniform_reserve,
        ),
        ("draw", before.draw, after.draw),
        (
            "notification_capacity_wait",
            before.notification_capacity_wait,
            after.notification_capacity_wait,
        ),
        (
            "upload_capacity_wait",
            before.upload_capacity_wait,
            after.upload_capacity_wait,
        ),
        (
            "resource_replacement_wait",
            before.resource_replacement_wait,
            after.resource_replacement_wait,
        ),
        (
            "display_callback",
            before.display.callback,
            after.display.callback,
        ),
        (
            "display_flip",
            before.display.set_framebuffer,
            after.display.set_framebuffer,
        ),
        (
            "display_wait",
            before.display.wait_framebuffer,
            after.display.wait_framebuffer,
        ),
    ] {
        match (b.calls.checked_sub(a.calls), b.cpu_us.checked_sub(a.cpu_us)) {
            (Some(calls), Some(wall_us)) => {
                report!(
                    &mut output,
                    "frozen_native\t{repetition}\t{name}\t{calls}\t{wall_us}"
                )
            }
            _ => report!(&mut output, "frozen_native_reset\t{repetition}\t{name}"),
        }
    }
    let a = before.notifications;
    let b = after.notifications;
    report!(
        &mut output,
        "frozen_notifications\t{repetition}\tsubmitted={}\tobserved={}\tobservation_age_us={}\tpending={}\tservice_time=unavailable",
        b.submissions.saturating_sub(a.submissions),
        b.observations.saturating_sub(a.observations),
        b.observed_age_us.saturating_sub(a.observed_age_us),
        b.pending.count
    );
    Ok(())
}
