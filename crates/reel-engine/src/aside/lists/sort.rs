//! Bounded, snapshot-owned merge index. No Rust allocation outlives a dispatch.
use super::*;

const MAX_WORK: u32 = 256;
#[repr(u32)]
enum Field {
    Input,
    A,
    B,
    Width,
    Base,
    Left,
    Right,
    Write,
    Offset,
    Choice,
    Phase,
    Scan,
    Direction,
}
struct Cursor {
    input: Value,
    a: Value,
    b: Value,
    width: u32,
    base: u32,
    left: u32,
    right: u32,
    write: u32,
    offset: u32,
    choice: u32,
    phase: u32,
    scan: u32,
    direction: u32,
}
impl Cursor {
    fn load(heap: &Heap, state: Value) -> Result<Self, HeapError> {
        let segment = heap.array_segment(state, 0)?;
        let value = |field| {
            let index = field as u32;
            segment
                .get(index as usize)
                .copied()
                .map_or_else(|| heap.sequence_get(state, index), Ok)
        };
        let number = |field| Ok::<_, HeapError>(value(field)?.as_int().unwrap() as u32);
        Ok(Self {
            input: value(Field::Input)?,
            a: value(Field::A)?,
            b: value(Field::B)?,
            width: number(Field::Width)?,
            base: number(Field::Base)?,
            left: number(Field::Left)?,
            right: number(Field::Right)?,
            write: number(Field::Write)?,
            offset: number(Field::Offset)?,
            choice: number(Field::Choice)?,
            phase: number(Field::Phase)?,
            scan: number(Field::Scan)?,
            direction: number(Field::Direction)?,
        })
    }
    fn save(&self, heap: &mut Heap, state: Value) -> Result<(), HeapError> {
        let values = [
            self.input,
            self.a,
            self.b,
            Value::int(i64::from(self.width)),
            Value::int(i64::from(self.base)),
            Value::int(i64::from(self.left)),
            Value::int(i64::from(self.right)),
            Value::int(i64::from(self.write)),
            Value::int(i64::from(self.offset)),
            Value::int(i64::from(self.choice)),
            Value::int(i64::from(self.phase)),
            Value::int(i64::from(self.scan)),
            Value::int(i64::from(self.direction)),
        ];
        let mut written = 0;
        while written < values.len() {
            written += heap.array_write(state, written as u32, &values[written..])?;
        }
        Ok(())
    }
    fn destination(&self) -> Value {
        if self.input.same_bits(self.a) {
            self.b
        } else {
            self.a
        }
    }
}
struct Head<'a> {
    base: u32,
    segment: &'a [Value],
    position: Option<u32>,
    value: Value,
    key: Option<Value>,
}
impl Head<'_> {
    fn empty() -> Self {
        Self {
            base: 0,
            segment: &[],
            position: None,
            value: Value::NULL,
            key: None,
        }
    }
}
struct Source<'a> {
    heap: &'a Heap,
    array: Value,
    left: Head<'a>,
    right: Head<'a>,
}
impl<'a> Source<'a> {
    fn new(heap: &'a Heap, array: Value) -> Self {
        Self {
            heap,
            array,
            left: Head::empty(),
            right: Head::empty(),
        }
    }
    fn head(&mut self, index: u32, left: bool) -> Result<&mut Head<'a>, HeapError> {
        let head = if left {
            &mut self.left
        } else {
            &mut self.right
        };
        if head.position != Some(index) {
            if index < head.base || index - head.base >= head.segment.len() as u32 {
                head.segment = self.heap.array_segment(self.array, index)?;
                head.base = index;
            }
            head.value = *head
                .segment
                .get((index - head.base) as usize)
                .ok_or(HeapError::OutOfBounds)?;
            head.position = Some(index);
            head.key = None;
        }
        Ok(head)
    }
    fn value(&mut self, index: u32, left: bool) -> Result<Value, HeapError> {
        Ok(self.head(index, left)?.value)
    }
    fn key(&mut self, index: u32, left: bool) -> Result<Value, HeapError> {
        let heap = self.heap;
        let head = self.head(index, left)?;
        if let Some(key) = head.key {
            return Ok(key);
        }
        let key = entry(heap, head.value, Entry::Key)?;
        head.key = Some(key);
        Ok(key)
    }
    fn next_scan(&mut self) {
        std::mem::swap(&mut self.left, &mut self.right);
        self.right.position = None;
    }
}
const BATCH: usize = 64;
struct Output {
    array: Value,
    start: u32,
    len: usize,
    values: [Value; BATCH],
}
impl Output {
    fn new() -> Self {
        Self {
            array: Value::NULL,
            start: 0,
            len: 0,
            values: [Value::NULL; BATCH],
        }
    }
    fn push(&mut self, array: Value, index: u32, value: Value) {
        if self.len == 0 {
            self.array = array;
            self.start = index;
        }
        debug_assert!(self.array.same_bits(array) && self.start + self.len as u32 == index);
        self.values[self.len] = value;
        self.len += 1;
    }
    fn full(&self) -> bool {
        self.len == BATCH
    }
    fn flush(&mut self, heap: &mut Heap) -> Result<(), HeapError> {
        let mut copied = 0;
        while copied < self.len {
            copied += heap.array_write(
                self.array,
                self.start + copied as u32,
                &self.values[copied..self.len],
            )?;
        }
        self.len = 0;
        Ok(())
    }
}
pub(in crate::aside) enum Step {
    More { state: Value, work: u32 },
    Ready { result: Value, work: u32 },
    Budget { required_work: u32 },
}
enum KeyStep {
    Ready(Ordering, u32),
    More(u32),
    Budget(u32),
}
fn key_step(
    heap: &Heap,
    a: Value,
    b: Value,
    offset: &mut u32,
    budget: u32,
    chunk: usize,
) -> Result<KeyStep, AdapterError> {
    if budget == 0 {
        return Ok(KeyStep::Budget(1));
    }
    // Keep exactly the shared scalar/type/object identity contract. Only a
    // distinct pair of immutable strings can require more than constant work.
    if a.same_bits(b)
        || a.tag() != ValueTag::Object
        || b.tag() != ValueTag::Object
        || heap.info(a)?.kind != ObjectKind::String
        || heap.info(b)?.kind != ObjectKind::String
    {
        return Ok(KeyStep::Ready(compare(heap, a, b, chunk)?, 1));
    }
    let (a_len, b_len) = (heap.info(a)?.len, heap.info(b)?.len);
    let remaining = a_len.min(b_len).saturating_sub(*offset);
    if remaining == 0 {
        return Ok(KeyStep::Ready(a_len.cmp(&b_len), 1));
    }
    // One comparison plus each copied byte from both strings is charged.
    if budget < 3 {
        return Ok(KeyStep::Budget(3));
    }
    let count = ((budget - 1) as usize / 2)
        .min(256)
        .min(chunk.max(1))
        .min(remaining as usize);
    let (mut left, mut right) = ([0; 256], [0; 256]);
    heap.read_bytes(a, *offset, &mut left[..count])?;
    heap.read_bytes(b, *offset, &mut right[..count])?;
    let work = 1 + 2 * count as u32;
    let order = left[..count].cmp(&right[..count]);
    if order != Ordering::Equal {
        return Ok(KeyStep::Ready(order, work));
    }
    *offset += count as u32;
    if *offset == a_len.min(b_len) {
        Ok(KeyStep::Ready(a_len.cmp(&b_len), work))
    } else {
        Ok(KeyStep::More(work))
    }
}
/// Every dispatch consumes at most min(the native work grant, 256) units.
/// Exact insufficient grants retain the original native ticket; RetryMinimum
/// admits only the first indivisible step. Both buffers and all cursors are VM
/// values, so GC, cancellation and snapshots use the existing native roots.
pub(in crate::aside) fn advance(
    heap: &mut Heap,
    entries: Value,
    mut state: Value,
    policy: NativeBudget,
    chunk: usize,
) -> Result<Step, AdapterError> {
    let mut limit = policy.with_minimum(1).min(MAX_WORK);
    if limit == 0 {
        return Ok(Step::Budget { required_work: 1 });
    }
    let count = heap.sequence_len(entries)?;
    if count < 2 {
        return Ok(Step::Ready {
            result: entries,
            work: 1,
        });
    }
    let mut used = 0;
    // Setup is sliced too: no input-sized allocation/copy precedes accounting.
    // Each control field (including its empty child buffer) is one bounded unit.
    if state.tag() == ValueTag::Null {
        state = heap.alloc_array(&[])?;
        used += 1;
    }
    while heap.sequence_len(state)? < 13 && used < limit {
        let value = match heap.sequence_len(state)? {
            0 => entries,
            1 | 2 => heap.alloc_array(&[])?,
            3 | 11 => Value::int(1),
            6 => Value::int(i64::from(count.min(1))),
            _ => Value::int(0),
        };
        heap.array_push(state, value)?;
        used += 1;
    }
    if heap.sequence_len(state)? < 13 || used == limit {
        return Ok(Step::More { state, work: used });
    }
    let mut cursor = Cursor::load(heap, state)?;
    let mut source = Source::new(heap, cursor.input);
    let mut output = Output::new();
    loop {
        if cursor.phase == 0 {
            // Preserve linear indexing for the common already ordered/reversed
            // source instead of paying all merge passes for every refresh.
            if cursor.scan == count {
                if cursor.direction == 1 {
                    return Ok(Step::Ready {
                        result: entries,
                        work: used.max(1),
                    });
                }
                cursor.phase = 2;
                cursor.write = 0;
                cursor.offset = 0;
                continue;
            }
            if used == limit {
                break;
            }
            let left = source.key(cursor.scan - 1, true)?;
            let right = source.key(cursor.scan, false)?;
            match key_step(
                source.heap,
                left,
                right,
                &mut cursor.offset,
                limit - used,
                chunk,
            )? {
                KeyStep::Budget(required) if used == 0 => {
                    let next = policy.with_minimum(required).min(MAX_WORK);
                    if next < required {
                        return Ok(Step::Budget {
                            required_work: required,
                        });
                    }
                    limit = next;
                }
                KeyStep::Budget(_) => break,
                KeyStep::More(work) => used += work,
                KeyStep::Ready(order, work) => {
                    used += work;
                    let direction = match order {
                        Ordering::Less => 1,
                        Ordering::Greater => 2,
                        Ordering::Equal => {
                            return Err(AdapterError::Input("list keys must be unique"));
                        }
                    };
                    if cursor.direction == 0 {
                        cursor.direction = direction;
                    }
                    if cursor.direction != direction {
                        cursor.phase = 1;
                    } else {
                        cursor.scan += 1;
                        source.next_scan();
                    }
                    cursor.offset = 0;
                }
            }
            continue;
        }
        if cursor.phase == 2 {
            if cursor.write == count {
                output.flush(heap)?;
                return Ok(Step::Ready {
                    result: cursor.a,
                    work: used.max(1),
                });
            }
            if used == limit {
                break;
            }
            output.push(
                cursor.a,
                cursor.write,
                source.value(count - cursor.write - 1, true)?,
            );
            cursor.write += 1;
            used += 1;
            if output.full() {
                output.flush(heap)?;
                source = Source::new(heap, cursor.input);
            }
            continue;
        }
        let middle = cursor.base.saturating_add(cursor.width).min(count);
        let end = cursor
            .base
            .saturating_add(cursor.width.saturating_mul(2))
            .min(count);
        if cursor.write == end {
            if end == count {
                output.flush(heap)?;
                cursor.input = cursor.destination();
                if cursor.width.saturating_mul(2) >= count {
                    return Ok(Step::Ready {
                        result: cursor.input,
                        work: used.max(1),
                    });
                }
                source = Source::new(heap, cursor.input);
                cursor.width *= 2;
                cursor.base = 0;
            } else {
                cursor.base = end;
            }
            cursor.left = cursor.base;
            cursor.right = cursor.base.saturating_add(cursor.width).min(count);
            cursor.write = cursor.base;
            cursor.choice = 0;
            cursor.offset = 0;
            continue;
        }
        if used == limit {
            break;
        }
        if cursor.choice == 0 {
            if cursor.left == middle {
                cursor.choice = 2;
            } else if cursor.right == end {
                cursor.choice = 1;
            } else {
                let left = source.key(cursor.left, true)?;
                let right = source.key(cursor.right, false)?;
                match key_step(
                    source.heap,
                    left,
                    right,
                    &mut cursor.offset,
                    limit - used,
                    chunk,
                )? {
                    KeyStep::Budget(required) if used == 0 => {
                        let next = policy.with_minimum(required).min(MAX_WORK);
                        if next < required {
                            return Ok(Step::Budget {
                                required_work: required,
                            });
                        }
                        limit = next;
                        continue;
                    }
                    KeyStep::Budget(_) => break,
                    KeyStep::More(work) => {
                        used += work;
                        continue;
                    }
                    KeyStep::Ready(order, work) => {
                        used += work;
                        // Each completed input run is unique. Equal keys from
                        // distinct runs meet at their first common merge.
                        cursor.choice = match order {
                            Ordering::Less => 1,
                            Ordering::Greater => 2,
                            Ordering::Equal => {
                                return Err(AdapterError::Input("list keys must be unique"));
                            }
                        };
                    }
                }
                if used == limit {
                    break;
                }
            }
        }
        let index = if cursor.choice == 1 {
            cursor.left
        } else {
            cursor.right
        };
        let value = source.value(index, cursor.choice == 1)?;
        output.push(cursor.destination(), cursor.write, value);
        if cursor.choice == 1 {
            cursor.left += 1;
        } else {
            cursor.right += 1;
        }
        cursor.write += 1;
        cursor.offset = 0;
        cursor.choice = 0;
        used += 1;
        if output.full() {
            output.flush(heap)?;
            source = Source::new(heap, cursor.input);
        }
    }
    output.flush(heap)?;
    cursor.save(heap, state)?;
    Ok(Step::More { state, work: used })
}

#[cfg(test)]
mod tests;
