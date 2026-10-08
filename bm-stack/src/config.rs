//! Loading and saving the config store: the two functions of
//! `bcmp/configuration.c` that touch storage.
//!
//! Everything else in that file is [`bm_wire::configuration`], which holds the
//! partitions and knows nothing of flash.
//!
//! [`Configuration`] is how a [`crate::Node`] reaches a store to answer the
//! config messages `0xA0`–`0xA9`: [`Config`] is a store with its storage,
//! [`NoConfig`] is none.

use bm_wire::configuration::{CONFIG_LOAD_TIMEOUT_MS, ConfigStore, Layout, Partition};

use crate::port::ConfigStorage;

/// `config_init`: load every partition from `storage`, in partition order.
///
/// A partition that fails to read or verify comes up empty, keeping whatever
/// bytes the read left behind; see
/// [`bm_wire::configuration::ConfigPartition::load_with`]. Returns which
/// partitions loaded.
pub fn config_init<S: ConfigStorage>(store: &mut ConfigStore, storage: &mut S) -> [bool; 3] {
    Partition::ALL.map(|p| {
        store
            .partition_mut(p)
            .load_with(|buf| storage.read(p, 0, buf, CONFIG_LOAD_TIMEOUT_MS))
    })
}

/// `save_config`: seal `partition` with its CRC and write it. On success,
/// [`ConfigStorage::reset`] if `restart`, then clear `needs_commit`.
///
/// The CRC is written into the RAM header whether or not the write succeeds.
pub fn save_config<S: ConfigStorage>(
    store: &mut ConfigStore,
    partition: Partition,
    storage: &mut S,
    restart: bool,
) -> bool {
    let part = store.partition_mut(partition);
    if !storage.write(partition, 0, part.seal(), CONFIG_LOAD_TIMEOUT_MS) {
        return false;
    }
    if restart {
        storage.reset();
    }
    part.mark_saved();
    true
}

/// The store a node answers config messages from, and the save a
/// `ConfigCommit` asks for.
pub trait Configuration {
    /// `CONFIGS`, or `None` for a node that keeps no configuration.
    fn store(&self) -> Option<&ConfigStore>;

    /// The same, mutably.
    fn store_mut(&mut self) -> Option<&mut ConfigStore>;

    /// `save_config(partition, true)`, which is what `bcmp/config.c` calls for
    /// a `ConfigCommit`: seal, write, then [`ConfigStorage::reset`]. Returns
    /// whether the write succeeded.
    fn commit(&mut self, partition: Partition) -> bool;
}

/// No configuration. A node built with this forwards config messages for
/// other nodes and answers none addressed to it; a C node always has a store.
#[derive(Debug, Clone, Copy, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NoConfig;

impl Configuration for NoConfig {
    fn store(&self) -> Option<&ConfigStore> {
        None
    }

    fn store_mut(&mut self) -> Option<&mut ConfigStore> {
        None
    }

    fn commit(&mut self, _partition: Partition) -> bool {
        false
    }
}

/// A config store and the storage it loads from and saves to.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Config<S> {
    /// `CONFIGS`.
    pub store: ConfigStore,
    /// `bm_config_*`.
    pub storage: S,
}

impl<S: ConfigStorage> Config<S> {
    /// [`config_init`] from `storage` into a store of `layout`.
    pub fn load(layout: Layout, mut storage: S) -> Self {
        let mut store = ConfigStore::new(layout);
        config_init(&mut store, &mut storage);
        Self { store, storage }
    }
}

impl<S: ConfigStorage> Configuration for Config<S> {
    fn store(&self) -> Option<&ConfigStore> {
        Some(&self.store)
    }

    fn store_mut(&mut self) -> Option<&mut ConfigStore> {
        Some(&mut self.store)
    }

    fn commit(&mut self, partition: Partition) -> bool {
        save_config(&mut self.store, partition, &mut self.storage, true)
    }
}

impl<T: Configuration + ?Sized> Configuration for &mut T {
    fn store(&self) -> Option<&ConfigStore> {
        (**self).store()
    }

    fn store_mut(&mut self) -> Option<&mut ConfigStore> {
        (**self).store_mut()
    }

    fn commit(&mut self, partition: Partition) -> bool {
        (**self).commit(partition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::RamConfigStorage;
    use bm_wire::configuration::{Key, Layout};

    /// A saved partition comes back from storage; a corrupted one comes back
    /// empty.
    #[test]
    fn a_saved_partition_survives_a_reload_and_a_corrupt_one_does_not() {
        let mut storage = RamConfigStorage::new();
        let mut store = ConfigStore::new(Layout::LP64);
        assert_eq!(config_init(&mut store, &mut storage), [false; 3]);

        let sys = store.partition_mut(Partition::System);
        assert!(sys.set_uint(Key::new(b"foo"), 42));
        assert!(sys.needs_commit());
        assert!(save_config(
            &mut store,
            Partition::System,
            &mut storage,
            false
        ));
        assert!(!store.partition(Partition::System).needs_commit());

        let mut fresh = ConfigStore::new(Layout::LP64);
        assert_eq!(config_init(&mut fresh, &mut storage), [false, true, false]);
        let sys = fresh.partition(Partition::System);
        assert_eq!(sys.get_uint(Key::new(b"foo")), Some(42));

        storage.bytes_mut(Partition::System)[100] ^= 1;
        let mut fresh = ConfigStore::new(Layout::LP64);
        assert_eq!(config_init(&mut fresh, &mut storage), [false; 3]);
        assert_eq!(fresh.partition(Partition::System).num_keys(), 0);
    }
}
