use super::*;

pub(super) fn gap(h: &Heap, s: Value) -> Result<f64, AdapterError> {
    stage::number(
        h.table_entry(
            h.host_fields(node(h, s)?)?,
            stage::NodeProperty::Gap.index() as u32,
        )?
        .1,
    )
}
pub(super) fn height(h: &Heap, s: Value, e: Value) -> Result<f64, AdapterError> {
    if h.sequence_get(e, EPOCH)?.as_int() == Some(int(h, s, S::Epoch)?) {
        let value = stage::number(h.sequence_get(e, HEIGHT)?)?;
        if value > 0. {
            return Ok(value);
        }
    }
    num(h, s, S::Estimate)
}
// A Fenwick tree of measured counts and height sums. Unknown rows use the
// current sample mean without rewriting the tree when that estimate changes.
// Epoch tags invalidate width-dependent measurements in O(1).
pub(super) fn sum(
    h: &Heap,
    tree: Value,
    mut n: u32,
    epoch: i64,
) -> Result<(i64, f64), AdapterError> {
    let mut count = 0;
    let mut value = 0.;
    while n > 0 {
        if h.sequence_get(tree, n * 3)?.as_int() == Some(epoch) {
            count += h.sequence_get(tree, n * 3 + 1)?.as_int().unwrap();
            value += stage::number(h.sequence_get(tree, n * 3 + 2)?)?;
        }
        n &= n - 1;
    }
    Ok((count, value))
}
pub(super) fn add(
    h: &mut Heap,
    tree: Value,
    count: u32,
    mut i: u32,
    epoch: i64,
    measured_delta: i64,
    height_delta: f64,
) -> Result<(), AdapterError> {
    i += 1;
    while i <= count {
        let (measured, height) = if h.sequence_get(tree, i * 3)?.as_int() == Some(epoch) {
            (
                h.sequence_get(tree, i * 3 + 1)?.as_int().unwrap(),
                stage::number(h.sequence_get(tree, i * 3 + 2)?)?,
            )
        } else {
            (0, 0.)
        };
        h.set_field(tree, i * 3, Value::int(epoch))?;
        h.set_field(tree, i * 3 + 1, Value::int(measured + measured_delta))?;
        h.set_field(tree, i * 3 + 2, Value::float(height + height_delta))?;
        i += i & i.wrapping_neg();
    }
    Ok(())
}
pub(super) fn prefix(h: &Heap, s: Value, n: u32) -> Result<f64, AdapterError> {
    let (measured, heights) = sum(h, get(h, s, S::Tree)?, n, int(h, s, S::Epoch)?)?;
    Ok(heights
        + (i64::from(n) - measured) as f64 * num(h, s, S::Estimate)?
        + f64::from(n) * gap(h, s)?)
}
pub(super) fn total(h: &Heap, s: Value) -> Result<f64, AdapterError> {
    let n = count(h, s)?;
    Ok(if n == 0 {
        0.
    } else {
        prefix(h, s, n)? - gap(h, s)?
    })
}
pub(super) fn locate(h: &Heap, s: Value, offset: f64) -> Result<u32, AdapterError> {
    let n = count(h, s)?;
    if n == 0 {
        return Ok(0);
    }
    let tree = get(h, s, S::Tree)?;
    let epoch = int(h, s, S::Epoch)?;
    let base = num(h, s, S::Estimate)? + gap(h, s)?;
    let mut step = 1u32 << (31 - n.leading_zeros());
    let (mut i, mut correction) = (0, 0.);
    while step != 0 {
        let next = i + step;
        if next <= n {
            let part = if h.sequence_get(tree, next * 3)?.as_int() == Some(epoch) {
                stage::number(h.sequence_get(tree, next * 3 + 2)?)?
                    - h.sequence_get(tree, next * 3 + 1)?.as_int().unwrap() as f64
                        * num(h, s, S::Estimate)?
            } else {
                0.
            };
            if f64::from(next) * base + correction + part <= offset {
                i = next;
                correction += part;
            }
        }
        step >>= 1;
    }
    Ok(i.min(n - 1))
}

// Called after a complete measurement batch or source-index replacement, never
// per item: an existing sample changing height replaces its prior contribution.
pub(super) fn refresh_estimate(h: &mut Heap, s: Value) -> Result<bool, AdapterError> {
    let (measured, heights) = sum(h, get(h, s, S::Tree)?, count(h, s)?, int(h, s, S::Epoch)?)?;
    if measured == 0 {
        return Ok(false);
    }
    let estimate = heights / measured as f64;
    if estimate == num(h, s, S::Estimate)? {
        return Ok(false);
    }
    put(h, s, S::Estimate, Value::float(estimate))?;
    Ok(true)
}
