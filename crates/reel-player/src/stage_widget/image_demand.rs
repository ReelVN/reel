//! Resource and viewport dependencies of a retained stage projection.
use super::*;

impl StageView {
    pub(crate) fn same_image_demand(&self, previous: &Self) -> bool {
        fn visit(node: &Arc<SceneNode>, old: &Arc<SceneNode>) -> bool {
            if Arc::ptr_eq(node, old) {
                return true;
            }
            if !Arc::ptr_eq(&node.node, &old.node)
                && (node.node.id != old.node.id
                    || !props_match(&node.node.props, &old.node.props)
                    || !kind_matches(&node.node.kind, &old.node.kind))
            {
                return false;
            }
            Arc::ptr_eq(&node.children, &old.children)
                || (node.children.len() == old.children.len()
                    && node
                        .children
                        .iter()
                        .zip(old.children.iter())
                        .all(|(a, b)| visit(a, b)))
        }
        self.same_stage(previous)
            && match (&self.root, &previous.root) {
                (None, None) => true,
                (Some(node), Some(old)) => {
                    // Image-free siblings can still move images through flow layout.
                    (!node.image_branch && !old.image_branch) || visit(node, old)
                }
                _ => false,
            }
    }
}

fn props_match(a: &NodeProps, b: &NodeProps) -> bool {
    macro_rules! fields {
        ($($name:ident),+; $($paint:ident),+ $(,)?) => {
            {
                // New properties must be classified alongside request and layout behavior.
                let NodeProps { $($name: _,)+ $($paint: _,)+ } = a;
                true $(&& a.$name == b.$name)+
            }
        };
    }
    fields!(
        flex,
        x,
        y,
        width,
        height,
        max_width,
        max_height,
        align_x,
        align_y,
        stretch,
        visible,
        enabled,
        clip,
        decorated,
        gap,
        padding,
        font_size,
        font_weight,
        letter_spacing,
        word_spacing,
        font,
        text_align,
        writing_mode,
        text_wrap,
        line_height,
        text_fit,
        text_outline,
        text_shadow_x,
        text_shadow_y,
        rotation,
        scale_x,
        scale_y,
        anchor_x,
        anchor_y,
        appearance_origin;
        semantics, opacity, invert, blend, capture_included, modal,
        reveal, reveal_fade, reveal_fade_from, reveal_fade_step,
        text_outline_color, text_shadow_color, color, text_gradient, background,
        border_width, border_color, corner_radius,
        mask, effect, effect_resolution_scale, mask_effect, mask_progress, mask_softness, mask_invert, mask_x, mask_y,
    ) && a.mask.as_ref().map(|mask| mask.sources()) == b.mask.as_ref().map(|mask| mask.sources())
        && effect_matches(&a.effect, &b.effect)
        && effect_matches(&a.mask_effect, &b.mask_effect)
}

pub(super) fn effect_matches(
    a: &Option<Arc<reel_ui::stage::effect::EffectInstance>>,
    b: &Option<Arc<reel_ui::stage::effect::EffectInstance>>,
) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            Arc::ptr_eq(a, b) || (a.definition() == b.definition() && a.textures() == b.textures())
        }
        _ => false,
    }
}

fn kind_matches(a: &NodeKind, b: &NodeKind) -> bool {
    match (a, b) {
        (
            NodeKind::Image {
                source: a,
                alpha_layout: b,
                source_rect: c,
                fit: d,
                slice: e,
                slice_scale: f,
                repeat: g,
                tile_scale: h,
                slice_center: i,
                origin: j,
                sampling: _,
                corner_colors: _,
                adjustment: _,
            },
            NodeKind::Image {
                source: k,
                alpha_layout: l,
                source_rect: m,
                fit: n,
                slice: o,
                slice_scale: p,
                repeat: q,
                tile_scale: r,
                slice_center: s,
                origin: t,
                sampling: _,
                corner_colors: _,
                adjustment: _,
            },
        ) => (a, b, c, d, e, f, g, h, i, j) == (k, l, m, n, o, p, q, r, s, t),
        (NodeKind::Particles(a), NodeKind::Particles(b)) => {
            a.source == b.source
                && a.origin == b.origin
                && a.batches == b.batches
                && a.size_scale == b.size_scale
        }
        (NodeKind::Button { label: a, .. }, NodeKind::Button { label: b, .. }) => a == b,
        _ => a == b,
    }
}
