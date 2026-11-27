//! When the editor puts its result on the clipboard. Each finished change (a stroke, a shape,
//! undo, crop, a background) bumps the document's revision; a short quiet spell after the last
//! one, the image is rendered, encoded off the main thread and delivered. One delivery runs at a
//! time and a newer revision always follows an older one, so the clipboard never goes backwards
//! and a clipboard manager sees one entry per pause, not one per stroke.

/// Quiet time after the last change before the result goes to the clipboard.
pub const DEBOUNCE_MS: u64 = 300;

#[derive(Default, Debug)]
pub struct Applier {
    /// The revision the clipboard holds (0: the editor's starting image, never sent).
    applied: u64,
    /// The revision being rendered and delivered.
    in_flight: Option<u64>,
    /// A change arrived while one was in flight.
    again: bool,
}

impl Applier {
    /// Whether this revision still has to reach the clipboard.
    pub fn needs(&self, revision: u64) -> bool {
        revision != self.applied && self.in_flight != Some(revision)
    }

    /// Starts delivering a revision. False when another delivery is still running; it is
    /// remembered and started again once that one finishes.
    pub fn start(&mut self, revision: u64) -> bool {
        if self.in_flight.is_some() {
            self.again = true;
            return false;
        }
        self.in_flight = Some(revision);
        true
    }

    /// A delivery finished (or failed, with `delivered` false). Returns whether to run again.
    pub fn finish(&mut self, revision: u64, delivered: bool) -> bool {
        if self.in_flight == Some(revision) {
            self.in_flight = None;
        }
        if delivered {
            self.applied = revision;
        }
        std::mem::take(&mut self.again)
    }

    /// Marks a revision as on the clipboard already, after Copy or Done sent it.
    pub fn mark(&mut self, revision: u64) {
        self.applied = revision;
    }

    #[cfg(test)]
    pub fn applied(&self) -> u64 {
        self.applied
    }

    #[cfg(test)]
    pub fn busy(&self) -> bool {
        self.in_flight.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_send_until_a_change() {
        let a = Applier::default();
        assert!(!a.needs(0));
        assert!(a.needs(1));
    }

    #[test]
    fn one_delivery_at_a_time_and_reruns() {
        let mut a = Applier::default();
        assert!(a.start(1));
        assert!(!a.needs(1), "revision 1 is on its way");
        // Another change while encoding waits its turn.
        assert!(a.needs(2));
        assert!(!a.start(2));
        assert!(a.busy());
        assert!(a.finish(1, true), "the waiting change runs next");
        assert_eq!(a.applied(), 1);
        assert!(a.needs(2));
        assert!(a.start(2));
        assert!(!a.finish(2, true));
        assert!(!a.needs(2));
    }

    #[test]
    fn failed_delivery_is_retried_on_next_change() {
        let mut a = Applier::default();
        assert!(a.start(3));
        assert!(!a.finish(3, false));
        assert!(a.needs(3));
    }

    #[test]
    fn undo_back_to_a_sent_revision_needs_nothing_new_only_if_identical() {
        // Revisions only grow, so undo after a delivery is a new revision and is sent.
        let mut a = Applier::default();
        a.start(4);
        a.finish(4, true);
        assert!(a.needs(5));
        a.mark(5);
        assert!(!a.needs(5));
    }
}
