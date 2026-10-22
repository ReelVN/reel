// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Flex and grid algorithms over borrowed children and the existing widget measurements.

use crate::kurbo::{Axis, Point, Size};
use crate::layout::{LayoutSize, LenDef, LenReq, Length, SizeDef};
use crate::stage::{FlowOptions, GridOptions, Justify, NodeKind, NodeProps};
use taffy::style::*;
use taffy::style_helpers::{TaffyAuto, fr, length};
use taffy::{Layout, LayoutInput, LayoutOutput, NodeId, RunMode, SizingMode};

/// Immediate children of a layout container. Measurements use the host's existing cache.
pub trait FlowChildren {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn node(&self, index: usize) -> (&NodeKind, &NodeProps);
    fn measure(&mut self, index: usize, size: SizeDef, context: LayoutSize) -> Size;
    fn place(&mut self, index: usize, size: Size, origin: Point);
}

/// Whether this node owns a flex/grid layout.
pub fn is_flow(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Row(_) | NodeKind::Column(_) | NodeKind::Grid(_)
    )
}

/// Measure one axis without storing a second tree or per-node algorithm cache.
pub fn measure_flow(
    kind: &NodeKind,
    props: &NodeProps,
    children: &mut impl FlowChildren,
    axis: Axis,
    request: LenReq,
    cross: Option<Length>,
) -> Length {
    let available = match request {
        LenReq::MinContent => AvailableSpace::MinContent,
        LenReq::MaxContent => AvailableSpace::MaxContent,
        LenReq::FitContent(v) => AvailableSpace::Definite(v.get() as f32),
    };
    let cross = cross.map(|v| v.get() as f32);
    let (known, space) = if axis == Axis::Horizontal {
        (
            taffy::Size {
                width: None,
                height: cross,
            },
            taffy::Size {
                width: available,
                height: cross.map_or(AvailableSpace::MaxContent, AvailableSpace::Definite),
            },
        )
    } else {
        (
            taffy::Size {
                width: cross,
                height: None,
            },
            taffy::Size {
                width: cross.map_or(AvailableSpace::MaxContent, AvailableSpace::Definite),
                height: available,
            },
        )
    };
    let output = calculate(
        kind,
        props,
        children,
        LayoutInput {
            known_dimensions: known,
            available_space: space,
            parent_size: known,
            run_mode: RunMode::ComputeSize,
            sizing_mode: SizingMode::InherentSize,
            axis: taffy::RequestedAxis::Both,
            ..LayoutInput::HIDDEN
        },
    );
    Length::px(f64::from(if axis == Axis::Horizontal {
        output.width
    } else {
        output.height
    }))
}

/// Place children using the same measured boxes later consumed by input and drawing.
pub fn layout_flow(
    kind: &NodeKind,
    props: &NodeProps,
    children: &mut impl FlowChildren,
    size: Size,
) {
    let known = taffy::Size {
        width: Some(size.width as f32),
        height: Some(size.height as f32),
    };
    calculate(
        kind,
        props,
        children,
        LayoutInput {
            known_dimensions: known,
            parent_size: known,
            available_space: taffy::Size {
                width: AvailableSpace::Definite(size.width as f32),
                height: AvailableSpace::Definite(size.height as f32),
            },
            run_mode: RunMode::PerformLayout,
            sizing_mode: SizingMode::InherentSize,
            axis: taffy::RequestedAxis::Both,
            ..LayoutInput::HIDDEN
        },
    );
}

