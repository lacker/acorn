use std::collections::HashMap;

use crate::acorn_type::AcornType;
use crate::acorn_value::ConstantInstance;
use crate::atom::{Atom, AtomId};
use crate::names::ConstantName;
use crate::term::{Term, TypeId};

/// In the Acorn language, constants and types have names, scoped by modules. They can be rich values
/// with internal structure, like polymorphic parameters or complex types.
/// The prover, on the other hand, operates in simply typed higher order logic.
/// The NormalizationMap is a mapping between the two.
#[derive(Clone)]
pub struct NormalizationMap {
    /// For global constant i in the prover, global_constants[i] is the corresponding ConstantName.
    /// Part of the Atom -> ConstantName lookup direction.
    global_constants: Vec<Option<ConstantName>>,

    /// For local constant i in the prover, local_constants[i] is the corresponding ConstantName.
    /// Part of the Atom -> ConstantName lookup direction.
    local_constants: Vec<Option<ConstantName>>,

    /// Inverse map of constants that can be referenced with a single name.
    /// The ConstantName -> Atom lookup direction.
    name_to_atom: HashMap<ConstantName, Atom>,

    /// type_to_type_id[acorn_type] is the TypeId
    type_to_type_id: HashMap<AcornType, TypeId>,

    /// type_id_to_type[type_id] is the AcornType
    type_id_to_type: Vec<AcornType>,

    /// One entry for each monomorphization.
    /// Maps the rich constant to the Atom and TypeId that represent the monomorph.
    /// It might not be a monomorph-type atom, if it's an alias to another constant.
    /// So it isn't quite parallel to id_to_monomorph.
    monomorph_to_id: HashMap<ConstantInstance, (Atom, TypeId)>,

    /// Indexed by the AtomId of the monomorph.
    /// For each id, store the rich constant corresponding to it.
    id_to_monomorph: Vec<ConstantInstance>,
}

impl NormalizationMap {
    pub fn new() -> NormalizationMap {
        let mut map = NormalizationMap {
            global_constants: vec![],
            local_constants: vec![],
            name_to_atom: HashMap::new(),
            type_to_type_id: HashMap::new(),
            type_id_to_type: vec![],
            id_to_monomorph: vec![],
            monomorph_to_id: HashMap::new(),
        };
        map.add_type(&AcornType::Empty);
        map.add_type(&AcornType::Bool);
        map
    }

    /// Assigns an id to this (module, name) pair if it doesn't already have one.
    /// local determines whether the constant will be represented as a local or global atom.
    pub fn add_constant(&mut self, name: ConstantName, local: bool) -> Atom {
        if name.is_skolem() {
            panic!("skolem constants should not be stored in the ConstantMap");
        }
        if let Some(&atom) = self.name_to_atom.get(&name) {
            return atom;
        }
        let atom = if local {
            let atom_id = self.local_constants.len() as AtomId;
            self.local_constants.push(Some(name.clone()));
            Atom::LocalConstant(atom_id)
        } else {
            let atom_id = self.global_constants.len() as AtomId;
            self.global_constants.push(Some(name.clone()));
            Atom::GlobalConstant(atom_id)
        };
        self.name_to_atom.insert(name, atom);
        atom
    }

    /// Get the name corresponding to a particular global AtomId.
    pub fn name_for_global_id(&self, atom_id: AtomId) -> &ConstantName {
        &self.global_constants[atom_id as usize].as_ref().unwrap()
    }

    /// Get the name corresponding to a particular local AtomId.
    pub fn name_for_local_id(&self, atom_id: AtomId) -> &ConstantName {
        &self.local_constants[atom_id as usize].as_ref().unwrap()
    }

    /// Returns the id for the new type.
    pub fn add_type(&mut self, acorn_type: &AcornType) -> TypeId {
        if let Some(type_id) = self.type_to_type_id.get(acorn_type) {
            return *type_id;
        }
        self.type_id_to_type.push(acorn_type.clone());
        let id = (self.type_id_to_type.len() - 1) as TypeId;
        self.type_to_type_id.insert(acorn_type.clone(), id);
        id
    }

    pub fn get_type(&self, type_id: TypeId) -> &AcornType {
        &self.type_id_to_type[type_id as usize]
    }

    /// Make this monomorphized constant an alias for the given name.
    /// If neither of the names map to anything, we create a new entry.
    /// This is rare but can happen if we're aliasing something that was structurally generated.
    pub fn alias_monomorph(
        &mut self,
        c: ConstantInstance,
        name: &ConstantName,
        constant_type: &AcornType,
        local: bool,
    ) {
        let type_id = self.add_type(constant_type);
        let atom = self.add_constant(name.clone(), local);
        self.monomorph_to_id.insert(c, (atom, type_id));
    }

    /// The provided constant instance should be monomorphized.
    pub fn term_from_monomorph(&mut self, c: &ConstantInstance) -> Term {
        let (atom, type_id) = if let Some((atom, type_id)) = self.monomorph_to_id.get(&c) {
            (*atom, *type_id)
        } else {
            // Construct an atom and appropriate entries for this monomorph
            let type_id = self.add_type(&c.instance_type);
            let monomorph_id = self.id_to_monomorph.len() as AtomId;
            let atom = Atom::Monomorph(monomorph_id);
            self.id_to_monomorph.push(c.clone());
            self.monomorph_to_id.insert(c.clone(), (atom, type_id));
            (atom, type_id)
        };

        Term {
            term_type: type_id,
            head_type: type_id,
            head: atom,
            args: vec![],
        }
    }

    pub fn get_monomorph(&self, id: AtomId) -> &ConstantInstance {
        &self.id_to_monomorph[id as usize]
    }
}

#[cfg(test)]
mod tests {
    use crate::term::{BOOL, EMPTY};

    use super::*;

    #[test]
    fn test_type_map_defaults() {
        let map = NormalizationMap::new();
        assert_eq!(map.get_type(EMPTY), &AcornType::Empty);
        assert_eq!(map.get_type(BOOL), &AcornType::Bool);
    }
}
