use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::{fmt, io};

use regex::Regex;
use tower_lsp::lsp_types::{CompletionItem, Url};
use walkdir::WalkDir;

use crate::binding_map::BindingMap;
use crate::block::NodeCursor;
use crate::build_cache::BuildCache;
use crate::builder::{BuildEvent, BuildStatus, Builder};
use crate::compilation;
use crate::environment::Environment;
use crate::fact::Fact;
use crate::goal::GoalContext;
use crate::module::{LoadState, Module, ModuleDescriptor, ModuleId, FIRST_NORMAL};
use crate::module_cache::{ModuleCache, ModuleHash};
use crate::proof_step::Truthiness;
use crate::prover::{Outcome, Prover};
use crate::token::Token;

// The Project is responsible for importing different files and assigning them module ids.
pub struct Project {
    // The root directory of the library.
    // This is used to resolve all imports.
    // Note that it may be different from the editor root, which is where the user is working.
    // Set to "/mock" for mock projects.
    library_root: PathBuf,

    // Whether we permit loading files from the filesystem
    use_filesystem: bool,

    // For "open" files, we use the content we are storing rather than the content on disk.
    // This can store either test data that doesn't exist on the filesystem at all, or
    // work in progress whose state is "owned" by an IDE via the language server protocol.
    //
    // Maps filename -> (contents, version number).
    // The version number can mean whatever the caller wants it to mean.
    // From vscode, it'll be the vscode version number.
    open_files: HashMap<PathBuf, (String, i32)>,

    // modules[module_id] is the (ref, Module, hash) for the given module id.
    // Built-in modules have no name.
    modules: Vec<Module>,

    // module_map maps from a module's descriptor to its id
    module_map: HashMap<ModuleDescriptor, ModuleId>,

    // The module names that we want to build.
    targets: HashSet<ModuleDescriptor>,

    // The cache contains a hash for each module from the last time it was cleanly built.
    build_cache: BuildCache,

    // Used as a flag to stop a build in progress.
    pub build_stopped: Arc<AtomicBool>,
}

// An error found while importing a module.
// Not an error in the code of the module itself.
#[derive(Debug)]
pub struct LoadError(pub String);