fn calculate(
    kind: &NodeKind,
    props: &NodeProps,
    children: &mut impl FlowChildren,
    input: LayoutInput,
) -> taffy::Size<f32> {
    let mut tree = Tree {
        kind,
        props,
        children,
    };
    let mut size = match kind {
        NodeKind::Row(_) | NodeKind::Column(_) => {
            taffy::compute_flexbox_layout(&mut tree, NodeId::from(0usize), input).size
        }
        NodeKind::Grid(_) => {
            taffy::compute_grid_layout(&mut tree, NodeId::from(0usize), input).size
        }
        _ => unreachable!("flow container"),
    };
    // A flow-axis overlay still contributes its unstretched cross-axis content.
    let row = matches!(kind, NodeKind::Row(_));
    let cross_known = if row {
        input.known_dimensions.height
    } else {
        input.known_dimensions.width
    };
    if !matches!(kind, NodeKind::Grid(_)) && cross_known.is_none() {
        let flow_axis = if row {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let inner = Size::new(
            (f64::from(size.width) - props.padding * 2.).max(0.),
            (f64::from(size.height) - props.padding * 2.).max(0.),
        );
        for index in 0..tree.children.len() {
            let (child_kind, child_props) = tree.children.node(index);
            if !child_props.visible
                || matches!(child_kind, NodeKind::Cursor)
                || !child_props.stretch.axis(flow_axis)
                || child_props.stretch.axis(flow_axis.cross())
            {
                continue;
            }
            let fixed = LenDef::Fixed(Length::px(inner.get_coord(flow_axis)));
            let definition = if row {
                SizeDef::new(fixed, LenDef::MaxContent)
            } else {
                SizeDef::new(LenDef::MaxContent, fixed)
            };
            let measured = tree.children.measure(index, definition, inner.into());
            let cross = (measured.get_coord(flow_axis.cross()) + 2. * props.padding) as f32;
            if row {
                size.height = size.height.max(cross);
            } else {
                size.width = size.width.max(cross);
            }
        }
    }
    size
}

#[derive(Clone, Copy)]
struct StyleRef<'a> {
    kind: &'a NodeKind,
    props: &'a NodeProps,
    parent: Option<&'a NodeKind>,
}
impl StyleRef<'_> {
    fn row(&self) -> bool {
        matches!(self.parent.unwrap_or(self.kind), NodeKind::Row(_))
    }
    fn options(&self) -> FlowOptions {
        match self.kind {
            NodeKind::Row(v) | NodeKind::Column(v) => *v,
            _ => FlowOptions::default(),
        }
    }
    fn grid(&self) -> GridOptions {
        match self.kind {
            NodeKind::Grid(v) => *v,
            _ => GridOptions::default(),
        }
    }
    fn overlay(&self) -> bool {
        self.parent
            .is_some_and(|p| matches!(p, NodeKind::Row(_) | NodeKind::Column(_)))
            && self.props.stretch.axis(if self.row() {
                Axis::Horizontal
            } else {
                Axis::Vertical
            })
    }
}
impl CoreStyle for StyleRef<'_> {
    type CustomIdent = String;
    fn box_generation_mode(&self) -> BoxGenerationMode {
        if !self.props.visible || matches!(self.kind, NodeKind::Cursor) || self.overlay() {
            BoxGenerationMode::None
        } else {
            BoxGenerationMode::Normal
        }
    }
    fn size(&self) -> taffy::Size<Dimension> {
        let flexible = self
            .parent
            .is_some_and(|p| matches!(p, NodeKind::Row(_) | NodeKind::Column(_)))
            && self.props.flex > 0.;
        let dimension = |axis, value: Option<f64>| {
            if self.parent.is_some()
                && (self.props.stretch.axis(axis)
                    || flexible && (axis == Axis::Horizontal) == self.row())
            {
                Dimension::AUTO
            } else {
                value.map_or(Dimension::AUTO, |v| length(v as f32))
            }
        };
        taffy::Size {
            width: dimension(Axis::Horizontal, self.props.width),
            height: dimension(Axis::Vertical, self.props.height),
        }
    }
    fn min_size(&self) -> taffy::Size<LengthPercentageAuto> {
        taffy::Size {
            width: length(0.),
            height: length(0.),
        }
    }
    fn max_size(&self) -> taffy::Size<LengthPercentageAuto> {
        let maximum = |axis, value: Option<f64>| {
            if self.parent.is_some() && self.props.stretch.axis(axis) {
                LengthPercentageAuto::AUTO
            } else {
                value.map_or(LengthPercentageAuto::AUTO, |v| length(v as f32))
            }
        };
        taffy::Size {
            width: maximum(Axis::Horizontal, self.props.max_width),
            height: maximum(Axis::Vertical, self.props.max_height),
        }
    }
    fn padding(&self) -> taffy::Rect<LengthPercentage> {
        // Child measurements already include their own padding.
        {
            let v = length(if self.parent.is_none() {
                self.props.padding as f32
            } else {
                0.
            });
            taffy::Rect {
                left: v,
                right: v,
                top: v,
                bottom: v,
            }
        }
    }
}
impl FlexboxContainerStyle for StyleRef<'_> {
    fn flex_direction(&self) -> FlexDirection {
        if self.row() {
            FlexDirection::Row
        } else {
            FlexDirection::Column
        }
    }
    fn flex_wrap(&self) -> FlexWrap {
        if self.options().wrap {
            FlexWrap::Wrap
        } else {
            FlexWrap::NoWrap
        }
    }
    fn gap(&self) -> taffy::Size<LengthPercentage> {
        let main = length(self.props.gap as f32);
        let cross = length(self.options().line_gap.unwrap_or(self.props.gap as f32));
        if self.row() {
            taffy::Size {
                width: main,
                height: cross,
            }
        } else {
            taffy::Size {
                width: cross,
                height: main,
            }
        }
    }
    fn align_content(&self) -> Option<AlignContent> {
        Some(AlignContent::START)
    }
    fn align_items(&self) -> Option<AlignItems> {
        Some(AlignItems::START)
    }
    fn justify_content(&self) -> Option<JustifyContent> {
        Some(match self.options().justify {
            Justify::Start => JustifyContent::START,
            Justify::Center => JustifyContent::CENTER,
            Justify::End => JustifyContent::END,
            Justify::SpaceBetween => JustifyContent::SPACE_BETWEEN,
            Justify::SpaceAround => JustifyContent::SPACE_AROUND,
            Justify::SpaceEvenly => JustifyContent::SPACE_EVENLY,
        })
    }
}
impl FlexboxItemStyle for StyleRef<'_> {
    fn flex_grow(&self) -> f32 {
        self.props.flex as f32
    }
    fn flex_shrink(&self) -> f32 {
        if self.props.flex > 0. { 1. } else { 0. }
    }
    fn flex_basis(&self) -> Dimension {
        if self.props.flex > 0. {
            length(0.)
        } else {
            Dimension::AUTO
        }
    }
    fn align_self(&self) -> Option<AlignSelf> {
        Some(
            if self.props.stretch.axis(if self.row() {
                Axis::Vertical
            } else {
                Axis::Horizontal
            }) {
                AlignSelf::STRETCH
            } else {
                AlignSelf::START
            },
        )
    }
}
impl GridContainerStyle for StyleRef<'_> {
    type Repetition<'a>
        = <taffy::Style as GridContainerStyle>::Repetition<'a>
    where
        Self: 'a;
    type TemplateTrackList<'a>
        = std::iter::RepeatN<GenericGridTemplateComponent<String, Self::Repetition<'a>>>
    where
        Self: 'a;
    type AutoTrackList<'a>
        = std::iter::Once<TrackSizingFunction>
    where
        Self: 'a;
    type TemplateLineNames<'a>
        = <taffy::Style as GridContainerStyle>::TemplateLineNames<'a>
    where
        Self: 'a;
    type GridTemplateAreas<'a>
        = std::iter::Empty<GridTemplateArea<String>>
    where
        Self: 'a;
    fn grid_template_rows(&self) -> Option<Self::TemplateTrackList<'_>> {
        None
    }
    fn grid_template_columns(&self) -> Option<Self::TemplateTrackList<'_>> {
        let grid = self.grid();
        let track = grid.column_width.map_or_else(|| fr(1.), length);
        Some(std::iter::repeat_n(
            GenericGridTemplateComponent::Single(track),
            usize::from(grid.columns),
        ))
    }
    fn grid_auto_rows(&self) -> Self::AutoTrackList<'_> {
        std::iter::once(
            self.grid()
                .row_height
                .map_or(TrackSizingFunction::AUTO, length),
        )
    }
    fn grid_auto_columns(&self) -> Self::AutoTrackList<'_> {
        std::iter::once(TrackSizingFunction::AUTO)
    }
    fn grid_template_areas(&self) -> Option<Self::GridTemplateAreas<'_>> {
        None
    }
    fn grid_template_column_names(&self) -> Option<Self::TemplateLineNames<'_>> {
        None
    }
    fn grid_template_row_names(&self) -> Option<Self::TemplateLineNames<'_>> {
        None
    }
    fn gap(&self) -> taffy::Size<LengthPercentage> {
        let grid = self.grid();
        taffy::Size {
            width: length(grid.column_gap.unwrap_or(self.props.gap as f32)),
            height: length(grid.row_gap.unwrap_or(self.props.gap as f32)),
        }
    }
    fn align_content(&self) -> Option<AlignContent> {
        Some(AlignContent::START)
    }
    fn justify_content(&self) -> Option<JustifyContent> {
        Some(JustifyContent::START)
    }
}
impl GridItemStyle for StyleRef<'_> {
    fn align_self(&self) -> Option<AlignSelf> {
        Some(if self.props.stretch.axis(Axis::Vertical) {
            AlignSelf::STRETCH
        } else {
            AlignSelf::START
        })
    }
    fn justify_self(&self) -> Option<JustifySelf> {
        Some(
            if self.props.width.is_none() || self.props.stretch.axis(Axis::Horizontal) {
                JustifySelf::STRETCH
            } else {
                JustifySelf::START
            },
        )
    }
}

