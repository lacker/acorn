use std::{fmt, path::PathBuf};

use crate::compilation;
use crate::environment::Environment;
use crate::module_cache::ModuleHash;

// The code in one file is exposed to other Acorn code as a "module".
// You could have two different types both named "MyStruct" but defined in different places.
// Each name is uniquely identified not just by its string name, but also by the module it's defined in.
// When you look at the AcornType object, they should have each have a different ModuleId.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(pub u16);

impl ModuleId {
    pub const fn get(&self) -> u16 {
        self.0
    }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.get())
    }
}

pub struct Module {
    // The way the user can refer to this module.
    pub descriptor: ModuleDescriptor,

    // The state of the module, whether it's been loaded or not.
    pub state: LoadState,

    // A hash of the state of the module, to use for the cache.
    // This corresponds to the current state of the module.
    // This may not match the cache held by the BuildCache.
    // In particular, when we have made some changes and are rebuilding, this hash will
    // reflect the current state of the module, while the BuildCache will have a previous good state.
    // None before the module is loaded.
    pub hash: Option<ModuleHash>,
}

impl Module {
    pub fn anonymous() -> Module {
        Module {
            descriptor: ModuleDescriptor::Anonymous,
            state: LoadState::None,
            hash: None,
        }
    }

    // New modules start in the Loading state.
    pub fn new(descriptor: ModuleDescriptor) -> Module {
        Module {
            descriptor,
            state: LoadState::Loading,
            hash: None,
        }
    }

    pub fn load_error(&mut self, error: compilation::Error) {
        self.state = LoadState::Error(error);
    }

    // Called when a module load succeeds.
    pub fn load_ok(&mut self, env: Environment, hash: ModuleHash) {
        self.state = LoadState::Ok(env);
        self.hash = Some(hash);
    }

    pub fn name(&self) -> Option<&str> {
        match &self.descriptor {
            ModuleDescriptor::Name(name) => Some(name),
            _ => None,
        }
    }
}

// The LoadState describes the state of a module, loaded or not or in progress.
pub enum LoadState {
    // There is no such module, not even an id for it
    None,

    // The module is in the process of being loaded.
    Loading,

    // The module has been loaded, but there is an error in its code
    Error(compilation::Error),

    // The module has been loaded successfully and we have its environment
    Ok(Environment),
}

// A Descriptor expresses the different ways that a module user can specify a module.
#[derive(Debug, PartialEq, Eq, Hash, Clone, PartialOrd, Ord)]
pub enum ModuleDescriptor {
    // Anything that can't be referred to
    Anonymous,

    // An import chain like foo.bar.baz
    // This sort of module can be either loaded by a project, or referred to in code.
    Name(String),

    // A filename.
    // This sort of module can be loaded by a project, but not referred to in code.
    File(PathBuf),
}

impl ModuleDescriptor {
    /// Returns true if this is a top-level module that can be imported with just "import x".
    /// These are Name descriptors with no dots in the name.
    pub fn is_top_level(&self) -> bool {
        match self {
            ModuleDescriptor::Name(name) => !name.contains('.'),
            _ => false,
        }
    }

    /// Returns true if this module has an authoritative name for the given type or typeclass name.
    /// A module is authoritative if its name (stripped of underscores and dots) matches
    /// the lowercased type or typeclass name.
    pub fn is_authoritative_name(&self, name: &str) -> bool {
        match self {
            ModuleDescriptor::Name(module_name) => {
                let cleaned_module = module_name.replace('_', "").replace('.', "");
                let cleaned_name = name.to_lowercase();
                cleaned_module == cleaned_name
            }
            _ => false,
        }
    }
}

impl fmt::Display for ModuleDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            ModuleDescriptor::Anonymous => write!(f, "<anonymous>"),
            ModuleDescriptor::Name(name) => write!(f, "{}", name),
            ModuleDescriptor::File(path) => write!(f, "{}", path.display()),
        }
    }
}