impl From<io::Error> for LoadError {
    fn from(error: io::Error) -> Self {
        LoadError(format!("{}", error))
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn check_valid_module_part(s: &str, error_name: &str) -> Result<(), LoadError> {
    if s.is_empty() {
        return Err(LoadError(format!("empty module part: {}", error_name)));
    }
    if !s.chars().next().unwrap().is_ascii_lowercase() {
        return Err(LoadError(format!(
            "module parts must start with a lowercase letter: {}",
            error_name
        )));
    }
    for char in s.chars() {
        if !char.is_ascii_alphanumeric() && char != '_' {
            return Err(LoadError(format!(
                "invalid character in module name: '{}' in {}",
                char, error_name
            )));
        }
    }
    Ok(())
}

impl Project {
    // Create a new project.
    // Flags control whether we read the cache, and whether we write the cache.
    pub fn new(library_root: PathBuf, read_cache: bool, write_cache: bool) -> Project {
        let cache_dir = library_root.join("build");

        // Check if the directory exists
        let build_cache = if read_cache && cache_dir.is_dir() {
            BuildCache::new(Some(cache_dir), write_cache)
        } else {
            BuildCache::new(None, false)
        };

        Project {
            library_root,
            use_filesystem: true,
            open_files: HashMap::new(),
            modules: Module::default_modules(),
            module_map: HashMap::new(),
            targets: HashSet::new(),
            build_cache,
            build_stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    // Finds a directory named acornlib, based on the provided path.
    // It can be either:
    //   a parent directory of the provided path
    //   a directory named "acornlib" next to one named "acorn"
    pub fn find_local_acorn_library(start: &Path) -> Option<PathBuf> {
        let mut current = Some(start);

        while let Some(path) = current {
            // Check if path is an acornlib
            if path.ends_with("acornlib") {
                return Some(path.to_path_buf());
            }

            // Check if path has a sibling named acornlib
            if path.ends_with("acorn") {
                let library_path = path.with_file_name("acornlib");
                if library_path.is_dir() {
                    return Some(library_path);
                }
            }

            current = path.parent();
        }

        None
    }

    // A Project based on the current working directory.
    // Returns None if we can't find an acorn library.
    pub fn new_local(use_cache: bool) -> Option<Project> {
        let current_dir = std::env::current_dir().ok()?;
        let library_root = Project::find_local_acorn_library(&current_dir)?;
        Some(Project::new(library_root, use_cache, use_cache))
    }

    // A Project where nothing can be imported.
    pub fn new_mock() -> Project {
        let mock_dir = PathBuf::from("/mock");
        let mut p = Project::new(mock_dir, false, false);
        p.use_filesystem = false;
        p
    }

    // Dropping existing modules lets you update the project for new data.
    // TODO: do this incrementally instead of dropping everything.
    fn drop_modules(&mut self) {
        self.modules = Module::default_modules();
        self.module_map = HashMap::new();
    }

    // You only need read access to an RwLock<Project> to stop the build.
    // When the build is stopped, threads that didn't stop the build themselves should
    // finish any long-running process with an "interrupted" behavior, and give up their
    // locks on the project.
    pub fn stop_build(&self) {
        self.build_stopped
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // You need to have write access to a RwLock<Project> to re-allow the build.
    //
    // To change the project, acquire a read lock, stop the build, acquire a write lock, mess
    // around with the project state however you wanted, then re-allow the build.
    //
    // This asymmetry ensures that when we quickly stop and re-allow the build, any build in
    // progress will in fact stop.
    pub fn allow_build(&mut self) {
        self.build_stopped = Arc::new(AtomicBool::new(false));
    }

    // Returns whether it loaded okay.
    // Either way, it's still added as a target.
    fn add_target_by_descriptor(&mut self, descriptor: &ModuleDescriptor) -> bool {
        let answer = self.load_module(descriptor).is_ok();
        self.targets.insert(descriptor.clone());
        answer
    }

    // Returns whether it loaded okay.
    pub fn add_target_by_name(&mut self, module_name: &str) -> bool {
        self.add_target_by_descriptor(&ModuleDescriptor::Name(module_name.to_string()))
    }

    // Returns whether it loaded okay.
    pub fn add_target_by_path(&mut self, path: &Path) -> bool {
        let descriptor = self.descriptor_from_path(path).unwrap();
        self.add_target_by_descriptor(&descriptor)
    }

    // Adds a target for all files in this directory.
    pub fn add_all_targets(&mut self) {
        if !self.use_filesystem {
            panic!("cannot add all targets without filesystem access")
        }
        for entry in WalkDir::new(&self.library_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let path = entry.path();

                // TODO: remove this when we want to check problems
                // Skip the file if it has the word "problems" in it
                if path.to_str().unwrap().contains("problems") {
                    continue;
                }

                if path.extension() == Some(std::ffi::OsStr::new("ac")) {
                    self.add_target_by_path(path);
                }
            }
        }
    }

    // Whether we currently have this version of a file.
    pub fn has_version(&self, path: &PathBuf, version: i32) -> bool {
        if let Some((_, v)) = self.open_files.get(path) {
            &version == v
        } else {
            false
        }
    }

    // Returns None if we don't have this file at all.
    pub fn get_version(&self, path: &PathBuf) -> Option<i32> {
        self.open_files.get(path).map(|(_, version)| *version)
    }

    // The ModuleHash corresponding to the current build, *not* the known-good build.
    pub fn get_hash(&self, module_id: ModuleId) -> Option<&ModuleHash> {
        self.modules[module_id as usize].hash.as_ref()
    }

    // Updating a file makes us treat it as "open". When a file is open, we use the
    // content in memory for it, rather than the content on disk.
    // Updated files are also added as build targets.
    pub fn update_file(
        &mut self,
        path: PathBuf,
        content: &str,
        version: i32,
    ) -> Result<(), LoadError> {
        if self.has_version(&path, version) {
            // No need to do anything
            return Ok(());
        }
        let descriptor = self.descriptor_from_path(&path)?;
        let mut reload_modules = vec![descriptor];
        if self.open_files.contains_key(&path) {
            // We're changing the value of an existing file. This could invalidate
            // current modules.
            // For now, we just drop everything and reload the targets.
            // TODO: figure out precisely which ones are invalidated.
            self.drop_modules();
            for target in &self.targets {
                reload_modules.push(target.clone());
            }
        }
        self.open_files.insert(path, (content.to_string(), version));
        for descriptor in &reload_modules {
            self.add_target_by_descriptor(descriptor);
        }
        Ok(())
    }

    pub fn close_file(&mut self, path: PathBuf) -> Result<(), LoadError> {
        if !self.open_files.contains_key(&path) {
            // No need to do anything
            return Ok(());
        }
        self.open_files.remove(&path);
        let descriptor = self.descriptor_from_path(&path)?;
        self.drop_modules();
        self.targets.remove(&descriptor);
        let targets = self.targets.clone();
        for target in targets {
            self.add_target_by_descriptor(&target);
        }
        Ok(())
    }

    // Create a Builder object that will then handle the build.
    pub fn builder<'a>(&self, event_handler: impl FnMut(BuildEvent) + 'a) -> Builder<'a> {
        Builder::new(event_handler)
    }

    // Builds all open modules, logging build events.
    pub fn build(&self, builder: &mut Builder) {
        // Build in alphabetical order by module name for consistency.
        let mut targets = self.targets.iter().collect::<Vec<_>>();
        targets.sort();

        builder.log_info(format!(
            "verifying modules: {}",
            targets
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));

        // The first phase is the "loading phase". We load modules and look for errors.
        // If there are errors, we won't try to do proving.
        let mut envs = vec![];
        for target in &targets {
            let module = self.get_module(target);
            match module {
                LoadState::Ok(env) => {
                    builder.module_loaded(&env);
                    envs.push(env);
                }
                LoadState::Error(e) => {
                    if e.secondary {
                        // The real problem is in a different module.
                        // So we don't want to locate the error in this module.
                        builder.log_info(format!("error: {}", e));
                    } else {
                        builder.log_loading_error(target, e);
                    }
                }
                LoadState::None => {
                    // Targets are supposed to be loaded already.
                    builder.log_info(format!("error: module {} is not loaded", target));
                }
                LoadState::Loading => {
                    // Happens if there's a circular import. A more localized error should
                    // show up elsewhere, so let's just log.
                    builder.log_info(format!("error: module {} stuck in loading", target));
                }
            }
        }

        if builder.status.is_error() {
            return;
        }

        builder.loading_phase_complete();

        // The second pass is the "proving phase".
        for (target, env) in targets.into_iter().zip(envs) {
            self.verify_module(&target, env, builder);
            if builder.status.is_error() {
                return;
            }
        }
    }

    // Turns a set of qualified premises from its serializable form into a plain hash set.
    fn load_premises(
        &self,
        module_cache: &Option<ModuleCache>,
        theorem: &Option<String>,
    ) -> Option<HashMap<ModuleId, HashSet<String>>> {
        let normalized = module_cache.as_ref()?.theorems.get(theorem.as_ref()?)?;
        let mut answer = HashMap::new();
        for (module_name, premises) in normalized.iter() {
            // A module could have been renamed, in which case the whole cache is borked.
            let module_id = self.get_module_id_by_name(module_name)?;
            answer.insert(module_id, premises.iter().cloned().collect());
        }
        Some(answer)
    }

    // Turns a hash set of qualified premises into its serializable form.
    fn normalize_premises(
        &self,
        theorem_module_id: ModuleId,
        theorem_name: &str,
        premises: &HashSet<(ModuleId, String)>,
    ) -> BTreeMap<String, BTreeSet<String>> {
        let mut answer = BTreeMap::new();
        for (premise_module_id, premise_name) in premises {
            if *premise_module_id == theorem_module_id && premise_name == theorem_name {
                // We don't need to include the theorem itself as a premise.
                continue;
            }
            let module_name = self.get_module_name_by_id(*premise_module_id).unwrap();
            answer
                .entry(module_name.to_string())
                .or_insert_with(BTreeSet::new)
                .insert(premise_name.clone());
        }
        answer
    }

    // Construct a prover with only the facts that are included in premises.
    // node_index is the top level index of the node we are verifying.
    fn make_filtered_prover(
        &self,
        env: &Environment,
        node_index: usize,
        premises: HashMap<ModuleId, HashSet<String>>,
    ) -> Prover {
        let mut prover = Prover::new(&self, false);

        // Add facts from the dependencies
        for module_id in self.all_dependencies(env.module_id) {
            let module_premises = match premises.get(&module_id) {
                Some(set) => set,
                None => continue,
            };
            let module_env = self.get_env_by_id(module_id).unwrap();
            for fact in module_env.exported_facts(Some(module_premises)) {
                prover.add_fact(fact);
            }
        }

        // Add facts from this file itself
        if let Some(local_premises) = premises.get(&env.module_id) {
            for node in env.nodes.iter().take(node_index) {
                let name = match node.claim.source.name() {
                    Some(name) => name,
                    None => continue,
                };
                if local_premises.contains(&name) {
                    let fact = Fact::new(node.claim.clone(), Truthiness::Factual);
                    prover.add_fact(fact);
                }
            }
        }

        prover
    }

    // Verifies all goals within this module.
    // If we run into an error, we exit without verifying any more goals.
    fn verify_module(&self, target: &ModuleDescriptor, env: &Environment, builder: &mut Builder) {
        if env.nodes.is_empty() {
            // Nothing to prove
            return;
        }

        let module_hash = self.get_hash(env.module_id).unwrap();
        let old_module_cache = self.build_cache.get_cloned(target);
        let mut new_module_cache = ModuleCache::new(module_hash);

        builder.module_proving_started(target.clone());

        // The full prover has access to all facts.
        let mut full_prover = Prover::new(&self, false);
        for fact in self.imported_facts(env.module_id, None) {
            full_prover.add_fact(fact);
        }
        let mut node = NodeCursor::new(&env, 0);

        // Loop over all the nodes that are right below the top level.
        loop {
            let theorem_name = node.current().theorem_name();
            if module_hash.matches_through_line(&old_module_cache, node.current().last_line()) {
                // We don't need to verify this, we can just treat it as verified due to the hash.
                builder.log_proving_cache_hit(&mut node);
                if let (Some(theorem), Some(old_mc)) = (theorem_name, &old_module_cache) {
                    // We skipped the proof of this theorem.
                    // But we still might want its premises cached.
                    // So we copy them over from the old module cache.
                    if let Some(old_theorem_cache) = old_mc.theorems.get(&theorem) {
                        new_module_cache
                            .theorems
                            .insert(theorem, old_theorem_cache.clone());
                    }
                }
            } else {
                // We do need to verify this.

                // If we have a cached set of premises, we use it to create a filtered prover.
                // The filtered prover only contains the premises that we think it needs.
                let old_premises = self.load_premises(&old_module_cache, &theorem_name);
                let filtered_prover =
                    old_premises.map(|ps| self.make_filtered_prover(env, node.top_index(), ps));

                // The premises we use while verifying this block.
                let mut new_premises = HashSet::new();

                // This call will recurse and verify everything within this top-level block.
                self.verify_node(
                    &full_prover,
                    &filtered_prover,
                    &mut node,
                    &mut new_premises,
                    builder,
                );
                if builder.status.is_error() {
                    return;
                }

                if let Some(theorem_name) = theorem_name {
                    new_module_cache.theorems.insert(
                        theorem_name.clone(),
                        self.normalize_premises(env.module_id, &theorem_name, &new_premises),
                    );
                }
            }
            if !node.has_next() {
                break;
            }
            full_prover.add_fact(node.get_fact());
            node.next();
        }

        if builder.module_proving_complete(target) {
            // The module was entirely verified. We can update the cache.
            if let Err(e) = self.build_cache.insert(target.clone(), new_module_cache) {
                builder.log_info(format!("error in build cache: {}", e));
            }
        }
    }

    // Verifies the goal at this node as well as at every child node.
    //
    // full_prover contains all facts that this node can use.
    // filtered_prover contains only the facts that were cached.
    // Our general strategy is to try the filtered prover, if we have it, and only fall back
    // to the full prover if the filtered prover doesn't work.
    //
    // node is a cursor, that typically we will mutate and reset to its original state.
    // new_premises is updated with the premises used in this theorem block.
    // builder tracks statistics and results for the build.
    //
    // If verify_node encounters an error, it stops, leaving node in a borked state.
    fn verify_node(
        &self,
        full_prover: &Prover,
        filtered_prover: &Option<Prover>,
        node: &mut NodeCursor,
        new_premises: &mut HashSet<(ModuleId, String)>,
        builder: &mut Builder,
    ) {
        if node.num_children() == 0 && !node.current().has_goal() {
            // There's nothing to do here
            return;
        }

        let mut full_prover = full_prover.clone();
        let mut filtered_prover = filtered_prover.clone();
        if node.num_children() > 0 {
            // We need to recurse into children
            node.descend(0);
            loop {
                self.verify_node(&full_prover, &filtered_prover, node, new_premises, builder);
                if builder.status.is_error() {
                    return;
                }

                full_prover.add_fact(node.get_fact());
                if let Some(ref mut filtered_prover) = filtered_prover {
                    filtered_prover.add_fact(node.get_fact());
                }

                if node.has_next() {
                    node.next();
                } else {
                    break;
                }
            }
            node.ascend();
        }

        if node.current().has_goal() {
            let goal_context = node.goal_context().unwrap();
            let prover =
                self.verify_with_fallback(full_prover, filtered_prover, &goal_context, builder);
            if builder.status.is_error() {
                return;
            }

            // Gather the premises used by this proof
            prover
                .useful_fact_qualified_names()
                .iter()
                .for_each(|fact| {
                    new_premises.insert(fact.clone());
                });
        }
    }

    // Tries to use the filtered prover to verify this goal, but falls back to the full prover
    // if that doesn't work.
    // Returns the prover that was used.
    fn verify_with_fallback(
        &self,
        mut full_prover: Prover,
        filtered_prover: Option<Prover>,
        goal_context: &GoalContext,
        builder: &mut Builder,
    ) -> Prover {
        // Try the filtered prover
        if let Some(mut filtered_prover) = filtered_prover {
            builder.searches_filtered += 1;
            filtered_prover.set_goal(goal_context);
            let start = std::time::Instant::now();
            let outcome = filtered_prover.verification_search();
            if outcome == Outcome::Success {
                builder.search_finished(&filtered_prover, goal_context, outcome, start.elapsed());
                return filtered_prover;
            }
        }

        // Fall back to the full prover
        builder.searches_full += 1;
        full_prover.set_goal(goal_context);
        let start = std::time::Instant::now();
        let outcome = full_prover.verification_search();
        builder.search_finished(&full_prover, goal_context, outcome, start.elapsed());
        full_prover
    }

    // Does the build and returns when it's done, rather than asynchronously.
    // Returns (status, events, searches_success, cache).
    pub fn sync_build(&self) -> (BuildStatus, Vec<BuildEvent>, i32) {
        let mut events = vec![];
        let (status, searches_success) = {
            let mut builder = self.builder(|event| events.push(event));
            self.build(&mut builder);
            (builder.status, builder.searches_success)
        };
        (status, events, searches_success)
    }

    // Set the file content. This has priority over the actual filesystem.
    pub fn mock(&mut self, filename: &str, content: &str) {
        assert!(!self.use_filesystem);
        let path = PathBuf::from(filename);
        let next_version = match self.get_version(&path) {
            Some(version) => version + 1,
            None => 0,
        };
        self.update_file(path, content, next_version)
            .expect("mock file update failed");
    }

    pub fn get_module_by_id(&self, module_id: ModuleId) -> &LoadState {
        match self.modules.get(module_id as usize) {
            Some(module) => &module.state,
            None => &LoadState::None,
        }
    }

    pub fn get_module(&self, descriptor: &ModuleDescriptor) -> &LoadState {
        match self.module_map.get(descriptor) {
            Some(id) => self.get_module_by_id(*id),
            None => &LoadState::None,
        }
    }

    pub fn get_module_name_by_id(&self, module_id: ModuleId) -> Option<&str> {
        match self.modules.get(module_id as usize) {
            Some(module) => module.name(),
            None => None,
        }
    }

    fn get_module_id_by_name(&self, module_name: &str) -> Option<ModuleId> {
        self.module_map
            .get(&ModuleDescriptor::Name(module_name.to_string()))
            .copied()
    }

    pub fn get_env_by_id(&self, module_id: ModuleId) -> Option<&Environment> {
        if let LoadState::Ok(env) = self.get_module_by_id(module_id) {
            Some(env)
        } else {
            None
        }
    }

    // You have to use the canonical descriptor, here. You can't use the path for a module
    // that can also be referenced by name.
    pub fn get_env(&self, descriptor: &ModuleDescriptor) -> Option<&Environment> {
        if let Some(module_id) = self.module_map.get(&descriptor) {
            self.get_env_by_id(*module_id)
        } else {
            None
        }
    }

    pub fn errors(&self) -> Vec<(ModuleId, &compilation::Error)> {
        let mut errors = vec![];
        for (module_id, module) in self.modules.iter().enumerate() {
            if let LoadState::Error(e) = &module.state {
                errors.push((module_id as ModuleId, e));
            }
        }
        errors
    }

    fn read_file(&mut self, path: &PathBuf) -> Result<String, LoadError> {
        if let Some((content, _)) = self.open_files.get(path) {
            return Ok(content.clone());
        }
        if !self.use_filesystem {
            return Err(LoadError(format!("no mocked file for: {}", path.display())));
        }
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(s),
            Err(e) => Err(LoadError(format!(
                "error loading {}: {}",
                path.display(),
                e
            ))),
        }
    }

    // Returns the canonical descriptor for a path.
    // Returns a load error if this isn't a valid path for an acorn file.
    pub fn descriptor_from_path(&self, path: &Path) -> Result<ModuleDescriptor, LoadError> {
        let relative = match path.strip_prefix(&self.library_root) {
            Ok(relative) => relative,
            Err(_) => return Ok(ModuleDescriptor::File(path.to_path_buf())),
        };
        let components: Vec<_> = relative
            .components()
            .map(|comp| comp.as_os_str().to_string_lossy())
            .collect();
        let mut name = String::new();
        for (i, component) in components.iter().enumerate() {
            let part = if i + 1 == components.len() {
                if !component.ends_with(".ac") {
                    return Err(LoadError(format!(
                        "path {} does not end with .ac",
                        path.display()
                    )));
                }
                component[..component.len() - 3].to_string()
            } else {
                component.to_string()
            };
            if i > 0 {
                name.push('.');
            }
            name.push_str(&part);
            check_valid_module_part(&part, &name)?;
        }

        Ok(ModuleDescriptor::Name(name))
    }

    pub fn path_from_module_name(&self, module_name: &str) -> Result<PathBuf, LoadError> {
        let mut path = self.library_root.clone();
        let parts: Vec<&str> = module_name.split('.').collect();

        for (i, part) in parts.iter().enumerate() {
            check_valid_module_part(part, module_name)?;

            let component = if i + 1 == parts.len() {
                format!("{}.ac", part)
            } else {
                part.to_string()
            };
            path.push(component);
        }
        Ok(path)
    }

    pub fn path_from_descriptor(&self, descriptor: &ModuleDescriptor) -> Option<PathBuf> {
        let name = match descriptor {
            ModuleDescriptor::Name(name) => name,
            ModuleDescriptor::File(path) => return Some(path.clone()),
            ModuleDescriptor::Anonymous => return None,
        };

        match self.path_from_module_name(&name) {
            Ok(path) => Some(path),
            Err(_) => None,
        }
    }

    pub fn url_from_descriptor(&self, descriptor: &ModuleDescriptor) -> Option<Url> {
        let path = self.path_from_descriptor(descriptor)?;
        Url::from_file_path(path).ok()
    }

    pub fn path_from_module_id(&self, module_id: ModuleId) -> Option<PathBuf> {
        self.path_from_descriptor(&self.modules[module_id as usize].descriptor)
    }

    // Loads a module from cache if possible, or else from the filesystem.
    // Module names are a .-separated list where each one must be [a-z_].
    // Each component maps to a subdirectory, except the last one, which maps to a .ac file.
    // load returns an error if the module-loading process itself has an error.
    // For example, we might have an invalid name, the file might not exist, or this
    // might be a circular import.
    // If there is an error in the file, the load will return a module id, but the module
    // for the id will have an error.
    // If "open" is passed, then we cache this file's content in open files.
    fn load_module(&mut self, descriptor: &ModuleDescriptor) -> Result<ModuleId, LoadError> {
        if let Some(module_id) = self.module_map.get(&descriptor) {
            if *module_id < FIRST_NORMAL {
                panic!("module {} should not be loadable", module_id);
            }
            if let LoadState::Loading = self.get_module_by_id(*module_id) {
                return Err(LoadError(format!("circular import of {}", descriptor)));
            }
            return Ok(*module_id);
        }

        let path = match self.path_from_descriptor(descriptor) {
            Some(path) => path,
            None => return Err(LoadError(format!("unloadable module: {:?}", descriptor))),
        };
        let text = self.read_file(&path)?;

        // Give this module an id before parsing it, so that we can catch circular imports.
        let module_id = self.modules.len() as ModuleId;
        self.modules.push(Module::new(descriptor.clone()));
        self.module_map.insert(descriptor.clone(), module_id);

        let mut env = Environment::new(module_id);
        let tokens = Token::scan(&text);
        if let Err(e) = env.add_tokens(self, tokens) {
            self.modules[module_id as usize].load_error(e);
            return Ok(module_id);
        }

        // Hash this module, reflecting its state on disk.
        let module_hash = ModuleHash::new(
            &text,
            env.bindings
                .direct_dependencies()
                .iter()
                .map(|dep_id| &self.modules[*dep_id as usize]),
        );
        self.modules[module_id as usize].load_ok(env, module_hash);
        Ok(module_id)
    }

    pub fn load_module_by_name(&mut self, module_name: &str) -> Result<ModuleId, LoadError> {
        let descriptor = ModuleDescriptor::Name(module_name.to_string());
        self.load_module(&descriptor)
    }

    // Appends all dependencies, including chains of direct dependencies.
    // Ie, if A imports B and B imports C, then A depends on B and C.
    // The order will be the "pop order", so that each module is added only
    // after all of its dependencies are added.
    pub fn all_dependencies(&self, original_module_id: ModuleId) -> Vec<ModuleId> {
        let mut answer = vec![];
        let mut seen = HashSet::new();
        self.append_dependencies(&mut seen, &mut answer, original_module_id);
        answer
    }

    // Helper function for all_dependencies.
    // Returns "false" if we have already seen this dependency.
    // Does not append module_id itself. If we want it, add that in last.
    fn append_dependencies(
        &self,
        seen: &mut HashSet<ModuleId>,
        output: &mut Vec<ModuleId>,
        module_id: ModuleId,
    ) -> bool {
        if !seen.insert(module_id) {
            return false;
        }
        if let LoadState::Ok(env) = self.get_module_by_id(module_id) {
            for dep in env.bindings.direct_dependencies() {
                if self.append_dependencies(seen, output, dep) {
                    output.push(dep);
                }
            }
        }
        true
    }

    pub fn get_bindings(&self, module_id: ModuleId) -> Option<&BindingMap> {
        if let LoadState::Ok(env) = self.get_module_by_id(module_id) {
            Some(&env.bindings)
        } else {
            None
        }
    }

    // All facts that the given module imports.
    pub fn imported_facts(
        &self,
        module_id: ModuleId,
        filter: Option<&HashSet<String>>,
    ) -> Vec<Fact> {
        let mut facts = vec![];
        for dependency in self.all_dependencies(module_id) {
            let env = self.get_env_by_id(dependency).unwrap();
            facts.extend(env.exported_facts(filter));
        }
        facts
    }

    // path is the file we're in.
    // env_line is zero-based. It's the closest unchanged line, to use for finding the environment.
    // prefix is the entire line they've typed so far. Generally different from env_line.
    // Returns a list of completions, or None if we don't have any.
    pub fn get_completions(
        &self,
        path: Option<&Path>,
        env_line: u32,
        prefix: &str,
    ) -> Option<Vec<CompletionItem>> {
        let re = Regex::new(r"[a-zA-Z0-9._]+").unwrap();
        let parts: Vec<&str> = re.find_iter(prefix).map(|mat| mat.as_str()).collect();
        if parts.len() == 4 && parts[0] == "from" && parts[2] == "import" {
            // We are in a "from X import Y" statement.
            let name = parts[1];
            let partial = parts[3];
            let descriptor = ModuleDescriptor::Name(name.to_string());
            let env = match self.get_env(&descriptor) {
                Some(env) => env,
                None => {
                    // The module isn't loaded, so we don't know what names it has.
                    if name == "nat" && "Nat".starts_with(partial) {
                        // Cheat to optimize the tutorial.
                        // If we always loaded nat, we wouldn't need this.
                        return Some(vec![CompletionItem {
                            label: "Nat".to_string(),
                            kind: Some(tower_lsp::lsp_types::CompletionItemKind::CLASS),
                            ..Default::default()
                        }]);
                    }
                    return None;
                }
            };
            return env.bindings.get_completions(&self, partial, true);
        }

        // If we don't have a path, we can only complete imports.
        let path = path?;

        // Check if we have a completable word
        let word = match parts.last() {
            Some(word) => *word,
            None => return None,
        };

        if !word.chars().all(|c| Token::identifierish(c) || c == '.') {
            return None;
        }

        // Find the right environment
        let descriptor = self.descriptor_from_path(&path).ok()?;
        let env = match self.get_env(&descriptor) {
            Some(env) => env,
            None => return None,
        };
        let env = env.env_for_line(env_line);

        env.bindings.get_completions(&self, word, false)
    }

    // Yields (url, version) for all open files.
    pub fn open_urls(&self) -> impl Iterator<Item = (Url, i32)> + '_ {
        self.open_files.iter().filter_map(|(path, (_, version))| {
            Url::from_file_path(path.clone())
                .ok()
                .map(|url| (url, *version))
        })
    }

