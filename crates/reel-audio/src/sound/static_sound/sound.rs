mod resampler;

#[cfg(test)]
mod test;

use std::collections::VecDeque;
use std::sync::{
	Arc,
	atomic::{AtomicU8, AtomicU64, Ordering},
};

use crate::{
	Decibels, Panning, Parameter, PlaybackRate, StartTime, Tween,
	command::read_commands_into_parameters,
	frame::Frame,
	info::Info,
	playback_state_manager::PlaybackStateManager,
	sound::{PlaybackState, Sound, transport::Transport},
};

use self::resampler::Resampler;

use super::{
	CommandReaders,
	data::StaticSoundData,
	frames::FrameCache,
	num_frames,
	queue::{QueueRenderer, QueuedStaticSound},
};

pub(super) struct StaticSound {
	command_readers: CommandReaders,
	sample_rate: u32,
	frames: super::FrameStorage,
	frame_cache: FrameCache,
	slice: Option<(usize, usize)>,
	reverse: bool,
	playback_state_manager: PlaybackStateManager,
	start_time: StartTime,
	resampler: Resampler,
	transport: Transport,
	fractional_position: f64,
	volume: Parameter<Decibels>,
	ducking: Option<Parameter<Decibels>>,
	playback_rate: Parameter<PlaybackRate>,
	rate_sequence: u64,
	pending_rate: Option<super::RateChangeCommand>,
	panning: Parameter<Panning>,
	shared: Arc<Shared>,
	queue: Option<QueueRenderer>,
	retention: Option<Arc<dyn Send + Sync>>,
	item: u64,
	queue_preroll: bool,
	preroll_remaining: u8,
	observed_frames: usize,
	observed_tail: u64,
	item_settings: super::StaticSoundSettings,
	history: VecDeque<QueuedStaticSound>,
	prefetched: VecDeque<QueuedStaticSound>,
}

impl StaticSound {
	#[must_use]
	pub fn new(data: StaticSoundData, command_readers: CommandReaders) -> Self {
		let settings = data.settings;
		let observed_frames = data.num_frames();
		let transport = Transport::new(
			data.settings.start_position.into_samples(data.sample_rate),
			data.settings.loop_region,
			data.settings.reverse,
			data.sample_rate,
			data.num_frames(),
		);
		let starting_frame_index = transport.position;
		let position = starting_frame_index as f64 / data.sample_rate as f64;
		let playback_rate = Parameter::new(settings.playback_rate, PlaybackRate(1.0));
		let initial_rate = playback_rate.value().0.to_bits();
		let mut sound = Self {
			command_readers,
			sample_rate: data.sample_rate,
			frames: data.frames,
			frame_cache: FrameCache::new(),
			slice: data.slice,
			reverse: data.settings.reverse,
			playback_state_manager: PlaybackStateManager::new(settings.fade_in_tween),
			start_time: settings.start_time,
			resampler: Resampler::for_source(starting_frame_index, 0, data.sample_rate),
			transport,
			fractional_position: 0.0,
			volume: Parameter::new(settings.volume, Decibels::IDENTITY),
			ducking: None,
			playback_rate,
			rate_sequence: 0,
			pending_rate: None,
			panning: Parameter::new(settings.panning, Panning::CENTER),
			shared: Arc::new(Shared {
				state: AtomicU8::new(PlaybackState::Playing as u8),
				position: AtomicU64::new(position.to_bits()),
				applied_seek: AtomicU64::new(0),
				item: AtomicU64::new(0),
				cursor_sequence: AtomicU64::new(0),
				rate: AtomicU64::new(initial_rate),
				rate_target: AtomicU64::new(initial_rate),
				rate_remaining_seconds: AtomicU64::new(0),
				rate_remaining_nanos: AtomicU64::new(0),
				applied_rate: AtomicU64::new(0),
			}),
			queue: None,
			retention: None,
			item: 0,
			queue_preroll: false,
			preroll_remaining: 3,
			observed_frames,
			observed_tail: 0,
			item_settings: settings,
			history: VecDeque::new(),
			prefetched: VecDeque::new(),
		};
		// fill the resample buffer with 3 samples so playback can
		// start immediately
		while sound.preroll_remaining > 0 {
			if !sound.advance_frame() {
				break;
			}
			sound.preroll_remaining -= 1;
		}
		sound
	}

