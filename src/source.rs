// A description of where propositions or facts come from in the source code.
// Not just the ability to find it in the text, but also useful metadata and descriptive
// information for human consumption.
use crate::acorn_value::AcornValue;
use crate::module::ModuleId;
use tower_lsp::lsp_types::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceType {
    // An axiom, which may have a name.
    Axiom(Option<String>),

    // A theorem which may have a name.
    Theorem(Option<String>),

    // An anonymous proposition that has previously been proved
    Anonymous,

    // A proposition that comes from the definition of a type.
    // The first string is the type, the second string is a member name.
    TypeDefinition(String, String),

    // A proposition that comes from the definition of a constant.
    // The value is instantiated during monomorphization.
    // The string is the name of the constant. It can be <Type>.<name> for members.
    ConstantDefinition(AcornValue, String),

    // The fact that a type is an instance of a typeclass.
    // Comes from an 'instance' statement.
    // The strings are (type, typeclass).
    Instance(String, String),

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

    // Whether this source can be imported to other modules.
    pub importable: bool,

    // The depth of this source in the module. Zero is top-level.
    pub depth: u32,
}

impl Source {
    pub fn new(
        module: ModuleId,
        range: Range,
        source_type: SourceType,
        importable: bool,
        depth: u32,
    ) -> Source {
        Source {
            module,
            range,
            source_type,
            importable,
            depth,
        }
    }

    pub fn theorem(
        axiomatic: bool,
        module: ModuleId,
        range: Range,
        depth: u32,
        name: Option<String>,
    ) -> Source {
        let source_type = if axiomatic {
            SourceType::Axiom(name)
        } else {
            SourceType::Theorem(name)
        };
        Source {
            module,
            range,
            source_type,
            importable: true,
            depth,
        }
    }

    pub fn anonymous(module: ModuleId, range: Range, depth: u32) -> Source {
        Source {
            module,
            range,
            source_type: SourceType::Anonymous,
            importable: false,
            depth,
        }
    }

    pub fn inhabited(module: ModuleId, type_name: &str, range: Range, depth: u32) -> Source {
        let source_type =
            SourceType::TypeDefinition(type_name.to_string(), "constraint".to_string());
        Source {
            module,
            range,
            source_type,
            importable: true,
            depth,
        }
    }

    pub fn type_definition(
        module: ModuleId,
        range: Range,
        depth: u32,
        type_name: String,
        member_name: String,
    ) -> Source {
        Source {
            module,
            range,
            source_type: SourceType::TypeDefinition(type_name, member_name),
            importable: true,
            depth,
        }
    }

    pub fn constant_definition(
        module: ModuleId,
        range: Range,
        depth: u32,
        constant: AcornValue,
        name: &str,
    ) -> Source {
        Source {
            module,
            range,
            source_type: SourceType::ConstantDefinition(constant, name.to_string()),
            importable: depth == 0,
            depth,
        }
    }

    // A source for instance statements, where an instance relationship is declared.
    pub fn instance(
        module: ModuleId,
        range: Range,
        depth: u32,
        instance_name: &str,
        typeclass_name: &str,
    ) -> Source {
        Source {
            module,
            range,
            source_type: SourceType::Instance(
                instance_name.to_string(),
                typeclass_name.to_string(),
            ),
            importable: true,
            depth,
        }
    }

    pub fn premise(module: ModuleId, range: Range, depth: u32) -> Source {
        Source {
            module,
            range,
            source_type: SourceType::Premise,
            importable: false,
            depth,
        }
    }

    pub fn negated_goal(module: ModuleId, range: Range, depth: u32) -> Source {
        Source {
            module,
            range,
            source_type: SourceType::NegatedGoal,
            importable: false,
            depth,
        }
    }

    pub fn mock() -> Source {
        Source {
            module: 0,
            range: Range::default(),
            source_type: SourceType::Anonymous,
            importable: true,
            depth: 0,
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
            SourceType::TypeDefinition(type_name, _) => format!("the '{}' definition", type_name),
            SourceType::ConstantDefinition(value, _) => format!("the '{}' definition", value),
            SourceType::Instance(instance, tc) => {
                format!("the '{}: {}' relationship", instance, tc)
            }
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
    // This can be a duplicate in some cases, like monomorphization or type definition.
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
            SourceType::TypeDefinition(type_name, member) => {
                Some(format!("{}.{}", type_name, member))
            }
            SourceType::ConstantDefinition(_, name) => Some(name.clone()),
            SourceType::Instance(instance, tc) => Some(format!("{}.{}", instance, tc)),
            SourceType::Premise | SourceType::NegatedGoal => None,
        }
    }

    // The source name with a module id to make it unique.
    pub fn qualified_name(&self) -> Option<(ModuleId, String)> {
        self.name().map(|name| (self.module, name))
    }
}