struct Tree<'a, C> {
    kind: &'a NodeKind,
    props: &'a NodeProps,
    children: &'a mut C,
}
impl<C: FlowChildren> Tree<'_, C> {
    fn style(&self, id: NodeId) -> StyleRef<'_> {
        let index = usize::from(id);
        if index == 0 {
            StyleRef {
                kind: self.kind,
                props: self.props,
                parent: None,
            }
        } else {
            let (kind, props) = self.children.node(index - 1);
            StyleRef {
                kind,
                props,
                parent: Some(self.kind),
            }
        }
    }
}
impl<C: FlowChildren> taffy::TraversePartialTree for Tree<'_, C> {
    type ChildIter<'a>
        = std::iter::Map<std::ops::Range<usize>, fn(usize) -> NodeId>
    where
        Self: 'a;
    fn child_ids(&self, id: NodeId) -> Self::ChildIter<'_> {
        (1..self.child_count(id) + 1).map(NodeId::from)
    }
    fn child_count(&self, id: NodeId) -> usize {
        if usize::from(id) == 0 {
            self.children.len()
        } else {
            0
        }
    }
    fn get_child_id(&self, _: NodeId, index: usize) -> NodeId {
        NodeId::from(index + 1)
    }
}
impl<C: FlowChildren> taffy::LayoutPartialTree for Tree<'_, C> {
    type CustomIdent = String;
    type CoreContainerStyle<'a>
        = StyleRef<'a>
    where
        Self: 'a;
    fn get_core_container_style(&self, id: NodeId) -> StyleRef<'_> {
        self.style(id)
    }
    fn set_unrounded_layout(&mut self, id: NodeId, layout: &Layout) {
        if self.style(id).box_generation_mode() == BoxGenerationMode::None {
            return;
        }
        self.children.place(
            usize::from(id) - 1,
            Size::new(layout.size.width.into(), layout.size.height.into()),
            Point::new(layout.location.x.into(), layout.location.y.into()),
        );
    }
    fn compute_child_layout(&mut self, id: NodeId, input: LayoutInput) -> LayoutOutput {
        if self.style(id).box_generation_mode() == BoxGenerationMode::None {
            return LayoutOutput::HIDDEN;
        }
        let dimension = |known: Option<f32>, space: AvailableSpace| match (known, space) {
            (Some(v), _) => LenDef::Fixed(Length::px(v.into())),
            (_, AvailableSpace::Definite(v)) => LenDef::FitContent(Length::px(v.max(0.).into())),
            (_, AvailableSpace::MinContent) => LenDef::MinContent,
            (_, AvailableSpace::MaxContent) => LenDef::MaxContent,
        };
        let size = self.children.measure(
            usize::from(id) - 1,
            SizeDef::new(
                dimension(input.known_dimensions.width, input.available_space.width),
                dimension(input.known_dimensions.height, input.available_space.height),
            ),
            match (input.parent_size.width, input.parent_size.height) {
                (Some(w), Some(h)) => LayoutSize::new(Length::px(w.into()), Length::px(h.into())),
                (w, h) => LayoutSize::maybe(
                    if w.is_some() {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    },
                    w.or(h).map(|v| Length::px(v.into())),
                ),
            },
        );
        LayoutOutput::from_outer_size(taffy::Size {
            width: size.width as f32,
            height: size.height as f32,
        })
    }
}
impl<C: FlowChildren> taffy::LayoutFlexboxContainer for Tree<'_, C> {
    type FlexboxContainerStyle<'a>
        = StyleRef<'a>
    where
        Self: 'a;
    type FlexboxItemStyle<'a>
        = StyleRef<'a>
    where
        Self: 'a;
    fn get_flexbox_container_style(&self, id: NodeId) -> StyleRef<'_> {
        self.style(id)
    }
    fn get_flexbox_child_style(&self, id: NodeId) -> StyleRef<'_> {
        self.style(id)
    }
}
impl<C: FlowChildren> taffy::LayoutGridContainer for Tree<'_, C> {
    type GridContainerStyle<'a>
        = StyleRef<'a>
    where
        Self: 'a;
    type GridItemStyle<'a>
        = StyleRef<'a>
    where
        Self: 'a;
    fn get_grid_container_style(&self, id: NodeId) -> StyleRef<'_> {
        self.style(id)
    }
    fn get_grid_child_style(&self, id: NodeId) -> StyleRef<'_> {
        self.style(id)
    }
}