	pub(super) fn shared(&self) -> Arc<Shared> {
		self.shared.clone()
	}

	pub(super) fn install_queue(&mut self, queue: QueueRenderer, retention: Arc<dyn Send + Sync>) {
		let start = (self.shared.position() * self.sample_rate as f64).round() as usize;
		self.transport
			.seek_to(start, num_frames(&self.frames, self.slice));
		self.resampler = Resampler::for_source(start, 0, self.sample_rate);
		self.queue_preroll = true;
		self.history.reserve_exact(3);
		self.prefetched.reserve_exact(3);
		self.queue = Some(queue);
		self.retention = Some(retention);
	}

	fn has_successor(&self) -> bool {
		self.queue.as_ref().is_some_and(|queue| {
			let tail = queue.tail.load(Ordering::Acquire);
			tail != u64::MAX && tail > self.item
		})
	}

	fn swap_item(&mut self, next: QueuedStaticSound) -> QueuedStaticSound {
		self.frame_cache.clear();
		let old = QueuedStaticSound {
			index: self.item,
			data: StaticSoundData {
				sample_rate: self.sample_rate,
				frames: std::mem::replace(&mut self.frames, next.data.frames),
				slice: self.slice,
				settings: self.item_settings,
			},
			retention: std::mem::replace(&mut self.retention, next.retention),
		};
		self.sample_rate = next.data.sample_rate;
		self.slice = next.data.slice;
		self.reverse = next.data.settings.reverse;
		self.item = next.index;
		self.item_settings = next.data.settings;
		self.observed_frames = num_frames(&self.frames, self.slice);
		self.transport = Transport::new(
			next.data
				.settings
				.start_position
				.into_samples(self.sample_rate),
			next.data.settings.loop_region,
			self.reverse,
			self.sample_rate,
			num_frames(&self.frames, self.slice),
		);
		self.transport.seek_to(
			self.transport.position,
			num_frames(&self.frames, self.slice),
		);
		self.frames
			.seek(self.transport.position + self.slice.map_or(0, |s| s.0));
		old
	}
	fn retire_history(&mut self) {
		let Some(queue) = &mut self.queue else {
			return;
		};
		while self
			.history
			.front()
			.is_some_and(|item| item.index < self.resampler.current_source())
			&& !queue.retired.is_full()
		{
			let item = self.history.pop_front().unwrap();
			queue
				.retired
				.push(item)
				.unwrap_or_else(|_| unreachable!("retirement slot reserved"));
		}
	}
	fn advance_queue(&mut self) -> bool {
		self.retire_history();
		let Some(queue) = &mut self.queue else {
			return false;
		};
		let empty = !self.reverse
			&& self.item_settings.loop_region.is_none()
			&& self
				.item_settings
				.start_position
				.into_samples(self.sample_rate)
				>= num_frames(&self.frames, self.slice);
		if (!empty && self.history.len() == 3) || (empty && queue.retired.is_full()) {
			return false;
		}
		let Some(next) = self
			.prefetched
			.pop_front()
			.or_else(|| queue.incoming.pop().ok())
		else {
			return false;
		};
		let old = self.swap_item(next);
		if empty {
			self.queue
				.as_mut()
				.unwrap()
				.retired
				.push(old)
				.unwrap_or_else(|_| unreachable!("retirement slot reserved"));
		} else {
			self.history.push_back(old);
		}
		true
	}
	fn align_to_cursor(&mut self) {
		// Keep the at-most-two lookahead segments until the content cursor
		// crosses them. Seeking never accidentally targets a prefetched source.
		let cursor = self.resampler.current_source();
		while self.history.back().is_some_and(|item| item.index >= cursor) {
			let previous = self.history.pop_back().unwrap();
			let next = self.swap_item(previous);
			self.prefetched.push_front(next);
		}
	}

	fn update_shared_playback_state(&mut self) {
		if self.playback_state_manager.playback_state() == PlaybackState::Stopped
			&& let Some(queue) = &self.queue
		{
			queue.tail.store(u64::MAX, Ordering::Release);
		}
		self.shared
			.set_state(self.playback_state_manager.playback_state());
	}

