use super::*;

fn pool() -> BudgetPool {
    BudgetPool::new(
        reel_core::budget::BudgetCategory::Temporary,
        reel_core::budget::QueueLimits {
            max_requests: 256,
            max_bytes: 1 << 20,
        },
    )
}

fn bitmap(pool: &BudgetPool, value: u8) -> GlyphBitmap {
    GlyphBitmap {
        x: 0.,
        y: 0.,
        size: [6, 6],
        alpha: vec![value; 36],
        _storage: pool.reserve(36).unwrap(),
    }
}

#[test]
fn merged_writes_preserve_live_gaps_borders_pages_and_budget_lifetime() {
    let pool = pool();
    let mut batch = Batch::default();
    // The old glyph at x=16 must survive. A second page has the same coordinates.
    for (page, x, y, value) in [
        (0, 0, 0, 1),
        (0, 8, 0, 2),
        (0, 0, 8, 3),
        (0, 8, 8, 4),
        (0, 24, 0, 5),
        (1, 0, 0, 6),
    ] {
        batch
            .push(page, [x, y], bitmap(&pool, value), &pool, u64::MAX)
            .unwrap();
    }
    let mut images = [[99u8; 32 * 16]; 2];
    let mut uploads = 0;
    let mut accepted = Vec::new();
    batch
        .flush(
            |a, b| a == b,
            |upload| {
                uploads += 1;
                let stride = upload.size[0] as usize;
                let mut data = vec![0; stride * upload.size[1] as usize];
                upload.write_rows(stride, |offset, source| {
                    data[offset..offset + source.len()].copy_from_slice(source)
                });
                for y in 0..upload.size[1] as usize {
                    let at = (upload.origin[1] as usize + y) * 32 + upload.origin[0] as usize;
                    images[*upload.owner][at..at + stride]
                        .copy_from_slice(&data[y * stride..(y + 1) * stride]);
                }
                Ok(())
            },
            |owner| accepted.push(*owner),
        )
        .unwrap();
    assert_eq!(uploads, 3);
    assert_eq!(accepted, [0, 0, 0, 0, 0, 1]);
    for y in 0..16 {
        for x in 0..32 {
            let expected = if x < 16 {
                if x % 8 == 0 || x % 8 == 7 || y % 8 == 0 || y % 8 == 7 {
                    0
                } else {
                    1 + (x / 8) as u8 + 2 * (y / 8) as u8
                }
            } else if x >= 24 && y < 8 {
                if x % 8 == 0 || x % 8 == 7 || y == 0 || y == 7 {
                    0
                } else {
                    5
                }
            } else {
                99
            };
            assert_eq!(images[0][y * 32 + x], expected, "({x}, {y})");
        }
    }
    assert_eq!(images[1][33], 6);
    assert_eq!(images[1][16], 99);
    assert_eq!(pool.usage().bytes, batch.metadata.as_ref().unwrap().bytes());
    drop(batch);
    assert_eq!(pool.usage().bytes, 0);
}

#[test]
fn failed_upload_keeps_owners_and_bytes_until_retry_or_cancellation() {
    let pool = pool();
    let mut batch = Batch::default();
    for index in 0..MAX_PATCHES {
        batch
            .push(0, [index as u32 * 8, 0], bitmap(&pool, 4), &pool, u64::MAX)
            .unwrap();
    }
    assert!(batch.full(&bitmap(&pool, 4)));
    let retained = pool.usage().bytes;
    assert!(
        batch
            .flush(
                |a, b| a == b,
                |_| Err(GpuError::InvalidSize),
                |_| panic!("failed upload was published")
            )
            .is_err()
    );
    assert_eq!(pool.usage().bytes, retained);
    let mut writes = 0;
    batch
        .flush(
            |a, b| a == b,
            |upload| {
                writes += 1;
                assert_eq!(upload.size, [512, 8]);
                Ok(())
            },
            |_| (),
        )
        .unwrap();
    assert_eq!(writes, 1);
    drop(batch);
    assert_eq!(pool.usage().bytes, 0);
}
