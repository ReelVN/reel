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
// A Fenwick tree of measured-height corrections. Epoch tags invalidate width
// dependent measurements in O(1), without visiting unmounted author data.
pub(super) fn sum(h: &Heap, tree: Value, mut n: u32, epoch: i64) -> Result<f64, AdapterError> {
    let mut value = 0.;
    while n > 0 {
        if h.sequence_get(tree, n * 2)?.as_int() == Some(epoch) {
            value += stage::number(h.sequence_get(tree, n * 2 + 1)?)?;
        }
        n &= n - 1;
    }
    Ok(value)
}
pub(super) fn add(
    h: &mut Heap,
    tree: Value,
    count: u32,
    mut i: u32,
    epoch: i64,
    delta: f64,
) -> Result<(), AdapterError> {
    i += 1;
    while i <= count {
        let old = if h.sequence_get(tree, i * 2)?.as_int() == Some(epoch) {
            stage::number(h.sequence_get(tree, i * 2 + 1)?)?
        } else {
            0.
        };
        h.set_field(tree, i * 2, Value::int(epoch))?;
        h.set_field(tree, i * 2 + 1, Value::float(old + delta))?;
        i += i & i.wrapping_neg();
    }
    Ok(())
}
pub(super) fn prefix(h: &Heap, s: Value, n: u32) -> Result<f64, AdapterError> {
    Ok(f64::from(n) * (num(h, s, S::Estimate)? + gap(h, s)?)
        + sum(h, get(h, s, S::Tree)?, n, int(h, s, S::Epoch)?)?)
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
            let part = if h.sequence_get(tree, next * 2)?.as_int() == Some(epoch) {
                stage::number(h.sequence_get(tree, next * 2 + 1)?)?
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
