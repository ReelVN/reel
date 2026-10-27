//! Thread-local allocation measurements for library contract and cost probes.
pub(crate) fn measure<T>(work: impl FnOnce() -> T) -> (T, allocation_counter::AllocationInfo) {
    let mut result = None;
    let info = allocation_counter::measure(|| result = Some(work()));
    (result.unwrap(), info)
}
