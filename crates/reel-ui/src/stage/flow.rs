// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Compact author layout options. Algorithm styles borrow these values.

/// Distribution of free space along a row or column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Options stored only by row and column nodes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FlowOptions {
    pub wrap: bool,
    pub justify: Justify,
    /// Cross-axis line spacing; None inherits the common gap.
    pub line_gap: Option<f32>,
}

/// Equal-track grid options, stored only by grid nodes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridOptions {
    pub columns: u16,
    pub column_width: Option<f32>,
    pub row_height: Option<f32>,
    pub column_gap: Option<f32>,
    pub row_gap: Option<f32>,
}
impl Default for GridOptions {
    fn default() -> Self {
        Self {
            columns: 1,
            column_width: None,
            row_height: None,
            column_gap: None,
            row_gap: None,
        }
    }
}