    // Expects the module to load successfully and for there to be no errors in the loaded module.
    // Only for testing.
    pub fn expect_ok(&mut self, module_name: &str) -> ModuleId {
        let module_id = self.load_module_by_name(module_name).expect("load failed");
        match self.get_module_by_id(module_id) {
            LoadState::Ok(_) => module_id,
            LoadState::Error(e) => panic!("error in {}: {}", module_name, e),
            _ => panic!("logic error"),
        }
    }

    // This expects there to be an error during loading itself.
    #[cfg(test)]
    fn expect_load_err(&mut self, module_name: &str) {
        assert!(self.load_module_by_name(module_name).is_err());
    }

    // This expects the module to load, but for there to be an error in the loaded module.
    #[cfg(test)]
    fn expect_module_err(&mut self, module_name: &str) {
        let module_id = self.load_module_by_name(module_name).expect("load failed");
        if let LoadState::Error(_) = self.get_module_by_id(module_id) {
            // What we expected
        } else {
            panic!("expected error");
        }
    }

    // Checks that the given expression can be parsed and turned back into code.
    #[cfg(test)]
    fn check_code_into(&mut self, module_name: &str, input: &str, expected: &str) {
        use crate::expression::Expression;
        let module_id = self.expect_ok(module_name);
        let expression = Expression::expect_value(input);
        let env = self.get_env_by_id(module_id).expect("no env");
        let value = match env.bindings.evaluate_value(self, &expression, None) {
            Ok(value) => value,
            Err(e) => panic!("evaluation error: {}", e),
        };
        let output = match env.bindings.value_to_code(&value) {
            Ok(output) => output,
            Err(e) => panic!("code generation error: {}", e),
        };

        if output != expected {
            panic!(
                "\nconverted:\n  {}\nto value:\n  {}\nand back to:\n  {}\nbut expected:\n  {}\n",
                input, value, output, expected
            );
        }
    }

