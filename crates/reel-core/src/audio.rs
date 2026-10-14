//! Platform-independent audio routing shared by hosts and the media adapter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(usize)]
pub enum AudioGroup {
    Music,
    Voice,
    #[default]
    Effects,
    Ui,
}
impl AudioGroup {
    pub const ALL: [Self; 4] = [Self::Music, Self::Voice, Self::Effects, Self::Ui];
}