	fn pause(&mut self, fade_out_tween: Tween) {
		self.frame_cache.clear();
		self.playback_state_manager.pause(fade_out_tween);
		self.update_shared_playback_state();
	}

	fn resume(&mut self, start_time: StartTime, fade_in_tween: Tween) {
		self.playback_state_manager
			.resume(start_time, fade_in_tween);
		self.update_shared_playback_state();
	}

	fn stop(&mut self, fade_out_tween: Tween) {
		self.playback_state_manager.stop(fade_out_tween);
		self.update_shared_playback_state();
	}

	#[must_use]
	fn is_playing_backwards(&self) -> bool {
		let mut is_playing_backwards = self.playback_rate.value().0.is_sign_negative();
		if self.reverse {
			is_playing_backwards = !is_playing_backwards
		}
		is_playing_backwards
	}

	/// Updates the current frame index by 1 and pushes a new sample to the resampler.
	fn advance_frame(&mut self) -> bool {
		while !self.transport.playing && self.has_successor() {
			if !self.advance_queue() {
				break;
			}
		}
		if !self.push_frame_to_resampler() {
			return false;
		}
		self.retire_history();
		let previous = self.transport.position;
		if self.is_playing_backwards() {
			self.transport.decrement_position();
			if self.transport.position > previous {
				self.frame_cache.clear();
			}
		} else {
			let end = self
				.transport
				.loop_region
				.map_or(num_frames(&self.frames, self.slice), |(_, end)| end);
			if self.transport.playing && self.has_successor() && self.transport.position + 1 >= end
			{
				self.transport.position = end;
				self.transport.playing = false;
			} else {
				self.transport
					.increment_position(num_frames(&self.frames, self.slice));
				if self.transport.position < previous {
					self.frame_cache.clear();
				}
			}
		}
		true
	}

	fn update_position(&mut self) -> bool {
		if !self.advance_frame() {
			return false;
		}
		if !self.transport.playing
			&& self.resampler.empty()
			&& !self.has_successor()
			&& self.queue.as_ref().is_none_or(|queue| {
				queue
					.tail
					.compare_exchange(self.item, u64::MAX, Ordering::AcqRel, Ordering::Acquire)
					.is_ok()
			}) {
			self.publish_position();
			self.playback_state_manager.mark_as_stopped();
			self.update_shared_playback_state();
		}
		true
	}

	fn seek_to_index(&mut self, index: usize) {
		self.frame_cache.clear();
		if self.playback_state_manager.playback_state() == PlaybackState::Stopped {
			return;
		}
		self.transport
			.seek_to(index, num_frames(&self.frames, self.slice));
		self.frames
			.seek(self.transport.position + self.slice.map_or(0, |s| s.0));
		self.resampler =
			Resampler::for_source(self.transport.position, self.item, self.sample_rate);
		self.fractional_position = 0.0;
		if self
			.queue
			.as_ref()
			.is_some_and(|queue| !queue.started.load(Ordering::Acquire))
		{
			self.queue_preroll = true;
			return;
		}
		self.queue_preroll = false;
		// Discard interpolation history from the old position, including while paused.
		// Completion remains driven by consumption, after a paused sound resumes.
		self.preroll_remaining = 3;
		while self.preroll_remaining > 0 {
			if !self.advance_frame() {
				break;
			}
			self.preroll_remaining -= 1;
		}
	}

	fn push_frame_to_resampler(&mut self) -> bool {
		let frame = if self.transport.playing {
			let start = self.slice.map_or(0, |(start, _)| start);
			let index = self.transport.position + start;
			let end = num_frames(&self.frames, self.slice) + start;
			let backwards = self.is_playing_backwards();
			let Some(frame) = self.frame_cache.frame(index, &self.frames, end, backwards) else {
				return false;
			};
			Some(frame)
		} else {
			None
		};
		self.resampler.push_source_frame(
			frame,
			self.transport.position,
			self.item,
			self.sample_rate,
		);
		true
	}

	fn seek_by(&mut self, amount: f64) {
		let current_position = self.resampler.current_frame_index() as f64
			/ self.resampler.current_sample_rate() as f64;
		self.align_to_cursor();
		let position = current_position + amount;
		let index = (position * self.sample_rate as f64) as usize;
		self.seek_to_index(index);
	}

