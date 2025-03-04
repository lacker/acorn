use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use crate::module::ModuleDescriptor;
use crate::module_cache::ModuleCache;

// The BuildCache contains a hash for each module from the last time it was cleanly built.
// This enables skipping verification for modules that haven't changed.
// We read once at startup, and write whole files at a time when needed.
// Hopefully that makes it okay to not lock it. But we might need to be better about this.
#[derive(Clone)]
pub struct BuildCache {
    // The internal map from module descriptor to module hash
    inner: Arc<DashMap<ModuleDescriptor, ModuleCache>>,

    // A directory to persist the cache in.
    directory: Option<PathBuf>,

    // Whether it's okay to write to the cache directory.
    // If false, the cache will not be saved to disk.
    writable: bool,
}

impl BuildCache {
    // Creates a new empty build cache
    pub fn new(directory: Option<PathBuf>, writable: bool) -> BuildCache {
        BuildCache {
            inner: Arc::new(DashMap::new()),
            directory,
            writable,
        }
    }

    // Gets the module cache for a module descriptor
    pub fn get(&self, descriptor: &ModuleDescriptor) -> Option<ModuleCache> {
        self.inner
            .get(descriptor)
            .map(|entry| entry.value().clone())
    }

    // Inserts a module cache for a module descriptor.
    // Saves the cache if it represents a change from the old one.
    pub fn insert(
        &self,
        descriptor: ModuleDescriptor,
        module_cache: ModuleCache,
    ) -> Result<(), Box<dyn Error>> {
        match self.inner.entry(descriptor) {
            Entry::Occupied(mut entry) => {
                if entry.get() == &module_cache {
                    // Nothing changed, no need to save
                    return Ok(());
                }

                self.save(entry.key(), &module_cache)?;
                *entry.get_mut() = module_cache;
            }
            Entry::Vacant(entry) => {
                self.save(entry.key(), &module_cache)?;
                entry.insert(module_cache);
            }
        }
        Ok(())
    }

    // Returns the number of entries in the cache
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    // Saves the cache for one module.
    fn save(
        &self,
        descriptor: &ModuleDescriptor,
        module_cache: &ModuleCache,
    ) -> Result<(), Box<dyn Error>> {
        if !self.writable {
            return Ok(());
        }
        let directory = match &self.directory {
            Some(directory) => directory,
            None => return Ok(()),
        };

        // Iterate over inner
        if let ModuleDescriptor::Name(name) = descriptor {
            let mut parts = name.split(".").collect::<Vec<_>>();
            if parts.is_empty() {
                return Ok(());
            }
            let last = parts.pop().unwrap();
            let mut path = directory.clone();
            for part in parts {
                path.push(part);
                // Make the directory, if needed
                if !path.exists() {
                    std::fs::create_dir(&path)?;
                }
            }
            path.push(format!("{}.yaml", last));
            module_cache.save(&path)?;
        }

        Ok(())
    }
}
