//! Bounded weak bindings of native methods. Four-way sets retain colliding live
//! receivers; ordinary heap validation enforces generation and GC cutoffs.
use super::*;

const SLOTS: usize = 256;
const WAYS: usize = 4;
const SETS: usize = SLOTS / WAYS;

#[derive(Clone, Copy)]
struct Binding {
    receiver: u64,
    callable: u64,
    function: u32,
}

pub(super) struct NativeBindings {
    entries: [Binding; SLOTS],
    next: [u8; SETS],
}

impl Default for NativeBindings {
    fn default() -> Self {
        Self {
            entries: [Binding {
                receiver: 0,
                callable: 0,
                function: u32::MAX,
            }; SLOTS],
            next: [0; SETS],
        }
    }
}

impl NativeBindings {
    fn slot(receiver: u64, function: FunctionId) -> usize {
        let hash = receiver.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ u64::from(function.0).wrapping_mul(0x85eb_ca6b);
        ((hash ^ (hash >> 32)) as usize) & (SLOTS - 1)
    }

    pub(super) fn get(&self, receiver: u64, function: FunctionId) -> Option<u64> {
        let slot = Self::slot(receiver, function);
        // Preserve the old single-probe fast path when the direct slot matches.
        let direct = self.entries[slot];
        if direct.receiver == receiver && direct.function == function.0 {
            return Some(direct.callable);
        }
        for offset in 1..WAYS {
            let entry = self.entries[slot ^ offset];
            if entry.receiver == receiver && entry.function == function.0 {
                return Some(entry.callable);
            }
        }
        None
    }

    pub(super) fn remember(&mut self, receiver: u64, function: FunctionId, callable: u64) {
        let slot = Self::slot(receiver, function);
        let binding = Binding {
            receiver,
            callable,
            function: function.0,
        };
        if self.entries[slot].function == u32::MAX
            || (self.entries[slot].receiver == receiver
                && self.entries[slot].function == function.0)
        {
            self.entries[slot] = binding;
            return;
        }
        let set = slot / WAYS;
        let entries = &mut self.entries[set * WAYS..(set + 1) * WAYS];
        // Refresh an expired weak handle in place, otherwise fill a vacant way
        // before bounded FIFO eviction. The entries carry no owning GC edges.
        let index = entries
            .iter()
            .position(|entry| entry.receiver == receiver && entry.function == function.0)
            .or_else(|| entries.iter().position(|entry| entry.function == u32::MAX))
            .unwrap_or_else(|| {
                let index = self.next[set] as usize;
                self.next[set] = ((index + 1) % WAYS) as u8;
                index
            });
        entries[index] = binding;
    }
}

// The fixed storage is included by Vm's control-size reservation. Snapshots
// rebuild it cold, and collection never traverses or clears these weak keys.
const _: () = assert!(std::mem::size_of::<NativeBindings>() <= 6_208);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colliding_full_identity_keys_retain_four_ways_and_refresh_without_eviction() {
        let function = FunctionId(3);
        let keys: Vec<_> = (1..10000)
            .filter(|key| NativeBindings::slot(*key, function) == 0)
            .take(5)
            .collect();
        let mut cache = NativeBindings::default();
        for &key in &keys[..4] {
            cache.remember(key, function, key + 1);
        }
        for &key in &keys[..4] {
            assert_eq!(cache.get(key, function), Some(key + 1));
        }
        cache.remember(keys[2], function, 90001);
        assert_eq!(cache.get(keys[2], function), Some(90001));
        cache.remember(keys[4], function, keys[4] + 1);
        assert_eq!(cache.get(keys[0], function), None);
        assert_eq!(cache.get(keys[1], function), Some(keys[1] + 1));
        assert_eq!(cache.get(keys[2], function), Some(90001));
        assert_eq!(cache.get(keys[3], function), Some(keys[3] + 1));
        assert_eq!(cache.get(keys[4], function), Some(keys[4] + 1));
        assert_eq!(cache.get(keys[1], FunctionId(4)), None);
        assert_eq!(cache.get(keys[1] | (1 << 32), function), None);
    }
}
