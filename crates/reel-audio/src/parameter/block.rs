//! Copyable block samplers keep invariant nonlinear coefficients off sample loops.
use super::Parameter;
use crate::{Decibels, Frame, Panning, Tweenable};

#[derive(Clone, Copy)]
pub(crate) enum Amplitude {
	Fixed(f32),
	Changing(Decibels, Decibels),
}
impl Amplitude {
	pub(crate) fn at(self, amount: f64) -> f32 {
		match self {
			Self::Fixed(value) => value,
			Self::Changing(previous, current) => {
				Decibels::interpolate(previous, current, amount).as_amplitude()
			}
		}
	}
}

#[derive(Clone, Copy)]
pub(crate) enum Pan {
	Fixed(Option<[f32; 2]>),
	Changing(Panning, Panning),
}
impl Pan {
	pub(crate) fn apply(self, frame: Frame, amount: f64) -> Frame {
		match self {
			Self::Fixed(weights) => frame.panned_with_weights(weights),
			Self::Changing(previous, current) => {
				frame.panned(Panning::interpolate(previous, current, amount))
			}
		}
	}
}

impl Parameter<Decibels> {
	pub(crate) fn amplitude_block(&self) -> Amplitude {
		if self.raw_value == self.previous_raw_value && self.raw_value.0.is_finite() {
			Amplitude::Fixed(self.raw_value.as_amplitude())
		} else {
			Amplitude::Changing(self.previous_raw_value, self.raw_value)
		}
	}
}
impl Parameter<Panning> {
	pub(crate) fn panning_block(&self) -> Pan {
		if self.raw_value == self.previous_raw_value && self.raw_value.0.is_finite() {
			Pan::Fixed(Frame::panning_weights(self.raw_value))
		} else {
			Pan::Changing(self.previous_raw_value, self.raw_value)
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Value;

	fn same(actual: f32, expected: f32) {
		if expected.is_nan() {
			assert!(actual.is_nan());
		} else {
			assert_eq!(actual.to_bits(), expected.to_bits());
		}
	}

	#[test]
	fn constant_and_changing_gains_preserve_interpolation_and_nonfinite_values() {
		for (previous, current) in [
			(-70., -70.),
			(-6., -6.),
			(0., 0.),
			(6., 6.),
			(-60., -6.),
			(-6., 0.),
			(f32::INFINITY, f32::INFINITY),
			(f32::NAN, 0.),
		] {
			let mut parameter = Parameter::new(Value::Fixed(Decibels(current)), Decibels::IDENTITY);
			parameter.previous_raw_value = Decibels(previous);
			let block = parameter.amplitude_block();
			for amount in [0., 0.003, 0.5, 1.] {
				same(
					block.at(amount),
					parameter.interpolated_value(amount).as_amplitude(),
				);
			}
		}
	}

	#[test]
	fn pan_weights_preserve_center_roots_multiply_order_and_changing_samples() {
		for (previous, current) in [
			(0., 0.),
			(-0., 0.),
			(-0.35, -0.35),
			(1., 1.),
			(3., 3.),
			(-1., 0.25),
			(f32::INFINITY, f32::INFINITY),
			(f32::NAN, 0.),
		] {
			let mut parameter = Parameter::new(Value::Fixed(Panning(current)), Panning::CENTER);
			parameter.previous_raw_value = Panning(previous);
			let block = parameter.panning_block();
			for frame in [
				Frame::new(0.125, -0.375),
				Frame::new(-0., 0.),
				Frame::new(f32::MAX, -f32::MAX),
			] {
				for amount in [0., 0.003, 0.5, 1.] {
					let expected = frame.panned(parameter.interpolated_value(amount));
					let actual = block.apply(frame, amount);
					same(actual.left, expected.left);
					same(actual.right, expected.right);
				}
			}
		}
	}
}
