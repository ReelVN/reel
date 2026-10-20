// Copyright 2025 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

//! Types needed for running a Masonry app.

mod cursor;
mod input_scope;
mod layer_stack;
mod pointer;
mod presented_geometry;
pub(crate) use presented_geometry::{ContextGeometry, GeometryRef, GeometryView};
pub use presented_geometry::{
    GeometryError, GeometryLimits, GeometryRecord, InputGeometrySource, PresentedGeometryView,
};
mod render_root;
mod tracing_backend;
mod visual_layers;

pub use render_root::{RenderRoot, RenderRootOptions, RenderRootSignal, WindowSizePolicy};
pub use tracing_backend::{
    TracingSubscriberHasBeenSetError, default_tracing_subscriber, try_init_test_tracing,
    try_init_tracing,
};
pub use visual_layers::{
    CommandOwnerRange, GroupMetadata, SubtreePaint, VisualLayer, VisualLayerKind, VisualLayerPlan,
};

pub(crate) use render_root::{MutateCallback, RenderRootState};
