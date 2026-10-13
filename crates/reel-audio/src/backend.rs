//! Host-driven audio rendering. Native device output belongs to reel-media.

pub mod mock;
mod renderer;
pub(crate) mod resources;

pub use renderer::*;

/// The default backend used by [`AudioManager`](crate::AudioManager)s.
///
/// The host advances this backend explicitly. Reel's device adapter supplies
/// its own [`Backend`] through reel-media.
pub type DefaultBackend = mock::MockBackend;

/// Connects a [`Renderer`] to a lower level audio API.
pub trait Backend: Sized {
	/// Settings for this backend.
	type Settings;

	/// Errors that can occur when using this backend.
	type Error;

	/// Starts the backend and returns itself and the initial sample rate.
	fn setup(
		settings: Self::Settings,
		internal_buffer_size: usize,
	) -> Result<(Self, u32), Self::Error>;

	/// Sends the renderer to the backend to start audio playback.
	fn start(&mut self, renderer: Renderer) -> Result<(), Self::Error>;
}
