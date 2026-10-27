use reel_core::budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits};
use std::sync::Arc;

#[test]
fn immutable_views_preserve_allocation_and_transfer_its_complete_capacity() {
    let pool = |category| {
        BudgetPool::new(
            category,
            QueueLimits {
                max_requests: 8,
                max_bytes: 1024,
            },
        )
    };
    let input = pool(BudgetCategory::CpuInput);
    let output = pool(BudgetCategory::CpuOutput);
    let data: Vec<u8> = (0..128).collect();
    let address = data.as_ptr();
    let capacity = data.capacity();
    let bytes = OwnedBytes::new(data, input.reserve(capacity as u64).unwrap()).unwrap();
    let bytes = bytes
        .into_slice(40..120)
        .unwrap()
        .into_slice(20..50)
        .unwrap();
    assert_eq!(bytes.as_slice(), (60..90).collect::<Vec<_>>());
    assert_eq!(bytes.as_slice().as_ptr(), address.wrapping_add(60));
    assert_eq!(bytes.capacity(), capacity);
    let rejected = bytes.with_ticket(output.reserve(30).unwrap()).unwrap_err();
    let (bytes, ticket) = rejected.request;
    drop(ticket);
    assert_eq!(input.usage().bytes, capacity as u64);
    let bytes = bytes
        .with_ticket(output.reserve(capacity as u64).unwrap())
        .unwrap();
    assert_eq!(input.usage().bytes, 0);
    let bytes = Arc::new(bytes);
    let other = bytes.clone();
    drop(bytes);
    assert_eq!(output.usage().bytes, capacity as u64);
    let bytes = Arc::try_unwrap(other).unwrap();
    let invalid = bytes.into_slice(0..31).unwrap_err().request;
    let (data, ticket) = invalid.into_parts();
    assert_eq!(data.as_ptr(), address);
    assert_eq!(data, (60..90).collect::<Vec<_>>());
    assert_eq!(data.capacity(), capacity);
    drop((data, ticket));
    assert_eq!(output.usage().bytes, 0);
}
