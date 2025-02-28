use tower_lsp::lsp_types::Range;

use crate::acorn_value::AcornValue;
use crate::module::ModuleId;

// The different reasons that can lead us to create a proposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceType {
    // An axiom, which may have a name.
    Axiom(Option<String>),

    // A theorem which may have a name.
    Theorem(Option<String>),

    // An anonymous proposition that has previously been proved
    Anonymous,

    // A proposition that is implicit in the definition of a type
    TypeDefinition(String),

    // A proposition that is implicit in the definition of a constant.
    // The value is instantiated during monomorphization.
    // The string is the name of the constant.
    ConstantDefinition(AcornValue, String),

    // A premise for a block that contains the current environment
    Premise,

    // A proposition generated by negating a goal, for the sake of proving it by contradiction
    NegatedGoal,
}

// The information about where a proposition comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    // The module where this value was defined
    pub module: ModuleId,

    // The range in the source document that corresponds to the value's definition
    pub range: Range,

    // How the expression at this location was turned into a proposition
    pub source_type: SourceType,
}

impl Source {
    pub fn mock() -> Source {
        Source {
            module: 0,
            range: Range::default(),
            source_type: SourceType::Anonymous,
        }
    }

    // The line the user sees, starting from 1.
    pub fn user_visible_line(&self) -> u32 {
        self.range.start.line + 1
    }

    // The description is human-readable.
    pub fn description(&self) -> String {
        match &self.source_type {
            SourceType::Axiom(name) => match name {
                Some(name) => format!("the '{}' axiom", name),
                None => "an anonymous axiom".to_string(),
            },
            SourceType::Theorem(name) => match name {
                Some(name) => format!("the '{}' theorem", name),
                None => "an anonymous theorem".to_string(),
            },
            SourceType::Anonymous => format!("line {}", self.user_visible_line()),
            SourceType::TypeDefinition(name) => format!("the '{}' definition", name),
            SourceType::ConstantDefinition(value, _) => format!("the '{}' definition", value),
            SourceType::Premise => "an assumed premise".to_string(),
            SourceType::NegatedGoal => "negating the goal".to_string(),
        }
    }

    pub fn is_axiom(&self) -> bool {
        match self.source_type {
            SourceType::Axiom(_) => true,
            _ => false,
        }
    }

    // The name is an identifier for this source that is somewhat resilient to common edits.
    // We use the line number as the name if there is no other identifier.
    // This is specific to the file it's in; to make it global it needs the fully qualified module name
    // as a prefix.
    // Premises and negated goals do not get names.
    pub fn name(&self) -> Option<String> {
        match &self.source_type {
            SourceType::Axiom(name) | SourceType::Theorem(name) => match name {
                None => Some(self.user_visible_line().to_string()),
                Some(name) => Some(name.clone()),
            },
            SourceType::Anonymous => Some(self.user_visible_line().to_string()),
            SourceType::TypeDefinition(name) => Some(name.clone()),
            SourceType::ConstantDefinition(_, name) => Some(name.clone()),
            SourceType::Premise | SourceType::NegatedGoal => None,
        }
    }
}

// A value along with information on where to find it in the source.
#[derive(Debug, Clone)]
pub struct Proposition {
    // A boolean value. The essence of the proposition is "value is true".
    pub value: AcornValue,

    // Where this proposition came from.
    pub source: Source,
}

impl Proposition {
    pub fn theorem(
        axiomatic: bool,
        value: AcornValue,
        module: ModuleId,
        range: Range,
        name: Option<String>,
    ) -> Proposition {
        let source_type = if axiomatic {
            SourceType::Axiom(name)
        } else {
            SourceType::Theorem(name)
        };
        Proposition {
            value,
            source: Source {
                module,
                range,
                source_type,
            },
        }
    }

    pub fn anonymous(value: AcornValue, module: ModuleId, range: Range) -> Proposition {
        Proposition {
            value,
            source: Source {
                module,
                range,
                source_type: SourceType::Anonymous,
            },
        }
    }

    pub fn type_definition(
        value: AcornValue,
        module: ModuleId,
        range: Range,
        name: String,
    ) -> Proposition {
        Proposition {
            value,
            source: Source {
                module,
                range,
                source_type: SourceType::TypeDefinition(name),
            },
        }
    }

    pub fn constant_definition(
        value: AcornValue,
        module: ModuleId,
        range: Range,
        constant: AcornValue,
        name: &str,
    ) -> Proposition {
        Proposition {
            value,
            source: Source {
                module,
                range,
                source_type: SourceType::ConstantDefinition(constant, name.to_string()),
            },
        }
    }

    pub fn premise(value: AcornValue, module: ModuleId, range: Range) -> Proposition {
        Proposition {
            value,
            source: Source {
                module,
                range,
                source_type: SourceType::Premise,
            },
        }
    }

    pub fn with_negated_goal(&self, value: AcornValue) -> Proposition {
        Proposition {
            value,
            source: Source {
                module: self.source.module,
                range: self.source.range,
                source_type: SourceType::NegatedGoal,
            },
        }
    }

    // Just changes the value while keeping the other stuff intact
    pub fn with_value(&self, value: AcornValue) -> Proposition {
        Proposition {
            value,
            source: self.source.clone(),
        }
    }

    // Theorems and axioms can have names
    pub fn name(&self) -> Option<&str> {
        match &self.source.source_type {
            SourceType::Axiom(name) | SourceType::Theorem(name) => name.as_deref(),
            _ => None,
        }
    }
}
