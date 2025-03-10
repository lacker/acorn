use crate::acorn_type::AcornType;
use crate::acorn_value::AcornValue;
use crate::proof_step::Truthiness;
use crate::proposition::{Proposition, Source, SourceType};

// A fact is a proposition that we already know to be true.
#[derive(Clone, Debug)]
pub struct Fact {
    pub value: AcornValue,
    pub source: Source,
    pub truthiness: Truthiness,
}

impl Fact {
    pub fn new(proposition: Proposition, truthiness: Truthiness) -> Fact {
        Fact {
            value: proposition.value,
            source: proposition.source,
            truthiness,
        }
    }

    pub fn local(&self) -> bool {
        self.truthiness != Truthiness::Factual
    }

    // Instantiates a generic fact.
    pub fn instantiate(&self, params: &[(String, AcornType)]) -> Fact {
        let value = self.value.instantiate(params);
        if value.has_generic() {
            panic!("tried to instantiate but {} is still generic", value);
        }
        let source = match &self.source.source_type {
            SourceType::ConstantDefinition(v, name) => {
                let new_type = SourceType::ConstantDefinition(v.instantiate(params), name.clone());
                Source {
                    module: self.source.module,
                    range: self.source.range.clone(),
                    source_type: new_type,
                }
            }
            _ => self.source.clone(),
        };
        Fact {
            value,
            source,
            truthiness: self.truthiness,
        }
    }
}