    #[cfg(test)]
    fn check_code(&mut self, module_name: &str, code: &str) {
        self.check_code_into(module_name, code, code);
    }

    // Returns num_success.
    #[cfg(test)]
    fn expect_build_ok(&mut self) -> i32 {
        let (status, events, searches_success) = self.sync_build();
        assert_eq!(status, BuildStatus::Good);
        assert!(events.len() > 0);
        let (done, total) = events.last().unwrap().progress.unwrap();
        assert_eq!(done, total, "expected number of build events didn't match");
        searches_success
    }

    #[cfg(test)]
    fn expect_build_fails(&mut self) {
        let (status, _, _) = self.sync_build();
        assert_ne!(status, BuildStatus::Good, "expected build to fail");
    }
}

#[cfg(test)]
mod tests {
    use crate::environment::LineType;

    use super::*;

    const FOO_AC: &str = r#"
    // Imported by other tests
    type Foo: axiom
    type AlsoFoo: Foo
    type NotFoo: axiom
    let foo: Foo = axiom
    define fooify(x: Foo) -> Foo { foo }
    "#;

    #[test]
    fn test_basic_import() {
        let mut p = Project::new_mock();
        p.mock("/mock/foo.ac", FOO_AC);
        p.mock("/mock/main.ac", "import foo");
        p.expect_ok("main");
    }

