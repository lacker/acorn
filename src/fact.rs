use std::fmt;

use crate::acorn_type::{AcornType, Class, Typeclass};
use crate::acorn_value::{AcornValue, ConstantInstance};
use crate::names::GlobalName;
use crate::potential_value::PotentialValue;
use crate::proposition::Proposition;
use crate::source::Source;

// A fact is a statement that we are assuming to be true in a particular context.
#[derive(Clone, Debug)]
pub enum Fact {
    // A true statement representable as a boolean value.
    Proposition(Proposition),

    // The fact that this class is an instance of this typeclass.
    Instance(Class, Typeclass, Source),

    /// A defined constant.
    /// The tuple is the name of the constant, the definition, and the source.
    /// Can be generic or not, depending on the potential value.
    Definition(PotentialValue, AcornValue, Source),
}

impl fmt::Display for Fact {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Fact::Proposition(p) => write!(f, "prop: {}", p),
            Fact::Instance(class, typeclass, _) => {
                write!(f, "{} is an instance of {}", class.name, typeclass.name)
            }
            Fact::Definition(name, _, _) => write!(f, "definition: {:?}", name),
        }
    }
}

impl Fact {
    pub fn proposition(value: AcornValue, source: Source) -> Fact {
        Fact::Proposition(Proposition::monomorphic(value, source))
    }

    pub fn source(&self) -> &Source {
        match self {
            Fact::Proposition(p) => &p.source,
            Fact::Instance(_, _, source) => source,
            Fact::Definition(_, _, source) => source,
        }
    }

    pub fn is_instance(&self) -> bool {
        match self {
            Fact::Instance(..) => true,
            _ => false,
        }
    }

    /// Returns Some(..) if this fact is an aliasing for an instance of a typeclass constant.
    /// I.e., it's part of an instance statement with "let _ = _" so that it's an alias of a previously
    /// defined constant.
    pub fn as_instance_alias(&self) -> Option<(ConstantInstance, &GlobalName, AcornType)> {
        if let Fact::Definition(potential, definition, _) = self {
            if let PotentialValue::Resolved(AcornValue::Constant(ci)) = potential {
                if let Some(name) = definition.as_simple_constant() {
                    return Some((ci.clone(), name, definition.get_type()));
                }
            }
        }
        None
    }
}
