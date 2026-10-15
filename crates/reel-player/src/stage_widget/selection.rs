//! Selection indicators share activation, focus and author-owned child content.
use super::*;
use reel_ui::{
    kurbo::{BezPath, Circle, Stroke},
    stage::ToggleAppearance,
};

impl SceneWidget {
    pub(super) fn selection_state(&self) -> Option<bool> {
        match self.node.kind {
            NodeKind::Toggle { value, .. } => Some(value),
            NodeKind::Radio { selected, .. } => Some(selected),
            _ => None,
        }
    }

    pub(super) fn paint_selection(&self, ctx: &PaintCtx<'_>, painter: &mut Painter<'_>) {
        let Some(selected) = self.selection_state().filter(|_| self.node.props.decorated) else {
            return;
        };
        let x = self.node.props.padding;
        let y = self.size.height * 0.5;
        let tint = color(self.node.props.color);
        let tint = tint.with_alpha(tint.components[3] * if ctx.is_disabled() { 0.4 } else { 1. });
        let stroke = Stroke::new(2.);
        match self.node.kind {
            NodeKind::Radio { .. } => {
                let center = Point::new(x + 12., y);
                painter
                    .stroke(Circle::new(center, 10.), &stroke, tint)
                    .draw();
                if selected {
                    painter.fill(Circle::new(center, 5.), tint).draw();
                }
            }
            NodeKind::Toggle {
                appearance: ToggleAppearance::Checkbox,
                ..
            } => {
                painter
                    .stroke(
                        Rect::new(x + 2., y - 10., x + 22., y + 10.).to_rounded_rect(2.),
                        &stroke,
                        tint,
                    )
                    .draw();
                if selected {
                    let mut check = BezPath::new();
                    check.move_to((x + 6., y));
                    check.line_to((x + 10., y + 4.));
                    check.line_to((x + 18., y - 5.));
                    painter.stroke(check, &stroke, tint).draw();
                }
            }
            NodeKind::Toggle {
                appearance: ToggleAppearance::Switch,
                ..
            } => {
                painter
                    .stroke(
                        Rect::new(x, y - 8., x + 26., y + 8.).to_rounded_rect(8.),
                        &stroke,
                        tint,
                    )
                    .draw();
                painter
                    .fill(
                        Circle::new(Point::new(x + if selected { 19. } else { 7. }, y), 5.),
                        tint,
                    )
                    .draw();
            }
            _ => unreachable!(),
        }
    }
}