    #[test]
    fn test_direct_import_nonexistent() {
        let mut p = Project::new_mock();
        p.expect_load_err("main");
    }

    #[test]
    fn test_indirect_import_nonexistent() {
        let mut p = Project::new_mock();
        p.mock("/mock/main.ac", "import nonexistent");
        p.expect_module_err("main");
    }

    #[test]
    fn test_nonexistent_property() {
        let mut p = Project::new_mock();
        p.mock("/mock/foo.ac", FOO_AC);
        p.mock(
            "/mock/main.ac",
            r#"
            import foo
            let bar: foo.nonexistent = axiom
        "#,
        );
        p.expect_module_err("main");
    }

    #[test]
    fn test_circular_imports() {
        let mut p = Project::new_mock();
        p.mock("/mock/a.ac", "import b");
        p.mock("/mock/b.ac", "import c");
        p.mock("/mock/c.ac", "import a");
        p.expect_module_err("a");
        // The error should show up in c.ac, not in a.ac
        assert!(p.errors().len() > 0);
    }

    #[test]
    fn test_self_import() {
        let mut p = Project::new_mock();
        p.mock("/mock/a.ac", "import a");
        p.expect_module_err("a");
    }

    #[test]
    fn test_import_from_subdir() {
        let mut p = Project::new_mock();
        p.mock("/mock/stuff/foo.ac", FOO_AC);
        p.mock("/mock/main.ac", "import stuff.foo");
        p.expect_ok("main");
    }

