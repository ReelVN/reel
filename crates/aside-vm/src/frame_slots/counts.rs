//! Integer-only diagnostics owned by the frame-slot storage layer.
use std::cell::Cell;

macro_rules! slot_fields {
    ($( $counter:ident => $field:ident: $doc:literal; )*) => {
        #[derive(Clone, Copy)]
        pub(crate) enum SlotCounter {
            $($counter,)*
        }
        const SLOT_FIELDS: usize = [$(stringify!($counter)),*].len();
        /// Main frame-slot API calls. Read/write/clear/borrow/move/copy counts
        /// include attempts; open/release counters describe successful operations.
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
        pub struct SlotCounts {
            $(#[doc = $doc] pub $field: u64,)*
        }
        impl SlotCounts {
            pub(crate) fn checked_delta(&self, before: &Self) -> Option<Self> {
                Some(Self {
                    $($field: self.$field.checked_sub(before.$field)?,)*
                })
            }
        }
        pub(crate) struct SlotState([Cell<u64>; SLOT_FIELDS]);
        impl Default for SlotState {
            fn default() -> Self {
                Self(std::array::from_fn(|_| Cell::new(0)))
            }
        }
        impl SlotState {
            pub(crate) fn add(&self, counter: SlotCounter, n: u64) {
                let cell = &self.0[counter as usize];
                cell.set(cell.get() + n);
            }
            pub(crate) fn snapshot(&self) -> SlotCounts {
                SlotCounts {
                    $($field: self.0[SlotCounter::$counter as usize].get(),)*
                }
            }
        }
    }
}
slot_fields! {
    Reads=>read_calls:"Single indexed read API attempts.";
    PairReads=>pair_read_calls:"Paired read API attempts, using one mapping in the ordinary path.";
    Writes=>write_calls:"Publishing write API attempts; bulk null initialization is counted by open_values.";
    ClearCalls=>clear_calls:"Single or packed clear API attempts.";
    ClearValues=>clear_values:"Requested single/packed clear operands; includes invalid later operands.";
    Borrows=>borrow_calls:"Initialized-window borrow attempts.";
    BorrowValues=>borrow_values:"Requested values in initialized-window borrows.";
    Opens=>open_calls:"Successfully opened windows, including empty windows.";
    OpenValues=>open_values:"Slots null-initialized by successful window opens.";
    EmptyOpens=>empty_opens:"Successfully opened zero-length windows.";
    Releases=>release_calls:"Successfully unregistered windows.";
    ReleaseValues=>release_values:"Slots removed from root registration; release does not overwrite them.";
    Moves=>move_calls:"move_value API attempts, whose writes bypass write/clear entry counts.";
    Copies=>copy_calls:"copy_value API attempts, which also use the read/write entries.";
}
