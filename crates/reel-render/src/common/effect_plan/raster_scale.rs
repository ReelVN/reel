//! Test-only raster experiments. Authored coordinates keep their original domain.

// A finite integer catalogue avoids logarithms or floating-point thresholds in
// frame planning. Each next size is ceil(previous * 9/8); small sizes may round
// by one pixel. Actual allocation, format and samples still use the common plan.
const SIZES: [u32; 256] = {
    let mut values = [u32::MAX; 256];
    values[0] = 1;
    let mut i = 1;
    while i < values.len() {
        let next = (values[i - 1] as u64 * 9).div_ceil(8);
        if next > u32::MAX as u64 {
            break;
        }
        values[i] = next as u32;
        i += 1;
    }
    values
};

fn enabled() -> bool {
    testing::OVERRIDE.get().unwrap_or(false)
}

pub(super) fn select(ideal: [u32; 2], max_dimension: u32) -> [u32; 2] {
    if !enabled() {
        return ideal;
    }
    let bucket = ideal.map(|size| SIZES[SIZES.partition_point(|&bound| bound < size)]);
    if bucket.iter().any(|&size| size > max_dimension) {
        ideal
    } else {
        bucket
    }
}

#[cfg(test)]
pub(crate) mod testing {
    std::thread_local! {
        pub(super) static OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
        static ORIENTATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    pub(crate) fn orientation_stable() -> bool {
        ORIENTATION.get()
    }
    pub(crate) fn with_orientation_stable<T>(run: impl FnOnce() -> T) -> T {
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) {
                ORIENTATION.set(self.0);
            }
        }
        let _restore = Restore(ORIENTATION.replace(true));
        with_enabled(false, run)
    }
    /// A test-only candidate for rotation-invariant local rasterization. The
    /// largest singular value covers either local axis under any rotation.
    pub(crate) fn orientation_grid(
        ideal: [u32; 2],
        bounds: [f64; 2],
        physical: reel_ui::kurbo::Affine,
        max_dimension: u32,
    ) -> [u32; 2] {
        if !orientation_stable() {
            return ideal;
        }
        let [a, b, c, d, _, _] = physical.as_coeffs();
        let sigma = ((a + d).hypot(b - c) + (a - d).hypot(b + c)) * 0.5;
        // Round the density upwards at GPU float precision. This keeps tiny
        // f64 rotation-rounding changes from crossing an integer size boundary.
        let density = f64::from((sigma as f32).next_up());
        let selected = bounds.map(|value| (value * density).ceil().max(1.) as u32);
        assert!(selected.iter().all(|&size| size <= max_dimension));
        assert!(selected.iter().zip(ideal).all(|(&size, old)| size >= old));
        selected
    }
    pub(crate) fn with_enabled<T>(enabled: bool, run: impl FnOnce() -> T) -> T {
        struct Restore(Option<bool>);
        impl Drop for Restore {
            fn drop(&mut self) {
                OVERRIDE.set(self.0);
            }
        }
        let _restore = Restore(OVERRIDE.replace(Some(enabled)));
        run()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_keeps_exact_sizes_and_upward_catalogue_has_bounded_intervals() {
        for width in 1..=4096 {
            let ideal = [width, 4097 - width];
            assert_eq!(testing::with_enabled(false, || select(ideal, 4096)), ideal);
            let selected = testing::with_enabled(true, || select(ideal, 8192));
            for (before, after) in ideal.into_iter().zip(selected) {
                assert!(after >= before);
                assert!(after <= (u64::from(before) * 9).div_ceil(8) as u32);
            }
        }
    }

    #[test]
    fn one_device_limit_failure_restores_the_whole_exact_grid() {
        assert_eq!(
            testing::with_enabled(true, || select([4096, 71], 4096)),
            [4096, 71]
        );
        assert_ne!(
            testing::with_enabled(true, || select([4000, 71], 8192)),
            [4000, 71]
        );
        let a = testing::with_enabled(true, || select([257, 195], 4096));
        assert_eq!(a, testing::with_enabled(true, || select([258, 196], 4096)));
    }
}
