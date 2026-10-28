//! One bounded weak cache for script and native method bindings. Four-way sets
//! share a fixed total slot count; heap validation enforces generation and GC cutoffs.
use super::*;

const SLOTS: usize = 256;
const WAYS: usize = 4;
const SETS: usize = SLOTS / WAYS;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct MethodKey([u64; 3]);
impl MethodKey {
    pub(super) fn native(receiver: u64, function: FunctionId) -> Self {
        Self([0, receiver, u64::from(function.0)])
    }
    pub(super) fn script(closure: u64, receiver: u64, owner: u64) -> Self {
        // A closure's full object key is nonzero, separating script and native keys.
        Self([closure, receiver, owner])
    }
}

#[derive(Clone, Copy)]
struct Binding {
    key: MethodKey,
    callable: u64,
}

pub(super) struct MethodBindings {
    entries: [Binding; SLOTS],
    next: [u8; SETS],
}

impl Default for MethodBindings {
    fn default() -> Self {
        Self {
            entries: [Binding {
                key: MethodKey([0; 3]),
                callable: 0,
            }; SLOTS],
            next: [0; SETS],
        }
    }
}

impl MethodBindings {
    fn slot(key: MethodKey) -> usize {
        let hash = key.0[1].wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ key.0[2].wrapping_mul(0x85eb_ca6b)
            ^ key.0[0].wrapping_mul(0xc2b2_ae35_27d4_eb4f);
        ((hash ^ (hash >> 32)) as usize) & (SLOTS - 1)
    }

    pub(super) fn get(&self, key: MethodKey) -> Option<u64> {
        let slot = Self::slot(key);
        let direct = self.entries[slot];
        if direct.key == key {
            return Some(direct.callable);
        }
        for offset in 1..WAYS {
            let entry = self.entries[slot ^ offset];
            if entry.key == key {
                return Some(entry.callable);
            }
        }
        None
    }

    pub(super) fn remember(&mut self, key: MethodKey, callable: u64) {
        let slot = Self::slot(key);
        let binding = Binding { key, callable };
        if self.entries[slot].callable == 0 || self.entries[slot].key == key {
            self.entries[slot] = binding;
            return;
        }
        let set = slot / WAYS;
        let entries = &mut self.entries[set * WAYS..(set + 1) * WAYS];
        // Refresh an expired weak handle in place, otherwise fill a vacant way
        // before bounded FIFO eviction. Neither keys nor results own GC edges.
        let index = entries
            .iter()
            .position(|entry| entry.key == key)
            .or_else(|| entries.iter().position(|entry| entry.callable == 0))
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
const _: () = assert!(std::mem::size_of::<MethodBindings>() <= 8_256);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_and_script_keys_share_the_same_bounded_ways_and_keep_full_identities() {
        let native = MethodKey::native(42, FunctionId(7));
        let slot = MethodBindings::slot(native);
        let scripts: Vec<_> = (1..10000)
            .map(|closure| MethodKey::script(closure, 42, 7))
            .filter(|key| MethodBindings::slot(*key) == slot)
            .take(4)
            .collect();
        let mut cache = MethodBindings::default();
        cache.remember(native, 101);
        for (index, &key) in scripts[..3].iter().enumerate() {
            cache.remember(key, 201 + index as u64);
        }
        assert_eq!(cache.get(native), Some(101));
        for (index, &key) in scripts[..3].iter().enumerate() {
            assert_eq!(cache.get(key), Some(201 + index as u64));
            for part in 0..3 {
                let mut changed = key;
                changed.0[part] ^= 1 << 32;
                assert_eq!(cache.get(changed), None);
            }
        }
        cache.remember(scripts[3], 204);
        let retained = std::iter::once(native)
            .chain(scripts)
            .filter(|key| cache.get(*key).is_some())
            .count();
        assert_eq!(retained, WAYS);
    }

    #[test]
    fn colliding_full_identity_keys_retain_four_ways_and_refresh_without_eviction() {
        let function = FunctionId(3);
        let keys: Vec<_> = (1..10000)
            .filter(|key| MethodBindings::slot(MethodKey::native(*key, function)) == 0)
            .take(5)
            .collect();
        let mut cache = MethodBindings::default();
        for &key in &keys[..4] {
            cache.remember(MethodKey::native(key, function), key + 1);
        }
        for &key in &keys[..4] {
            assert_eq!(cache.get(MethodKey::native(key, function)), Some(key + 1));
        }
        cache.remember(MethodKey::native(keys[2], function), 90001);
        assert_eq!(cache.get(MethodKey::native(keys[2], function)), Some(90001));
        cache.remember(MethodKey::native(keys[4], function), keys[4] + 1);
        assert_eq!(cache.get(MethodKey::native(keys[0], function)), None);
        assert_eq!(
            cache.get(MethodKey::native(keys[1], function)),
            Some(keys[1] + 1)
        );
        assert_eq!(cache.get(MethodKey::native(keys[2], function)), Some(90001));
        assert_eq!(
            cache.get(MethodKey::native(keys[3], function)),
            Some(keys[3] + 1)
        );
        assert_eq!(
            cache.get(MethodKey::native(keys[4], function)),
            Some(keys[4] + 1)
        );
        assert_eq!(cache.get(MethodKey::native(keys[1], FunctionId(4))), None);
        assert_eq!(
            cache.get(MethodKey::native(keys[1] | (1 << 32), function)),
            None
        );
    }
}
