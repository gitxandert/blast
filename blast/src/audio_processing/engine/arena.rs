//! Fixed-capacity storage for engine objects, indexed by ID (README §3.4).

/// Fixed-capacity slots indexed by ID. Built (and fully allocated) before the
/// audio thread starts; nothing here allocates or frees afterwards.
pub struct Arena<T> {
    slots: Vec<Option<T>>,
    // one past the highest occupied slot, so iteration costs the live range
    high: usize,
}

impl<T> Arena<T> {
    /// An empty arena with `cap` slots, all allocated now.
    pub fn with_capacity(cap: usize) -> Self {
        let mut slots = Vec::with_capacity(cap);
        slots.resize_with(cap, || None);
        Self { slots, high: 0 }
    }

    /// Puts `value` in slot `id`; hands it back if the slot is taken or out of range.
    pub fn insert(&mut self, id: usize, value: T) -> Result<(), T> {
        let Some(slot @ None) = self.slots.get_mut(id) else {
            return Err(value);
        };
        *slot = Some(value);
        self.high = self.high.max(id + 1);
        Ok(())
    }

    /// Takes the value out of slot `id`, if there is one.
    pub fn remove(&mut self, id: usize) -> Option<T> {
        let value = self.slots.get_mut(id)?.take()?;
        if id + 1 == self.high {
            while self.high > 0 && self.slots[self.high - 1].is_none() {
                self.high -= 1;
            }
        }
        Some(value)
    }

    /// The value in slot `id`.
    pub fn get(&self, id: usize) -> Option<&T> {
        self.slots.get(id)?.as_ref()
    }

    /// The value in slot `id`, mutably.
    pub fn get_mut(&mut self, id: usize) -> Option<&mut T> {
        self.slots.get_mut(id)?.as_mut()
    }

    /// Occupied slots in ID order, as (id, value). Stops at the highest occupied slot.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &T)> {
        self.slots[..self.high]
            .iter()
            .enumerate()
            .filter_map(|(id, slot)| slot.as_ref().map(|v| (id, v)))
    }

    /// Like `iter`, mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (usize, &mut T)> {
        self.slots[..self.high]
            .iter_mut()
            .enumerate()
            .filter_map(|(id, slot)| slot.as_mut().map(|v| (id, v)))
    }

    /// Removes every element for which `pred` is true, handing each to `sink`.
    pub fn remove_where(
        &mut self,
        mut pred: impl FnMut(usize, &T) -> bool,
        mut sink: impl FnMut(usize, T),
    ) {
        for (id, slot) in self.slots[..self.high].iter_mut().enumerate() {
            if slot.as_ref().is_some_and(|v| pred(id, v))
                && let Some(value) = slot.take()
            {
                sink(id, value);
            }
        }
        while self.high > 0 && self.slots[self.high - 1].is_none() {
            self.high -= 1;
        }
    }

    /// How many slots are occupied.
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Whether no slot is occupied.
    pub fn is_empty(&self) -> bool {
        self.high == 0
    }

    /// How many slots there are.
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }
}

/// Capacities of every arena and queue (one place, README §3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Live Voices.
    pub voices: u32,
    /// Live Groups.
    pub groups: u32,
    /// Live TempoStates.
    pub tempos: u32,
    /// Live Processes.
    pub procs: u32,
    /// Loaded tracks.
    pub tracks: u32,
    /// Voices in one Group.
    pub group_members: u32,
    /// Engine-to-REPL event queue slots.
    pub events: usize,
    /// Garbage queue slots.
    pub garbage: usize,
    /// Objects parked while the garbage queue is full.
    pub parked: usize,
    /// Lines waiting in the schedule (`at`, `after`).
    pub scheduled: u32,
    /// Timed scripts running at once (PKb): slots of the marks table.
    pub scripts: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            voices: 256,
            groups: 64,
            tempos: 1024,
            procs: 512,
            tracks: 1024,
            group_members: 256,
            events: 1024,
            garbage: 256,
            parked: 64,
            scheduled: 256,
            scripts: 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_insert_get_remove() {
        let mut a = Arena::with_capacity(8);
        assert!(a.insert(3, "x").is_ok());
        assert_eq!(a.get(3), Some(&"x"));
        assert_eq!(a.len(), 1);
        assert_eq!(a.remove(3), Some("x"));
        assert_eq!(a.get(3), None);
        assert!(a.is_empty());
    }

    #[test]
    fn arena_rejects_taken_and_out_of_range() {
        let mut a = Arena::with_capacity(4);
        assert!(a.insert(1, 10).is_ok());
        assert_eq!(a.insert(1, 11), Err(11));
        assert_eq!(a.insert(4, 12), Err(12));
        assert_eq!(a.get(1), Some(&10));
        assert_eq!(a.capacity(), 4);
    }

    #[test]
    fn arena_iter_in_id_order_and_skips_holes() {
        let mut a = Arena::with_capacity(8);
        for id in [6, 1, 4] {
            a.insert(id, id * 10).unwrap();
        }
        let got: Vec<(usize, usize)> = a.iter().map(|(id, v)| (id, *v)).collect();
        assert_eq!(got, vec![(1, 10), (4, 40), (6, 60)]);
        for (_, v) in a.iter_mut() {
            *v += 1;
        }
        assert_eq!(a.get(4), Some(&41));
    }

    #[test]
    fn arena_high_water_shrinks() {
        let mut a = Arena::with_capacity(8);
        a.insert(0, 'a').unwrap();
        a.insert(5, 'b').unwrap();
        a.remove(5);
        assert_eq!(a.high, 1);
        let ids: Vec<usize> = a.iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![0]);
        a.remove(0);
        assert_eq!(a.high, 0);
    }

    #[test]
    fn arena_remove_where() {
        let mut a = Arena::with_capacity(8);
        for id in [0, 2, 3, 5] {
            a.insert(id, id * 10).unwrap();
        }
        let mut removed = Vec::new();
        a.remove_where(|id, v| id == 5 || *v == 20, |id, v| removed.push((id, v)));
        assert_eq!(removed, vec![(2, 20), (5, 50)]);
        let left: Vec<usize> = a.iter().map(|(id, _)| id).collect();
        assert_eq!(left, vec![0, 3]);
        assert_eq!(a.high, 4);
    }

    #[test]
    fn arena_never_reallocates() {
        let mut a = Arena::with_capacity(16);
        let before = a.slots.as_ptr();
        for id in 0..16 {
            a.insert(id, id).unwrap();
        }
        for id in (0..16).step_by(2) {
            a.remove(id);
        }
        assert_eq!(a.slots.as_ptr(), before);
        assert_eq!(a.slots.capacity(), 16);
    }
}