	fn seek_to(&mut self, position: f64) {
		self.align_to_cursor();
		let index = (position * self.sample_rate as f64).round() as usize;
		self.seek_to_index(index);
	}

	fn read_commands(&mut self) {
		read_commands_into_parameters!(self, volume, panning);
		if let Some(command) = self.command_readers.set_ducking.read() {
			self.ducking
				.get_or_insert_with(|| Parameter::new(Decibels::IDENTITY.into(), Decibels::IDENTITY))
				.set(command.target, command.tween);
		}
		if let Some(command) = self.command_readers.set_playback_rate.read() {
			// A seek can publish during this method. Keep the previous parameter
			// and acknowledgement together until process actually applies the rate.
			self.pending_rate = Some(command);
		}
		if let Some(loop_region) = self.command_readers.set_loop_region.read() {
			self.transport.set_loop_region(
				loop_region,
				self.sample_rate,
				num_frames(&self.frames, self.slice),
			);
		}
		if let Some(tween) = self.command_readers.pause.read() {
			self.pause(tween);
		}
		if let Some((start_time, tween)) = self.command_readers.resume.read() {
			self.resume(start_time, tween);
		}
		if let Some(tween) = self.command_readers.stop.read() {
			self.stop(tween);
		}
		if let Some(amount) = self.command_readers.seek_by.read() {
			self.seek_by(amount);
		}
		if let Some((sequence, position)) = self.command_readers.seek_to.read() {
			self.apply_seek(sequence, position);
		}
	}
	fn apply_seek(&mut self, sequence: u64, position: f64) {
		self.seek_to(position);
		// Publish the applied position before acknowledging this specific request.
		// A newer control-side seek remains observable until its own acknowledgement.
		self.publish_position();
		self.shared.applied_seek.store(sequence, Ordering::Release);
	}

	fn publish_position(&self) {
		let last_played_frame_position = self.resampler.current_frame_index();
		self.shared.cursor_sequence.fetch_add(1, Ordering::SeqCst);
		self.shared
			.item
			.store(self.resampler.current_source(), Ordering::SeqCst);
		self.shared.position.store(
			(last_played_frame_position as f64 / self.resampler.current_sample_rate() as f64)
				.to_bits(),
			Ordering::SeqCst,
		);
		let rate = self.playback_rate.value();
		let (target, remaining) = self
			.playback_rate
			.fixed_transition()
			.unwrap_or((rate, std::time::Duration::ZERO));
		self.shared.rate.store(rate.0.to_bits(), Ordering::SeqCst);
		self.shared
			.rate_target
			.store(target.0.to_bits(), Ordering::SeqCst);
		self.shared
			.rate_remaining_seconds
			.store(remaining.as_secs(), Ordering::SeqCst);
		self.shared
			.rate_remaining_nanos
			.store(u64::from(remaining.subsec_nanos()), Ordering::SeqCst);
		self.shared
			.applied_rate
			.store(self.rate_sequence, Ordering::SeqCst);
		self.shared.cursor_sequence.fetch_add(1, Ordering::SeqCst);
	}
}

impl Sound for StaticSound {
	fn on_start_processing(&mut self) {
		let frames = num_frames(&self.frames, self.slice);
		if frames != self.observed_frames {
			self.observed_frames = frames;
			self.transport.set_loop_region(
				self.item_settings.loop_region,
				self.sample_rate,
				frames,
			);
			if self
				.transport
				.loop_region
				.is_some_and(|(start, end)| start >= end)
			{
				self.transport.loop_region = None;
			}
			self.transport.seek_to(self.transport.position, frames);
		}
		self.publish_position();
		self.read_commands();
	}

