use super::AdapterError;
use aside_vm::{
    Heap, HeapError, ObjectKind, RootKind, RootOwner, RootToken, TableLookupStatus,
    TableStringLookup, Value,
};

struct Row {
    table: Value,
    version: u64,
    lookup: TableStringLookup<'static>,
    key: Option<Value>,
}

/// One native entry remains issued during decoding. Independent roots also
/// protect every cursor target and each completed key/label between turns.
/// No raw table layout, hooks or nested VM execution appear in this reader.
pub(super) struct ChoiceReader {
    array: Value,
    version: u64,
    count: u32,
    owner: RootOwner,
    roots: Vec<RootToken>,
    tables: Vec<(Value, u64)>,
    pub rows: Vec<(Value, Value)>,
    current: Option<Row>,
}

impl ChoiceReader {
    pub fn new(
        heap: &mut Heap,
        array: Value,
        owner: RootOwner,
        max_rows: usize,
    ) -> Result<Self, AdapterError> {
        let info = heap.info(array)?;
        if info.kind != ObjectKind::Array {
            return Err(AdapterError::Input("choose requires an array"));
        }
        let count = heap.sequence_len(array)?;
        if count == 0 || count as usize > max_rows.min(2) {
            return Err(AdapterError::Input("player requires one or two choices"));
        }
        let mut roots = Vec::new();
        roots
            .try_reserve_exact(1 + count as usize * 3)
            .map_err(|_| AdapterError::Allocation)?;
        let mut rows = Vec::new();
        rows.try_reserve_exact(count as usize)
            .map_err(|_| AdapterError::Allocation)?;
        let mut tables = Vec::new();
        tables
            .try_reserve_exact(count as usize)
            .map_err(|_| AdapterError::Allocation)?;
        roots.push(heap.add_root(owner, RootKind::Host, array)?);
        Ok(Self {
            array,
            version: info.content_version,
            count,
            owner,
            roots,
            tables,
            rows,
            current: None,
        })
    }

    /// A sequence slot costs one unit, and the shared lookup reports each slot,
    /// key byte and completion unit. Per-unit root bookkeeping is constant.
    /// String payload copies are separately bounded by Session's text limit.
    pub fn step(&mut self, heap: &mut Heap, budget: u32) -> Result<(bool, u32), AdapterError> {
        if heap.info(self.array)?.content_version != self.version {
            return Err(HeapError::ConcurrentModification.into());
        }
        for &(table, version) in &self.tables {
            if heap.info(table)?.content_version != version {
                return Err(HeapError::ConcurrentModification.into());
            }
        }
        let mut work = 0;
        while work < budget && self.rows.len() < self.count as usize {
            if self.current.is_none() {
                work += 1;
                let table = heap.sequence_get(self.array, self.rows.len() as u32)?;
                let lookup = heap.table_own_string_lookup(table, "key")?;
                let version = heap.info(table)?.content_version;
                self.roots
                    .push(heap.add_root(self.owner, RootKind::Host, table)?);
                self.tables.push((table, version));
                self.current = Some(Row {
                    table,
                    version,
                    lookup,
                    key: None,
                });
                continue;
            }
            let row = self.current.as_mut().unwrap();
            if heap.info(row.table)?.content_version != row.version {
                return Err(HeapError::ConcurrentModification.into());
            }
            let progress = heap.table_own_string_step(&mut row.lookup, budget - work)?;
            work += progress.work;
            match progress.status {
                TableLookupStatus::Pending => break,
                TableLookupStatus::Complete(None) => {
                    return Err(AdapterError::Input("choice row needs own key and label"));
                }
                TableLookupStatus::Complete(Some(value)) => {
                    if heap.info(value)?.kind != ObjectKind::String {
                        return Err(AdapterError::Input("choice key and label must be strings"));
                    }
                    self.roots
                        .push(heap.add_root(self.owner, RootKind::Host, value)?);
                    if let Some(key) = row.key {
                        self.rows.push((key, value));
                        self.current = None;
                    } else {
                        row.key = Some(value);
                        row.lookup = heap.table_own_string_lookup(row.table, "label")?;
                    }
                }
            }
        }
        Ok((self.rows.len() == self.count as usize, work))
    }

    pub fn release(self, heap: &mut Heap) -> Result<(), AdapterError> {
        for root in self.roots {
            heap.release_root(root)?;
        }
        Ok(())
    }
}