    #[test]
    fn test_good_imported_types() {
        let mut p = Project::new_mock();
        p.mock("/mock/foo.ac", FOO_AC);
        p.mock(
            "/mock/main.ac",
            r#"
            import foo
            type MyFoo: foo.AlsoFoo
            let x: foo.Foo = axiom
            let y: MyFoo = axiom
            let z: Bool = (x = y)
        "#,
        );
        p.expect_ok("main");
    }

    #[test]
    fn test_bad_imported_types() {
        let mut p = Project::new_mock();
        p.mock("/mock/foo.ac", FOO_AC);
        p.mock(
            "/mock/main.ac",
            r#"
            import foo
            type MyFoo: foo.NotFoo
            let x: foo.Foo = axiom
            let y: MyFoo = axiom
            let z: Bool = (x = y)
        "#,
        );
        p.expect_module_err("main");
    }

    #[test]
    fn test_imported_constants() {
        let mut p = Project::new_mock();
        p.mock("/mock/foo.ac", FOO_AC);
        p.mock(
            "/mock/main.ac",
            r#"
            import foo
            let x: foo.Foo = axiom
            let y: foo.Foo = foo.foo
            let z: Bool = (x = y)
        "#,
        );
        p.expect_ok("main");
    }

    #[test]
    fn test_code_generation() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/main.ac",
            r#"
            type MyType: axiom
            let t: MyType = axiom
        "#,
        );
        p.check_code("main", "t");
        p.check_code("main", "forall(x0: MyType) { x0 = t }");
    }

    #[test]
    fn test_code_for_imported_things() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/stuff.ac",
            r#"
            let thing1: Bool = axiom
            let thing2: Bool = axiom
        "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            import stuff
            let st1: Bool = stuff.thing1
        "#,
        );
        p.check_code_into("main", "stuff.thing1", "st1");
        p.check_code("main", "st1");
        p.check_code("main", "stuff.thing2");
    }

    #[test]
    fn test_building_project() {
        let mut p = Project::new_mock();
        p.mock("/mock/foo.ac", FOO_AC);
        p.mock(
            "/mock/main.ac",
            r#"
            import foo
            let new_foo: foo.Foo = axiom
            theorem goal { foo.fooify(new_foo) = foo.foo }
        "#,
        );
        p.load_module_by_name("foo").expect("loading foo failed");
        p.load_module_by_name("main").expect("loading main failed");
        p.add_target_by_name("foo");
        p.add_target_by_name("main");
        p.expect_build_ok();
    }

    #[test]
    fn test_target_outside_library() {
        let mut p = Project::new_mock();
        let outside_path = "/outside/foo.ac";
        p.mock(outside_path, FOO_AC);
        p.add_target_by_path(&PathBuf::from(outside_path));
        p.expect_build_ok();
    }

    #[test]
    fn test_imported_member_functions() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/boolpair.ac",
            r#"
            structure BoolPair {
                first: Bool
                second: Bool
            }
        "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            import boolpair
            type BoolPair: boolpair.BoolPair
            let first: BoolPair -> Bool = BoolPair.first
        "#,
        );
        p.expect_ok("main");
        p.check_code("main", "first");
        p.check_code_into("main", "BoolPair.first", "first");
        p.check_code_into("main", "boolpair.BoolPair.first", "first");

        p.check_code("main", "BoolPair.second");
        p.check_code_into("main", "boolpair.BoolPair.second", "BoolPair.second");
    }

    #[test]
    fn test_structure_aliasing() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/stuff.ac",
            r#"
            structure Foo {
                member: Bool
            }
            type Bar: Foo
        "#,
        );
        p.expect_ok("stuff");
        p.check_code_into("stuff", "Bar.member", "Foo.member");
    }

    #[test]
    fn test_names_imported_via_from() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/stuff.ac",
            r#"
            type Foo: axiom
            class Foo {
                let foo: Bool = true
                let foo2: Bool = false
            }
            type Bar: Foo
            let bar: Bar = axiom
        "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            from stuff import Foo, Bar, bar
            let x: Bool = Bar.foo
            let y: Bar = bar
        "#,
        );
        p.expect_ok("stuff");
        p.expect_ok("main");
        p.check_code("main", "x");
        p.check_code_into("main", "y", "bar");
        p.check_code_into("main", "stuff.Foo.foo2", "Foo.foo2");
    }

    #[test]
    fn test_build_catches_unsolved_solve_blocks() {
        let mut p = Project::new_mock();
        p.mock("/mock/main.ac", "solve false by {\n}");
        p.expect_ok("main");
        p.expect_build_fails();
    }

    #[test]
    fn test_imported_numbers_codegen_basic() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/nat.ac",
            r#"
            inductive Nat {
                0
                suc(Nat)
            }

            numerals Nat

            class Nat {
                define add(self, other: Nat) -> Nat {
                    0
                }
            }
        "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            from nat import Nat
            "#,
        );
        p.check_code_into("main", "nat.Nat.0", "Nat.0");
        p.check_code_into("main", "Nat.suc(Nat.0)", "Nat.0.suc");
        p.check_code_into("main", "Nat.add(Nat.0, Nat.0)", "Nat.0 + Nat.0");
    }

    #[test]
    fn test_imported_numbers_codegen_with_numerals() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/nat.ac",
            r#"
            inductive Nat {
                0
                suc(Nat)
            }

            numerals Nat

            class Nat {
                define add(self, other: Nat) -> Nat {
                    0
                }
            }
        "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            from nat import Nat
            numerals Nat
            "#,
        );
        p.check_code_into("main", "nat.Nat.0", "0");
        p.check_code_into("main", "Nat.suc(Nat.0)", "0.suc");
        p.check_code_into("main", "Nat.add(Nat.0, Nat.0)", "0 + 0");
    }

    #[test]
    fn test_import_without_from_codegen() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/boolbox.ac",
            r#"
            structure BoolBox {
                item: Bool
            }
        "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            import boolbox
        "#,
        );
        p.check_code("main", "forall(x0: boolbox.BoolBox) { true }");
    }

    #[test]
    fn test_repeated_verification() {
        let mut p = Project::new_mock();
        let nat_text = r#"
        inductive Nat {
            0
            suc(Nat)
        }

        let nz: Nat = axiom
        axiom nz_nonzero {
            nz != Nat.0
        }
        "#;
        p.mock("/mock/nat.ac", nat_text);
        let main_text = r#"
            from nat import Nat
            let x: Nat = axiom
            let y: Nat = axiom

            theorem goal1(a: Nat) {
                a != x or a != y or x = y
            } by {
                if a = x {
                    if a = y {
                        x = y
                    }
                    a != y or x = y
                }
                a != x or a != y or x = y
            }

            // Relies on imported things
            theorem goal2 {
                exists(b: Nat) { nat.nz = b.suc }
            }
            "#;
        p.mock("/mock/main.ac", main_text);

        let main_descriptor = ModuleDescriptor::Name("main".to_string());
        let env = p.get_env(&main_descriptor).unwrap();
        let goal_count = env.iter_goals().count() as i32;
        assert_eq!(goal_count, 5);

        // The first verification should populate the cache, starting from an empty cache.
        let mut builder = Builder::new(|_| {});
        p.verify_module(&main_descriptor, &env, &mut builder);
        assert_eq!(builder.status, BuildStatus::Good);
        assert_eq!(builder.searches_total, 5);
        assert_eq!(builder.searches_full, 5);
        assert_eq!(builder.searches_filtered, 0);
        let module_cache = p.build_cache.get_cloned(&main_descriptor).unwrap();
        assert_eq!(module_cache.theorems.len(), 2);
        module_cache.assert_premises_eq("goal1", &[]);
        module_cache.assert_premises_eq("goal2", &["nat:Nat.new", "nat:nz_nonzero"]);

        // Run a second verification with no changes. This should use the cache.
        let mut builder = Builder::new(|_| {});
        p.verify_module(&main_descriptor, &env, &mut builder);
        assert_eq!(builder.status, BuildStatus::Good);
        assert_eq!(builder.searches_total, 0);
        assert_eq!(builder.searches_full, 0);
        assert_eq!(builder.searches_filtered, 0);
        let module_cache = p.build_cache.get_cloned(&main_descriptor).unwrap();
        assert_eq!(module_cache.theorems.len(), 2);
        module_cache.assert_premises_eq("goal1", &[]);
        module_cache.assert_premises_eq("goal2", &["nat:Nat.new", "nat:nz_nonzero"]);

        // After we bust all the hashes, it should use the premise cache.
        p.mock("/mock/nat.ac", format!("// \n{}", nat_text).as_str());
        let env = p.get_env(&main_descriptor).unwrap();
        let mut builder = Builder::new(|_| {});
        p.verify_module(&main_descriptor, &env, &mut builder);
        assert_eq!(builder.status, BuildStatus::Good);
        assert_eq!(builder.searches_total, 5);
        assert_eq!(builder.searches_full, 0);
        assert_eq!(builder.searches_filtered, 5);
        let module_cache = p.build_cache.get_cloned(&main_descriptor).unwrap();
        assert_eq!(module_cache.theorems.len(), 2);
        module_cache.assert_premises_eq("goal1", &[]);
        module_cache.assert_premises_eq("goal2", &["nat:Nat.new", "nat:nz_nonzero"]);
    }

    #[test]
    fn test_completions() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/nat.ac",
            r#"
            inductive Nat {
                0
                suc(Nat)
            }

            theorem ugly {
                true = true
            }
            "#,
        );
        let main = PathBuf::from("/mock/main.ac");
        p.mock(
            main.to_str().unwrap(),
            r#"
            from nat import Nat
            let foo: Nat = axiom
            let bar: Nat = axiom
            theorem goal(bop: Nat) {
                bop != foo or bop != bar or foo = bar
            } by {
                // This should be line 7. Let's test completions here.
            }
            "#,
        );
        let env = p
            .get_env(&ModuleDescriptor::Name("main".to_string()))
            .unwrap();

        // Make sure the indexes are what we expect
        assert_eq!(env.get_line_type(0), Some(LineType::Empty));
        assert_eq!(env.get_line_type(1), Some(LineType::Other));
        assert_eq!(env.get_line_type(2), Some(LineType::Other));
        assert_eq!(env.get_line_type(3), Some(LineType::Other));
        assert_eq!(env.get_line_type(4), Some(LineType::Node(0)));
        assert_eq!(env.get_line_type(5), Some(LineType::Node(0)));
        assert_eq!(env.get_line_type(6), Some(LineType::Node(0)));
        assert_eq!(env.get_line_type(7), Some(LineType::Node(0)));
        assert_eq!(env.get_line_type(8), Some(LineType::Node(0)));
        assert_eq!(env.get_line_type(9), None);

        let check = |prefix: &str, line: u32, expected: &[&str]| {
            let completions = match p.get_completions(Some(&main), line, prefix) {
                Some(c) => c,
                None => {
                    assert!(expected.is_empty(), "no completions found for '{}'", prefix);
                    return;
                }
            };
            let labels: Vec<_> = completions.iter().map(|c| &c.label).collect();
            assert_eq!(labels, expected, "completions for '{}'", prefix);
        };

        // Test completions
        check("from nat import N", 0, &["Nat"]);
        check("ba", 7, &["bar"]);
        check("fo", 7, &["forall", "foo"]);
        check("b", 7, &["by", "bar", "bop"]);
        check("Nat.s", 7, &["suc"]);
        check("foo.s", 7, &["suc"]);
        check("nat.N", 7, &["Nat"]);
        check("(ba", 7, &["bar"]);
        check("nat.u", 7, &[]);
        check("nat.", 7, &["Nat"]);
        check("foo.", 7, &["0", "induction", "suc"]);
    }

    #[test]
    fn test_build_cache() {
        let mut p = Project::new_mock();
        let lib_text = r#"
        let thing1: Bool = axiom
        let thing2: Bool = thing1

        theorem one {
            thing1 = thing2
        }
        "#;
        let main_text = r#"
        import lib
        theorem two {
            lib.thing1 = lib.thing2
        }
        "#;
        p.mock("/mock/lib.ac", lib_text);
        p.mock("/mock/main.ac", main_text);
        let num_success = p.expect_build_ok();
        assert_eq!(num_success, 2);
        assert_eq!(p.build_cache.len(), 2);

        // Just rebuilding a second time should require no work
        let num_success = p.expect_build_ok();
        assert_eq!(num_success, 0);

        // If we change main, we should only have to rebuild main
        let touched_main = format!("// Touch\n{}", main_text);
        p.update_file(PathBuf::from("/mock/main.ac"), &touched_main, 1)
            .expect("update failed");
        let num_success = p.expect_build_ok();
        assert_eq!(num_success, 1);

        // If we change lib, we should have to rebuild both
        let touched_lib = format!("// Touch\n{}", lib_text);
        p.update_file(PathBuf::from("/mock/lib.ac"), &touched_lib, 1)
            .expect("update failed");
        let num_success = p.expect_build_ok();
        assert_eq!(num_success, 2);
    }

    #[test]
    fn test_build_cache_partial_rebuild() {
        let mut p = Project::new_mock();
        let mut lines = vec![
            "theorem one {",
            "    true = true",
            "}",
            "theorem two {",
            "    true = true",
            "}",
            "theorem three {",
            "    true = true",
            "}",
        ];
        let filename = "/mock/main.ac";
        p.mock(filename, &lines.join("\n"));
        let num_success = p.expect_build_ok();
        assert_eq!(num_success, 3);

        // Change the middle theorem
        lines[4] = "    false = false";
        p.update_file(PathBuf::from(filename), &lines.join("\n"), 1)
            .expect("update failed");
        let num_success = p.expect_build_ok();
        assert_eq!(num_success, 2);
    }

    #[test]
    fn test_importing_a_generic_type() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/pair.ac",
            r#"
            structure Pair<T, U> {
                first: T
                second: U
            }
            "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            from pair import Pair
            "#,
        );
        p.check_code("main", "forall(x0: Pair<Bool, Bool>) { true }");
        p.check_code(
            "main",
            "forall(x0: Bool, x1: Bool) { Pair.new(x0, x1).second = x1 }",
        );
    }

    #[test]
    fn test_generic_type_in_imported_module() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/pair.ac",
            r#"
            structure Pair<T, U> {
                first: T
                second: U
            }
            "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            import pair
            "#,
        );
        p.check_code("main", "forall(x0: pair.Pair<Bool, Bool>) { true }");
    }

    #[test]
    fn test_aliasing_local_generic_constant() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/pair.ac",
            r#"
            structure Pair<T, U> {
                first: T
                second: U
            }

            let pbbn: (Bool, Bool) -> Pair<Bool, Bool> = Pair<Bool, Bool>.new
            "#,
        );
        p.expect_ok("pair");
        p.check_code("pair", "pbbn(false, true)");
    }

    #[test]
    fn test_importing_generic_function() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/pair.ac",
            r#"
            structure Pair<T, U> {
                first: T
                second: U
            }

            define double<T>(x: T) -> Pair<T, T> {
                Pair.new(x, x)
            }
            "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            from pair import double
            "#,
        );
        p.check_code("main", "double(true)");
    }

    #[test]
    fn test_generic_function_in_imported_module() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/pair.ac",
            r#"
            structure Pair<T, U> {
                first: T
                second: U
            }

            define double<T>(x: T) -> Pair<T, T> {
                Pair.new(x, x)
            }
            "#,
        );
        p.mock(
            "/mock/main.ac",
            r#"
            import pair
            "#,
        );
        p.check_code("main", "pair.double(true)");
    }
}