	fn process(&mut self, out: &mut [Frame], dt: f64, info: &Info) {
		if self.frames.failed() {
			self.playback_state_manager.mark_as_stopped();
			self.update_shared_playback_state();
			out.fill(Frame::ZERO);
			return;
		}
		// update parameters
		self.volume.update(dt * out.len() as f64, info);
		if let Some(ducking) = &mut self.ducking {
			ducking.update(dt * out.len() as f64, info);
		}
		self.panning.update(dt * out.len() as f64, info);
		let changed_playback_state = self.playback_state_manager.update(
			if self
				.queue
				.as_ref()
				.is_some_and(|queue| !queue.started.load(Ordering::Acquire))
				&& self.playback_state_manager.playback_state() == PlaybackState::Playing
			{
				0.0
			} else {
				dt * out.len() as f64
			},
			info,
		);
		if changed_playback_state {
			self.update_shared_playback_state();
		}

		let will_never_start = self.start_time.update(dt * out.len() as f64, info);
		let advancing = self.start_time == StartTime::Immediate
			&& self.playback_state_manager.playback_state().is_advancing()
			&& self
				.queue
				.as_ref()
				.is_none_or(|queue| queue.started.load(Ordering::Acquire));
		let rate_command = self.pending_rate.take();
		if let Some(command) = rate_command {
			if let Some(baseline) = command.baseline_after(self.rate_sequence) {
				self.playback_rate.set(
					crate::Value::Fixed(baseline),
					Tween {
						duration: std::time::Duration::ZERO,
						..Default::default()
					},
				);
				self.playback_rate.update(0.0, info);
			}
			self.playback_rate
				.set(command.change.target, command.change.tween);
		}
		// Rate ramps use active transport time; zero-duration changes also apply
		// while paused or waiting for the initial queue to be committed.
		self.playback_rate.update(
			if advancing {
				dt * out.len() as f64
			} else {
				0.0
			},
			info,
		);
		if let Some(command) = rate_command {
			self.rate_sequence = command.sequence;
		}
		if will_never_start {
			self.playback_state_manager.mark_as_stopped();
			self.update_shared_playback_state();
		}
		if self.start_time != StartTime::Immediate {
			out.fill(Frame::ZERO);
			return;
		}

		if !self.playback_state_manager.playback_state().is_advancing() {
			out.fill(Frame::ZERO);
			return;
		}
		if self
			.queue
			.as_ref()
			.is_some_and(|queue| !queue.started.load(Ordering::Acquire))
		{
			out.fill(Frame::ZERO);
			return;
		}
		if let Some(queue) = &self.queue {
			let tail = queue.tail.load(Ordering::Acquire);
			if tail != u64::MAX && tail > self.observed_tail {
				// Interpolation may already look into the next loop pass. Rebuild
				// from the current content cursor when the first successor arrives.
				if !self.queue_preroll
					&& self.observed_tail <= self.item
					&& self.transport.loop_region.is_some()
					&& self.resampler.current_source() == self.item
				{
					let fraction = self.fractional_position;
					self.seek_to_index(self.resampler.current_frame_index());
					self.fractional_position = fraction;
				}
				self.observed_tail = tail;
			}
		}
		if self.queue_preroll {
			self.queue_preroll = false;
			self.preroll_remaining = 3;
		}
		while self.preroll_remaining > 0 {
			if !self.advance_frame() {
				out.fill(Frame::ZERO);
				return;
			}
			self.preroll_remaining -= 1;
		}

		// play back audio
		let num_frames = out.len();
		let volume = self.volume.amplitude_block();
		let ducking = self.ducking.as_ref().map(Parameter::amplitude_block);
		let fade = self.playback_state_manager.fade_amplitudes();
		let panning = self.panning.panning_block();
		for (i, frame) in out.iter_mut().enumerate() {
			while self.preroll_remaining > 0 {
				if !self.advance_frame() {
					break;
				}
				self.preroll_remaining -= 1;
			}
			if self.preroll_remaining > 0 {
				*frame = Frame::ZERO;
				continue;
			}
			while self.fractional_position >= 1.0 {
				let previous_rate = self.resampler.current_sample_rate();
				if !self.update_position() {
					*frame = Frame::ZERO;
					break;
				}
				self.fractional_position -= 1.0;
				let next_rate = self.resampler.current_sample_rate();
				if next_rate != previous_rate {
					self.fractional_position *= next_rate as f64 / previous_rate as f64;
				}
			}
			if self.fractional_position >= 1.0 {
				continue;
			}
			if !self.transport.playing && self.resampler.empty() && self.has_successor() {
				if !self.advance_queue() {
					*frame = Frame::ZERO;
					continue;
				}
				self.resampler =
					Resampler::for_source(self.transport.position, self.item, self.sample_rate);
				self.fractional_position = 0.0;
				self.preroll_remaining = 3;
				while self.preroll_remaining > 0 {
					if !self.advance_frame() {
						break;
					}
					self.preroll_remaining -= 1;
				}
				if self.preroll_remaining > 0 {
					*frame = Frame::ZERO;
					continue;
				}
			}
			let time_in_chunk = (i + 1) as f64 / num_frames as f64;
			let volume = volume.at(time_in_chunk)
				* ducking.map_or(1., |gain| gain.at(time_in_chunk));
			let fade_volume = fade.at(time_in_chunk);
			let playback_rate = self.playback_rate.interpolated_value(time_in_chunk);
			let resampler_out = self.resampler.get(self.fractional_position as f32);
			self.fractional_position +=
				self.resampler.current_sample_rate() as f64 * playback_rate.0.abs() * dt;
			while self.fractional_position >= 1.0 {
				let previous_rate = self.resampler.current_sample_rate();
				if !self.update_position() {
					break;
				}
				self.fractional_position -= 1.0;
				let next_rate = self.resampler.current_sample_rate();
				if next_rate != previous_rate {
					self.fractional_position *= next_rate as f64 / previous_rate as f64;
				}
			}
			*frame = panning.apply(resampler_out * fade_volume * volume, time_in_chunk);
		}
	}

