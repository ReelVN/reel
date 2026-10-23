//! Persistent action prefixes shared by running branches and terminal reports.

use std::{fmt, ops::Index, sync::Arc};

use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};

use super::ActionSelection;

/// An immutable-prefix action sequence that serializes as an ordinary array.
/// Cloning a trace shares its prefix; appending preserves existing snapshots.
#[derive(Clone, Default)]
pub struct ActionTrace {
    tail: Option<Arc<Link>>,
    len: usize,
}

struct Link {
    previous: Option<Arc<Link>>,
    action: ActionSelection,
}

impl Drop for Link {
    fn drop(&mut self) {
        let mut previous = self.previous.take();
        while let Some(link) = previous {
            match Arc::try_unwrap(link) {
                Ok(mut link) => previous = link.previous.take(),
                Err(_) => break,
            }
        }
    }
}

impl ActionTrace {
    /// Number of actions in this prefix.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether this prefix contains no actions.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends one action while sharing all earlier actions with existing clones.
    pub fn push(&mut self, action: ActionSelection) {
        self.tail = Some(Arc::new(Link {
            previous: self.tail.take(),
            action,
        }));
        self.len += 1;
    }

    /// Last submitted action, without traversing the prefix.
    pub fn last(&self) -> Option<&ActionSelection> {
        self.tail.as_ref().map(|link| &link.action)
    }

    /// Resolves one action by walking backward from the tail.
    pub fn get(&self, index: usize) -> Option<&ActionSelection> {
        let distance = self.len.checked_sub(index.checked_add(1)?)?;
        let mut link = self.tail.as_deref()?;
        for _ in 0..distance {
            link = link.previous.as_deref()?;
        }
        Some(&link.action)
    }

    /// Iterates in submission order using temporary references to shared links.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &ActionSelection> + DoubleEndedIterator {
        let mut actions = Vec::with_capacity(self.len);
        let mut link = self.tail.as_deref();
        while let Some(current) = link {
            actions.push(&current.action);
            link = current.previous.as_deref();
        }
        actions.into_iter().rev()
    }
}

impl FromIterator<ActionSelection> for ActionTrace {
    fn from_iter<T: IntoIterator<Item = ActionSelection>>(actions: T) -> Self {
        let mut trace = Self::default();
        for action in actions {
            trace.push(action);
        }
        trace
    }
}

impl From<Vec<ActionSelection>> for ActionTrace {
    fn from(actions: Vec<ActionSelection>) -> Self {
        actions.into_iter().collect()
    }
}

impl Index<usize> for ActionTrace {
    type Output = ActionSelection;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("action trace index out of bounds")
    }
}

impl PartialEq for ActionTrace {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        if let (Some(left), Some(right)) = (&self.tail, &other.tail)
            && Arc::ptr_eq(left, right)
        {
            return true;
        }
        self.iter().eq(other.iter())
    }
}

impl Eq for ActionTrace {}

impl fmt::Debug for ActionTrace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.iter()).finish()
    }
}

impl Serialize for ActionTrace {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.len))?;
        for action in self.iter() {
            sequence.serialize_element(action)?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for ActionTrace {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<ActionSelection>::deserialize(deserializer).map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::ActionKind;

    #[test]
    fn branch_snapshots_share_prefixes_and_serialize_as_v1_arrays() {
        let keys: Arc<[String]> = vec!["left".into(), "right".into()].into();
        let action = ActionSelection {
            kind: ActionKind::Controls,
            option_keys: keys.clone(),
            input_state: Some("ab".repeat(32).into()),
            selected: 0,
        };
        let mut parent = ActionTrace::default();
        parent.push(action.clone());
        let mut branch = parent.clone();
        assert!(Arc::ptr_eq(
            parent.tail.as_ref().unwrap(),
            branch.tail.as_ref().unwrap()
        ));
        branch.push(ActionSelection {
            selected: 1,
            ..action
        });
        assert!(Arc::ptr_eq(
            parent.tail.as_ref().unwrap(),
            branch.tail.as_ref().unwrap().previous.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(&keys, &branch[1].option_keys));
        let encoded = serde_json::to_value(&branch).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!([
            {"kind":"controls", "option_keys":["left","right"], "input_state":"ab".repeat(32), "selected":0},
            {"kind":"controls", "option_keys":["left","right"], "input_state":"ab".repeat(32), "selected":1}
            ])
        );
        let restored: ActionTrace = serde_json::from_value(encoded).unwrap();
        assert_eq!(branch, restored);
        assert_eq!(parent.len(), 1);
        assert_eq!(restored.last().unwrap().selected, 1);
    }

    #[test]
    fn dropping_a_long_unshared_prefix_uses_constant_stack() {
        let mut trace = ActionTrace::default();
        let keys: Arc<[String]> = vec!["continue".into()].into();
        for _ in 0..100_000 {
            trace.push(ActionSelection {
                kind: ActionKind::Controls,
                option_keys: keys.clone(),
                input_state: Some("ab".repeat(32).into()),
                selected: 0,
            });
        }
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || drop(trace))
            .unwrap()
            .join()
            .unwrap();
    }
}
