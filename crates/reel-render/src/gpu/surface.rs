use super::{Gpu, GpuError, Quality, SurfacePass};

#[derive(Debug, PartialEq, Eq)]
pub enum ContractError {
    NoSrgbFormat,
    NoOpaqueAlpha,
    NoFifo,
    MissingRenderAttachment,
}
impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "incompatible surface: {self:?}")
    }
}
impl std::error::Error for ContractError {}

fn select(
    caps: &wgpu::SurfaceCapabilities,
    size: [u32; 2],
    supports_srgb_view: bool,
) -> Result<wgpu::SurfaceConfiguration, ContractError> {
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .or_else(|| {
            // Browser canvases expose unorm base formats, with sRGB view support.
            supports_srgb_view
                .then(|| {
                    caps.formats.iter().copied().find(|format| {
                        matches!(
                            format,
                            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
                        )
                    })
                })
                .flatten()
        })
        .ok_or(ContractError::NoSrgbFormat)?;
    if !caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
        return Err(ContractError::NoOpaqueAlpha);
    }
    if !caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
        return Err(ContractError::NoFifo);
    }
    if !caps.usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
        return Err(ContractError::MissingRenderAttachment);
    }
    Ok(wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size[0].max(1),
        height: size[1].max(1),
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: if format.is_srgb() {
            vec![]
        } else {
            vec![format.add_srgb_suffix()]
        },
        color_space: wgpu::SurfaceColorSpace::Srgb,
    })
}
/// Requery the current adapter/surface pair. No configuration from an old
/// device generation or surface lifetime participates in negotiation.
pub(crate) fn negotiate(
    gpu: &Gpu,
    surface: &wgpu::Surface<'_>,
    size: [u32; 2],
    vsync: bool,
) -> Result<
    (
        wgpu::SurfaceConfiguration,
        SurfacePass,
        Option<wgpu::PresentMode>,
        Quality,
    ),
    GpuError,
> {
    let caps = surface.get_capabilities(&gpu.adapter);
    let mut config = select(&caps, size, cfg!(target_arch = "wasm32"))?;
    let unsynchronized = [wgpu::PresentMode::Immediate, wgpu::PresentMode::Mailbox]
        .into_iter()
        .find(|mode| caps.present_modes.contains(mode));
    if !vsync && let Some(mode) = unsynchronized {
        config.present_mode = mode;
    }
    let quality = gpu.quality([config.width, config.height])?;
    let output_format = config
        .view_formats
        .first()
        .copied()
        .unwrap_or(config.format);
    let output = SurfacePass::new(&gpu.device, output_format)?;
    if !size.contains(&0) {
        surface.configure(&gpu.device, &config);
    }
    gpu.check()?;
    Ok((config, output, unsynchronized, quality))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn caps(format: wgpu::TextureFormat) -> wgpu::SurfaceCapabilities {
        wgpu::SurfaceCapabilities {
            formats: vec![format],
            present_modes: vec![wgpu::PresentMode::Fifo],
            alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
            usages: wgpu::TextureUsages::RENDER_ATTACHMENT,
            ..Default::default()
        }
    }
    #[test]
    fn capability_change_selects_new_format_and_size() {
        let old = select(
            &caps(wgpu::TextureFormat::Bgra8UnormSrgb),
            [640, 420],
            false,
        )
        .unwrap();
        let fresh = select(&caps(wgpu::TextureFormat::Rgba8UnormSrgb), [0, 0], false).unwrap();
        assert_ne!(old.format, fresh.format);
        assert_eq!((fresh.width, fresh.height), (1, 1));
        assert_eq!(fresh.alpha_mode, wgpu::CompositeAlphaMode::Opaque);
        assert_eq!(fresh.present_mode, wgpu::PresentMode::Fifo);
    }
    #[test]
    fn incompatible_capabilities_are_refused() {
        let mut fresh = caps(wgpu::TextureFormat::Rgba8Unorm);
        assert_eq!(
            select(&fresh, [640, 420], false),
            Err(ContractError::NoSrgbFormat)
        );
        fresh.formats = vec![wgpu::TextureFormat::Rgba8UnormSrgb];
        fresh.alpha_modes = vec![wgpu::CompositeAlphaMode::PreMultiplied];
        assert_eq!(
            select(&fresh, [640, 420], false),
            Err(ContractError::NoOpaqueAlpha)
        );
        fresh.alpha_modes = vec![wgpu::CompositeAlphaMode::Opaque];
        fresh.present_modes.clear();
        assert_eq!(
            select(&fresh, [640, 420], false),
            Err(ContractError::NoFifo)
        );
        fresh.present_modes.push(wgpu::PresentMode::Fifo);
        fresh.usages = wgpu::TextureUsages::COPY_DST;
        assert_eq!(
            select(&fresh, [640, 420], false),
            Err(ContractError::MissingRenderAttachment)
        );
    }

    #[test]
    fn browser_canvas_uses_the_preferred_base_format_with_an_srgb_view() {
        let mut browser = caps(wgpu::TextureFormat::Bgra8Unorm);
        browser.formats.push(wgpu::TextureFormat::Rgba8Unorm);
        browser.formats.push(wgpu::TextureFormat::Rgba16Float);
        let config = select(&browser, [640, 420], true).unwrap();
        assert_eq!(config.format, wgpu::TextureFormat::Bgra8Unorm);
        assert_eq!(config.view_formats, [wgpu::TextureFormat::Bgra8UnormSrgb]);
    }
}