	fn finished(&self) -> bool {
		let finished = self.playback_state_manager.playback_state() == PlaybackState::Stopped;
		if finished && let Some(queue) = &self.queue {
			queue.finished.store(true, Ordering::Release);
		}
		finished
	}
}

#[derive(Debug)]
pub(super) struct Shared {
	state: AtomicU8,
	position: AtomicU64,
	applied_seek: AtomicU64,
	item: AtomicU64,
	cursor_sequence: AtomicU64,
	rate: AtomicU64,
	rate_target: AtomicU64,
	rate_remaining_seconds: AtomicU64,
	rate_remaining_nanos: AtomicU64,
	applied_rate: AtomicU64,
}

impl Shared {
	pub fn playback(&self) -> (super::StaticSoundPlayback, u64) {
		loop {
			let sequence = self.cursor_sequence.load(Ordering::SeqCst);
			if !sequence.is_multiple_of(2) {
				continue;
			}
			let playback = super::StaticSoundPlayback {
				item: self.item.load(Ordering::SeqCst),
				position: self.position(),
				rate: PlaybackRate(f64::from_bits(self.rate.load(Ordering::SeqCst))),
				rate_target: PlaybackRate(f64::from_bits(self.rate_target.load(Ordering::SeqCst))),
				rate_remaining: std::time::Duration::new(
					self.rate_remaining_seconds.load(Ordering::SeqCst),
					self.rate_remaining_nanos.load(Ordering::SeqCst) as u32,
				),
			};
			let applied_rate = self.applied_rate.load(Ordering::SeqCst);
			if self.cursor_sequence.load(Ordering::SeqCst) == sequence {
				return (playback, applied_rate);
			}
		}
	}
	pub fn cursor(&self) -> (u64, f64) {
		loop {
			let sequence = self.cursor_sequence.load(Ordering::SeqCst);
			if !sequence.is_multiple_of(2) {
				continue;
			}
			let item = self.item.load(Ordering::SeqCst);
			let position = self.position();
			if self.cursor_sequence.load(Ordering::SeqCst) == sequence {
				return (item, position);
			}
		}
	}
	pub fn applied_seek(&self) -> u64 {
		self.applied_seek.load(Ordering::Acquire)
	}

	pub fn state(&self) -> PlaybackState {
		match self.state.load(Ordering::SeqCst) {
			0 => PlaybackState::Playing,
			1 => PlaybackState::Pausing,
			2 => PlaybackState::Paused,
			3 => PlaybackState::WaitingToResume,
			4 => PlaybackState::Resuming,
			5 => PlaybackState::Stopping,
			6 => PlaybackState::Stopped,
			_ => panic!("Invalid playback state"),
		}
	}

	pub fn set_state(&self, state: PlaybackState) {
		self.state.store(state as u8, Ordering::SeqCst);
	}

	pub fn position(&self) -> f64 {
		f64::from_bits(self.position.load(Ordering::SeqCst))
	}
}
