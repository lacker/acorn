use std::collections::HashMap;

use crate::acorn_type::AcornType;

use crate::acorn_value::ConstantInstance;
use crate::atom::{Atom, AtomId};
use crate::term::{Term, TypeId};

// The Acorn language allows a rich variety of types, where each value has an AcornType, and where
// functions can be polymorphic.
// The low-level prover only understands simple typing, where each value has a TypeId, and there
// is no polymorphism.
// The TypeMap is a mapping between the two.
#[derive(Clone)]
pub struct TypeMap {
    // type_map[acorn_type] is the TypeId
    type_to_type_id: HashMap<AcornType, TypeId>,

    // types[type_id] is the AcornType
    type_id_to_type: Vec<AcornType>,

    // One entry for each monomorphization.
    // Maps the rich constant to the AtomId of the monomorph.
    monomorph_map: HashMap<ConstantInstance, AtomId>,

    // Indexed by the AtomId of the monomorph.
    // For each monomorphization, store the rich constant corresponding to it, and its type id.
    monomorph_info: Vec<(ConstantInstance, TypeId)>,
}

impl TypeMap {
    pub fn new() -> TypeMap {
        let mut map = TypeMap {
            type_to_type_id: HashMap::new(),
            type_id_to_type: vec![],
            monomorph_info: vec![],
            monomorph_map: HashMap::new(),
        };
        map.add_type(&AcornType::Empty);
        map.add_type(&AcornType::Bool);
        map
    }

    // Returns the id for the new type.
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

    // The provided constant instance should be monomorphized.
    pub fn term_from_monomorph(&mut self, c: &ConstantInstance) -> Term {
        let (monomorph_id, type_id) = if let Some(monomorph_id) = self.monomorph_map.get(&c) {
            let (_, type_id) = self.monomorph_info[*monomorph_id as usize];
            (*monomorph_id, type_id)
        } else {
            // Construct an atom and appropriate entries for this monomorph
            let type_id = self.add_type(&c.instance_type);
            let monomorph_id = self.monomorph_info.len() as AtomId;
            self.monomorph_info.push((c.clone(), type_id));
            self.monomorph_map.insert(c.clone(), monomorph_id);
            (monomorph_id, type_id)
        };

        Term {
            term_type: type_id,
            head_type: type_id,
            head: Atom::Monomorph(monomorph_id),
            args: vec![],
        }
    }

    pub fn get_monomorph(&self, id: AtomId) -> &ConstantInstance {
        &self.monomorph_info[id as usize].0
    }
}

#[cfg(test)]
mod tests {
    use crate::term::{BOOL, EMPTY};

    use super::*;

    #[test]
    fn test_type_map_defaults() {
        let map = TypeMap::new();
        assert_eq!(map.get_type(EMPTY), &AcornType::Empty);
        assert_eq!(map.get_type(BOOL), &AcornType::Bool);
    }
}
