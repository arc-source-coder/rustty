use std::sync::atomic::{AtomicU64, Ordering};

fn next_tab_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn next_profile_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TabId(u64);

impl TabId {
    pub fn new() -> Self {
        Self(next_tab_id())
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ProfileId(u64);

impl ProfileId {
    pub fn new() -> Self {
        Self(next_profile_id())
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_id_uniqueness() {
        let a = TabId::new();
        let b = TabId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn profile_id_uniqueness() {
        let a = ProfileId::new();
        let b = ProfileId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn tab_id_as_u64() {
        let id = TabId::new();
        assert!(id.as_u64() > 0);
    }
}
