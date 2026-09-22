//! `common/ll.c`'s link structure, modelled just far enough to stay out of
//! divergence #20.
//!
//! `ll_remove` reads `current->previous` when it unlinks a node that is the
//! tail but not the head, and never fixes up the `previous` of a node whose
//! predecessor it just freed. So the tail's `previous` can dangle, `ll_remove`
//! stores it into `LL::tail`, and the next `ll_item_add` writes through it.
//!
//! Every list in bm_core is exposed, so this is shared rather than owned by
//! one comparator. [`crate::registry`] drives `PACKET.sequence_list` and
//! [`crate::info`] drives `INFO_REQUEST_LIST`; both use [`LinkModel`] to
//! decline the append that would be undefined. **A domain restriction, never a
//! relaxed assertion**: every step a comparator does perform is compared in
//! full.

/// A model of one `LL`'s link structure.
///
/// Only the `previous` pointers matter, and only for the tail. Removals are
/// always defined — `ll_remove` and `ll_get_item` both walk from the head —
/// so [`Self::add_is_undefined`] is the only question to ask.
#[derive(Debug, Default)]
pub struct LinkModel {
    /// `(node id, id of the node that was the tail when this one was added)`,
    /// in list order.
    nodes: Vec<(u64, Option<u64>)>,
    next_id: u64,
    /// `LL::tail` points at a freed node.
    tail_dangling: bool,
}

impl LinkModel {
    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many nodes the list holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Whether an `ll_item_add` right now would write through a dangling
    /// `LL::tail`. The list emptying through the head branch clears the
    /// hazard: `ll_item_add` looks at `LL::head`, and takes the branch that
    /// rebuilds both pointers when it is null.
    #[must_use]
    pub fn add_is_undefined(&self) -> bool {
        self.tail_dangling && !self.nodes.is_empty()
    }

    /// `ll_item_add`.
    ///
    /// # Panics
    ///
    /// If [`Self::add_is_undefined`], which the caller must check first.
    pub fn add(&mut self) {
        assert!(
            !self.add_is_undefined(),
            "the comparator must not append through a dangling tail"
        );
        let previous = self.nodes.last().map(|(id, _)| *id);
        self.nodes.push((self.next_id, previous));
        self.next_id += 1;
    }

    /// `ll_remove`, for the node at `index`.
    ///
    /// # Panics
    ///
    /// If `index` is past the end.
    pub fn remove(&mut self, index: usize) {
        let is_tail = index + 1 == self.nodes.len();
        if is_tail && self.nodes.len() >= 2 {
            let previous = self.nodes[index].1;
            let alive = previous.is_some_and(|id| self.nodes.iter().any(|(n, _)| *n == id));
            if !alive {
                self.tail_dangling = true;
            }
        }
        self.nodes.remove(index);
        if self.nodes.is_empty() {
            self.tail_dangling = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four operations divergence #20 describes, in order.
    #[test]
    fn removing_the_middle_then_the_tail_dangles_it() {
        let mut list = LinkModel::new();
        for _ in 0..3 {
            list.add();
        }
        assert!(!list.add_is_undefined());

        list.remove(1); // the middle; the new tail's `previous` now dangles
        assert!(!list.add_is_undefined(), "nothing has read it yet");

        list.remove(1); // the tail, which stores the freed pointer into LL::tail
        assert!(list.add_is_undefined());
    }

    #[test]
    fn emptying_the_list_clears_the_hazard() {
        let mut list = LinkModel::new();
        for _ in 0..3 {
            list.add();
        }
        list.remove(1);
        list.remove(1);
        assert!(list.add_is_undefined());

        list.remove(0);
        assert!(list.is_empty());
        assert!(
            !list.add_is_undefined(),
            "ll_item_add rebuilds both pointers when LL::head is null"
        );
    }

    /// Removing the tail of a list whose predecessor is still alive is fine,
    /// which is what makes ordinary first-in-first-out use safe.
    #[test]
    fn removing_in_list_order_never_dangles() {
        let mut list = LinkModel::new();
        for _ in 0..4 {
            list.add();
        }
        while !list.is_empty() {
            list.remove(0);
            assert!(!list.add_is_undefined());
        }
    }

    #[test]
    fn removing_the_tail_repeatedly_never_dangles() {
        let mut list = LinkModel::new();
        for _ in 0..4 {
            list.add();
        }
        while !list.is_empty() {
            list.remove(list.len() - 1);
            assert!(!list.add_is_undefined());
        }
    }
}
