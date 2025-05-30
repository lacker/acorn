use core::panic;
use std::collections::{BTreeMap, HashMap, HashSet};

use tower_lsp::lsp_types::{CompletionItem, CompletionItemKind};

use crate::acorn_type::{AcornType, Class, PotentialType, TypeParam, Typeclass, UnresolvedType};
use crate::acorn_value::AcornValue;
use crate::code_generator::CodeGenerator;
use crate::compilation::{self, ErrorSource, PanicOnError};
use crate::evaluator::Evaluator;
use crate::expression::{Declaration, Expression, TypeParamExpr};
use crate::module::{Module, ModuleId};
use crate::named_entity::NamedEntity;
use crate::names::{ConstantName, DefinedName, InstanceName};
use crate::potential_value::PotentialValue;
use crate::project::Project;
use crate::proposition::Proposition;
use crate::stack::Stack;
use crate::termination_checker::TerminationChecker;
use crate::token::{self, Token};
use crate::type_unifier::{TypeUnifier, TypeclassRegistry};
use crate::unresolved_constant::UnresolvedConstant;

/// The BindingMap contains all of the mappings needed to figure out what a string refers to
/// in a particular environment.
/// Variables, constants, types, typeclasses, modules, numeric literals.
/// This representation does not have to be efficient enough to run in the inner loop of the prover.
#[derive(Clone)]
pub struct BindingMap {
    /// The module all these names are in.
    module_id: ModuleId,

    /// Maps the name of a constant defined in this scope to information about it.
    /// Doesn't handle variables defined on the stack, only ones that will be in scope for the
    /// entirety of this environment.
    /// This also includes aliases.
    constant_info: HashMap<ConstantName, ConstantInfo>,

    /// Maps the name of a type to the type object.
    /// Includes unresolved names like List that don't have enough information
    /// to get a specific type.
    typename_to_type: BTreeMap<String, PotentialType>,

    /// Maps the type object to its name in this environment.
    type_to_typename: HashMap<PotentialType, String>,

    /// Stores information about every class accessible from this module.
    class_info: HashMap<Class, ClassInfo>,

    /// Maps the name of a typeclass to the typeclass.
    /// Includes typeclasses that were imported from other modules.
    name_to_typeclass: BTreeMap<String, Typeclass>,

    /// Stores information about every typeclass accessible from this module.
    typeclass_info: HashMap<Typeclass, TypeclassInfo>,

    /// A map whose keys are the unqualified constants in this module.
    /// Used for completion.
    unqualified: BTreeMap<String, ()>,

    /// Whenever a name from some other scope has a local alias in this one,
    /// if we're generating code, we prefer to use the local name.
    /// This contains constants, types, and typenames.
    /// For this reason, canonical_to_alias maps the global name to the preferred local alias.
    constant_to_alias: HashMap<ConstantName, String>,

    /// Names that refer to other modules.
    /// After "import foo", "foo" refers to a module.
    /// It's important that these are in alphabetical order, so that dependency hashes are consistent.
    name_to_module: BTreeMap<String, ModuleId>,

    /// The local name for imported modules.
    module_to_name: HashMap<ModuleId, String>,

    /// The default data type to use for numeric literals.
    numerals: Option<Class>,

    /// The definitions of the instance attributes defined in this module.
    /// Alias-type definitions are stored here just like anything else, because the monomorphizer
    /// is going to need to see them in their parametrized form.
    instance_definitions: HashMap<InstanceName, AcornValue>,
}

impl BindingMap {
    pub fn new(module: ModuleId) -> Self {
        assert!(module >= Module::FIRST_NORMAL);
        let mut answer = BindingMap {
            module_id: module,
            constant_info: HashMap::new(),
            typename_to_type: BTreeMap::new(),
            type_to_typename: HashMap::new(),
            name_to_typeclass: BTreeMap::new(),
            typeclass_info: HashMap::new(),
            unqualified: BTreeMap::new(),
            constant_to_alias: HashMap::new(),
            name_to_module: BTreeMap::new(),
            module_to_name: HashMap::new(),
            numerals: None,
            instance_definitions: HashMap::new(),
            class_info: HashMap::new(),
        };
        answer.add_type_alias("Bool", PotentialType::Resolved(AcornType::Bool));
        answer
    }

    pub fn module_id(&self) -> ModuleId {
        self.module_id
    }

    /// Returns the default data type for numeric literals, if set.
    pub fn numerals(&self) -> Option<&Class> {
        self.numerals.as_ref()
    }

    /// Whether this type has this attribute in the current context.
    pub fn has_type_attribute(&self, class: &Class, var_name: &str) -> bool {
        self.class_info
            .get(class)
            .map_or(false, |info| info.attributes.contains_key(var_name))
    }

    /// For a given typeclass attribute, find the typeclass that defines it.
    /// This can return the typeclass argument itself, or a base typeclass that it extends.
    /// Returns None if there is no such attribute.
    pub fn typeclass_attribute_lookup(
        &self,
        typeclass: &Typeclass,
        attr: &str,
    ) -> Option<&Typeclass> {
        self.typeclass_info.get(typeclass)?.attributes.get(attr)
    }

    /// Gets the local alias to use for a given constant.
    pub fn constant_alias(&self, name: &ConstantName) -> Option<&String> {
        self.constant_to_alias.get(name)
    }

    /// Gets the local alias to use for a given class.
    pub fn class_alias(&self, class: &Class) -> Option<&String> {
        self.class_info.get(class)?.alias.as_ref()
    }

    fn add_class_alias(&mut self, class: &Class, alias: &str) {
        if !self.class_info.contains_key(class) {
            self.class_info.insert(class.clone(), ClassInfo::new());
        }
        let info = self.class_info.get_mut(class).unwrap();
        if info.alias.is_none() {
            info.alias = Some(alias.to_string());
        }
    }

    fn add_typeclass_alias(&mut self, typeclass: &Typeclass, alias: &str) {
        if !self.typeclass_info.contains_key(typeclass) {
            self.typeclass_info
                .insert(typeclass.clone(), TypeclassInfo::new());
        }
        let info = self.typeclass_info.get_mut(typeclass).unwrap();
        if info.alias.is_none() {
            info.alias = Some(alias.to_string());
        }
    }

    /// Gets the local alias to use for a given typeclass.
    pub fn typeclass_alias(&self, typeclass: &Typeclass) -> Option<&String> {
        self.typeclass_info.get(typeclass)?.alias.as_ref()
    }

    pub fn check_defined_name_available(
        &self,
        defined_name: &DefinedName,
        source: &dyn ErrorSource,
    ) -> compilation::Result<()> {
        if self.constant_name_in_use(defined_name) {
            return Err(source.error(&format!("constant name {} is already in use", defined_name)));
        }
        Ok(())
    }

    /// Note: Doesn't work correctly for typeclass attributes.
    pub fn constant_name_in_use(&self, name: &DefinedName) -> bool {
        match name {
            DefinedName::Constant(constant_name) => {
                if self.constant_info.contains_key(constant_name) {
                    return true;
                }
                match constant_name {
                    ConstantName::Unqualified(_, name) => {
                        self.unqualified.contains_key(name)
                            || self.name_to_module.contains_key(name)
                    }
                    ConstantName::ClassAttribute(class, attr) => {
                        self.has_type_attribute(class, attr)
                    }
                    ConstantName::TypeclassAttribute(..) => {
                        // This doesn't seem right!
                        false
                    }
                }
            }
            DefinedName::Instance(instance_name) => {
                self.instance_definitions.contains_key(instance_name)
            }
        }
    }

    /// Get the set of all typeclasses that this one extends.
    /// This is the transitive closure, ie when A extends B and B extends C, A's set
    /// will include both B and C.
    pub fn get_extends_set(&self, typeclass: &Typeclass) -> Option<&HashSet<Typeclass>> {
        Some(&self.typeclass_info.get(typeclass)?.extends)
    }

    pub fn unifier(&self) -> TypeUnifier {
        TypeUnifier::new(self)
    }

    /// Returns a PotentialValue representing this name, if there is one.
    /// This can be either a resolved or unresolved value.
    /// This function assumes that you are calling the correct binding map.
    pub fn get_constant_value(
        &self,
        name: &DefinedName,
        source: &dyn ErrorSource,
    ) -> compilation::Result<PotentialValue> {
        match name {
            DefinedName::Constant(constant_name) => match self.constant_info.get(constant_name) {
                Some(info) => Ok(info.value.clone()),
                None => Err(source.error(&format!("local constant {} not found", name))),
            },
            DefinedName::Instance(instance_name) => {
                let definition = self
                    .instance_definitions
                    .get(instance_name)
                    .ok_or_else(|| {
                        source.error(&format!("instance constant {} not found", name))
                    })?;
                let value =
                    AcornValue::instance_constant(instance_name.clone(), definition.get_type());
                Ok(PotentialValue::Resolved(value))
            }
        }
    }

    /// Gets the type for a type name, not for an identifier.
    pub fn get_type_for_typename(&self, type_name: &str) -> Option<&PotentialType> {
        self.typename_to_type.get(type_name)
    }

    pub fn get_typename_for_type(&self, potential_type: &PotentialType) -> Option<&String> {
        self.type_to_typename.get(potential_type)
    }

    pub fn has_typename(&self, type_name: &str) -> bool {
        self.typename_to_type.contains_key(type_name)
    }

    pub fn get_typeclass_for_name(&self, typeclass_name: &str) -> Option<&Typeclass> {
        self.name_to_typeclass.get(typeclass_name)
    }

    pub fn has_typeclass_name(&self, typeclass_name: &str) -> bool {
        self.name_to_typeclass.contains_key(typeclass_name)
    }

    pub fn get_module_id_for_name(&self, name: &str) -> Option<ModuleId> {
        self.name_to_module.get(name).copied()
    }

    pub fn get_name_for_module_id(&self, module_id: ModuleId) -> Option<&String> {
        self.module_to_name.get(&module_id)
    }

    /// Just use this for testing.
    pub fn has_constant_name(&self, name: &str) -> bool {
        let constant_name = ConstantName::unqualified(self.module_id, name);
        self.constant_info.contains_key(&constant_name)
    }

    /// Returns the defined value, if there is a defined value.
    /// If there isn't, returns None.
    pub fn get_definition(&self, name: &DefinedName) -> Option<&AcornValue> {
        match name {
            DefinedName::Constant(constant_name) => {
                self.constant_info.get(constant_name)?.definition.as_ref()
            }
            DefinedName::Instance(instance_name) => self.instance_definitions.get(instance_name),
        }
    }

    /// Returns the defined value and its parameters in their canonical order.
    /// Returns None if there is no definition.
    pub fn get_definition_and_params(
        &self,
        constant_name: &ConstantName,
    ) -> Option<(&AcornValue, &[TypeParam])> {
        let info = self.constant_info.get(constant_name)?;
        Some((info.definition.as_ref()?, info.value.unresolved_params()))
    }

    // Get a set of all the typeclasses that this typeclass extends.
    pub fn get_extends(&self, typeclass: &Typeclass) -> impl Iterator<Item = &Typeclass> {
        self.typeclass_info
            .get(typeclass)
            .into_iter()
            .flat_map(|info| info.extends.iter())
    }

    /// A helper to get the bindings from the project if needed bindings.
    pub fn get_bindings<'a>(&'a self, module_id: ModuleId, project: &'a Project) -> &'a BindingMap {
        if module_id == self.module_id {
            self
        } else {
            project.get_bindings(module_id).unwrap()
        }
    }

    pub fn get_typeclass_attributes<'a>(
        &'a self,
        typeclass: &Typeclass,
        project: &'a Project,
    ) -> &'a BTreeMap<String, Typeclass> {
        &self
            .get_bindings(typeclass.module_id, project)
            .typeclass_info
            .get(&typeclass)
            .unwrap()
            .attributes
    }

    pub fn get_constructor_info(&self, name: &ConstantName) -> Option<&ConstructorInfo> {
        self.constant_info
            .get(name)
            .and_then(|info| info.constructor.as_ref())
    }

    pub fn get_module_for_class_attr(&self, class: &Class, attr: &str) -> Option<ModuleId> {
        self.class_info
            .get(class)
            .and_then(|info| info.attributes.get(attr))
            .copied()
    }

    /// Checks against names for both types and typeclasses because they can conflict.
    pub fn check_typename_available(
        &self,
        name: &str,
        source: &dyn ErrorSource,
    ) -> compilation::Result<()> {
        if self.typename_to_type.contains_key(name) || self.name_to_typeclass.contains_key(name) {
            return Err(source.error(&format!("typename {} is already in use", name)));
        }
        Ok(())
    }

    /// Returns an error if this name is already in use.
    pub fn check_unqualified_name_available(
        &self,
        name: &str,
        source: &dyn ErrorSource,
    ) -> compilation::Result<()> {
        let defined_name = DefinedName::unqualified(self.module_id, name);
        self.check_defined_name_available(&defined_name, source)
    }

    /// We use variables named x0, x1, x2, etc when new temporary variables are needed.
    /// Find the next one that's available.
    /// 'x' is the prefix here.
    pub fn next_indexed_var(&self, prefix: char, next_index: &mut u32) -> String {
        loop {
            let name =
                DefinedName::unqualified(self.module_id, &format!("{}{}", prefix, next_index));
            *next_index += 1;
            if !self.constant_name_in_use(&name) {
                return name.to_string();
            }
        }
    }

    /// Adds both directions for a name iff type correspondence.
    /// Panics if the name is already bound.
    fn insert_type_name(&mut self, name: String, potential_type: PotentialType) {
        // There can be multiple names for a type.
        // If we already have a name for the reverse lookup, we don't overwrite it.
        if !self.type_to_typename.contains_key(&potential_type) {
            self.type_to_typename
                .insert(potential_type.clone(), name.clone());
        }

        match self.typename_to_type.entry(name) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(potential_type);
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                panic!("typename {} already bound", entry.key());
            }
        }
    }

    /// Adds a new data type to the binding map.
    /// Panics if the name is already bound.
    pub fn add_data_type(&mut self, name: &str) -> AcornType {
        let class = Class {
            module_id: self.module_id,
            name: name.to_string(),
        };
        let t = AcornType::Data(class, vec![]);
        self.insert_type_name(name.to_string(), PotentialType::Resolved(t.clone()));
        t
    }

    /// Panics if the name is already bound.
    pub fn add_potential_type(
        &mut self,
        name: &str,
        params: Vec<Option<Typeclass>>,
    ) -> PotentialType {
        if params.len() == 0 {
            return PotentialType::Resolved(self.add_data_type(name));
        }
        let class = Class {
            module_id: self.module_id,
            name: name.to_string(),
        };
        let ut = UnresolvedType { class, params };
        let potential = PotentialType::Unresolved(ut);
        self.insert_type_name(name.to_string(), potential.clone());
        potential
    }

    /// Adds an arbitrary type to the binding map.
    /// This indicates a type parameter that is coming into scope.
    /// Panics if the param name is already bound.
    pub fn add_arbitrary_type(&mut self, param: TypeParam) -> AcornType {
        let name = param.name.to_string();
        let arbitrary_type = AcornType::Arbitrary(param);
        let potential = PotentialType::Resolved(arbitrary_type.clone());
        self.insert_type_name(name, potential);
        arbitrary_type
    }

    /// Adds a new type name that's an alias for an existing type.
    /// Bindings are the bindings that we are importing the type from.
    /// If the alias is a local one, bindings is None.
    /// Panics if the alias is already bound.
    pub fn add_type_alias(&mut self, alias: &str, potential: PotentialType) {
        // Local type aliases for concrete types should be preferred.
        if let PotentialType::Resolved(AcornType::Data(class, params)) = &potential {
            if params.is_empty() {
                self.add_class_alias(class, alias);
            }
        }

        // Local type aliases should also be preferred to the canonical name for
        // unresolved generic types.
        if let PotentialType::Unresolved(u) = &potential {
            self.add_class_alias(&u.class, alias);
        }

        self.insert_type_name(alias.to_string(), potential);
    }

    /// Adds a newly-defined typeclass to this environment.
    pub fn add_typeclass(
        &mut self,
        name: &str,
        extends: Vec<Typeclass>,
        project: &Project,
        source: &dyn ErrorSource,
    ) -> compilation::Result<()> {
        let mut info = TypeclassInfo::new();
        for base in extends {
            info.extends.insert(base.clone());
            let bindings = self.get_bindings(base.module_id, project);
            let base_info = bindings.typeclass_info.get(&base).unwrap();
            for base_base in &base_info.extends {
                if !info.extends.contains(base_base) {
                    info.extends.insert(base_base.clone());
                }
            }
            for (attr, original) in base_info.attributes.iter() {
                if let Some(current) = info.attributes.get(attr) {
                    if current != original {
                        return Err(source.error(&format!(
                            "you cannot extend both '{}' and '{}' because they both define the attribute '{}'",
                            &current.name, &original.name, attr
                        )));
                    }
                } else {
                    info.attributes.insert(attr.clone(), original.clone());
                }
            }
        }

        let typeclass = Typeclass {
            module_id: self.module_id,
            name: name.to_string(),
        };
        match self.typeclass_info.entry(typeclass.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(info);
            }
            std::collections::hash_map::Entry::Occupied(entry) => {
                return Err(
                    source.error(&format!("typeclass {} is already bound", entry.key().name))
                );
            }
        }
        self.add_typeclass_name(&name, typeclass);
        Ok(())
    }

    /// Adds a local name for this typeclass.
    /// Panics if the name is already bound - that should be checked before calling this.
    fn add_typeclass_name(&mut self, name: &str, typeclass: Typeclass) {
        self.add_typeclass_alias(&typeclass, name);

        match self.name_to_typeclass.entry(name.to_string()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(typeclass);
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                panic!("typeclass name {} is already bound", entry.key());
            }
        }
    }

    /// Call this after an instance attribute has been defined to typecheck it.
    /// Returns (resolved typeclass attribute, defined instance attribute).
    /// The resolved typeclass attribute is like
    /// Ring.add<Int>
    /// and the defined instance attribute is the one that we defined, before
    /// proving that Int was actually a Ring.
    pub fn check_instance_attribute(
        &self,
        instance_type: &AcornType,
        typeclass: &Typeclass,
        attr_name: &str,
        project: &Project,
        source: &dyn ErrorSource,
    ) -> compilation::Result<(AcornValue, AcornValue)> {
        // Get the relevant properties of the typeclass.
        let typeclass_attr_name = DefinedName::typeclass_attr(typeclass, attr_name);
        let typeclass_attr = self
            .get_bindings(typeclass.module_id, &project)
            .get_constant_value(&typeclass_attr_name, source)?;
        let uc = typeclass_attr.as_unresolved(source)?;
        let resolved_attr = uc.resolve(source, vec![instance_type.clone()])?;
        let resolved_attr_type = resolved_attr.get_type();

        // Get the relevant properties of the instance class.
        let instance_class = instance_type.get_class(source)?;
        let instance_attr_name =
            DefinedName::instance(typeclass.clone(), attr_name, instance_class.clone());
        let instance_attr = self.get_constant_value(&instance_attr_name, source)?;
        let instance_attr = instance_attr.as_value(source)?;
        let instance_attr_type = instance_attr.get_type();
        if instance_attr_type != resolved_attr_type {
            return Err(source.error(&format!(
                "type mismatch for attribute '{}': expected {}, found {}",
                attr_name, resolved_attr_type, instance_attr_type
            )));
        }
        Ok((resolved_attr, instance_attr))
    }

    pub fn add_instance_of(&mut self, class: Class, typeclass: Typeclass) {
        self.class_info
            .entry(class)
            .or_insert_with(ClassInfo::new)
            .typeclasses
            .insert(typeclass, self.module_id);
    }

    /// All other modules that we directly depend on, besides this one.
    /// Sorted by the name of the import, so that the order will be consistent.
    pub fn direct_dependencies(&self) -> Vec<ModuleId> {
        self.name_to_module.values().copied().collect()
    }

    pub fn set_numerals(&mut self, class: Class) {
        self.numerals = Some(class);
    }

    /// Adds a definition for a name that can either be a normal constant, or an instance.
    /// Panics if the name is already bound.
    /// The type and definition can be generic. If so, the parameters must be listed in params.
    /// Don't call this for aliases, this doesn't handle aliases intelligently.
    /// Returns the value for the newly created constant.
    pub fn add_defined_name(
        &mut self,
        defined_name: &DefinedName,
        params: Vec<TypeParam>,
        constant_type: AcornType,
        definition: Option<AcornValue>,
        constructor: Option<ConstructorInfo>,
    ) -> PotentialValue {
        match defined_name {
            DefinedName::Constant(constant_name) => self.add_constant_name(
                constant_name,
                params,
                constant_type,
                definition,
                constructor,
            ),
            DefinedName::Instance(instance_name) => {
                let definition = definition.expect("instance must have a definition");
                if !params.is_empty() {
                    panic!("instance may not have parameters");
                }
                if constructor.is_some() {
                    panic!("instance may not be a constructor");
                }
                self.instance_definitions
                    .insert(instance_name.clone(), definition);
                let value = AcornValue::instance_constant(instance_name.clone(), constant_type);
                PotentialValue::Resolved(value)
            }
        }
    }

    /// Adds a constant that is an attribute of a class.
    pub fn add_class_attribute(
        &mut self,
        class: &Class,
        attr: &str,
        params: Vec<TypeParam>,
        constant_type: AcornType,
        definition: Option<AcornValue>,
        constructor: Option<ConstructorInfo>,
    ) -> PotentialValue {
        let constant_name = ConstantName::class_attr(class.clone(), attr);
        self.add_constant_name(
            &constant_name,
            params,
            constant_type,
            definition,
            constructor,
        )
    }

    pub fn add_typeclass_attribute(
        &mut self,
        typeclass: &Typeclass,
        attr: &str,
        params: Vec<TypeParam>,
        constant_type: AcornType,
        definition: Option<AcornValue>,
        constructor: Option<ConstructorInfo>,
    ) -> PotentialValue {
        let constant_name = ConstantName::typeclass_attr(typeclass.clone(), attr);
        self.add_constant_name(
            &constant_name,
            params,
            constant_type,
            definition,
            constructor,
        )
    }

    /// Adds a constant that is not an attribute of anything.
    pub fn add_unqualified_constant(
        &mut self,
        name: &str,
        params: Vec<TypeParam>,
        constant_type: AcornType,
        definition: Option<AcornValue>,
        constructor: Option<ConstructorInfo>,
    ) -> PotentialValue {
        let constant_name = ConstantName::unqualified(self.module_id, name);
        self.add_constant_name(
            &constant_name,
            params,
            constant_type,
            definition,
            constructor,
        )
    }

    /// Adds a definition for a constant.
    /// Panics if the name is already bound.
    /// The type and definition can be generic. If so, the parameters must be listed in params.
    /// Don't call this for aliases, this doesn't handle aliases intelligently.
    /// Returns the value for the newly created constant.
    fn add_constant_name(
        &mut self,
        constant_name: &ConstantName,
        params: Vec<TypeParam>,
        constant_type: AcornType,
        definition: Option<AcornValue>,
        constructor: Option<ConstructorInfo>,
    ) -> PotentialValue {
        if let Some(definition) = &definition {
            if let Err(e) = definition.validate() {
                panic!("invalid definition for constant {}: {}", constant_name, e);
            }
            if params.is_empty() && definition.has_generic() {
                panic!("there should not be generic types in non-parametrized definitions");
            }
            if !params.is_empty() && definition.has_arbitrary() {
                panic!("there should not be arbitrary types in parametrized definitions");
            }
        }

        let value = if params.is_empty() {
            if constant_type.has_generic() {
                panic!("there should not be generic types in non-parametrized constant types");
            }
            PotentialValue::Resolved(AcornValue::constant(
                constant_name.clone(),
                vec![],
                constant_type,
            ))
        } else {
            if constant_type.has_arbitrary() {
                panic!("there should not be arbitrary types in parametrized constant types");
            }
            PotentialValue::Unresolved(UnresolvedConstant {
                name: constant_name.clone(),
                params,
                generic_type: constant_type,
            })
        };

        // New constants start out not being theorems and are marked as a theorem later.
        let info = ConstantInfo {
            value: value.clone(),
            canonical: true,
            definition,
            theorem: false,
            constructor,
        };

        self.add_constant_info(constant_name.clone(), info);
        value
    }

    /// Adds information for either a newly defined constant, or an alias.
    fn add_constant_info(&mut self, constant_name: ConstantName, info: ConstantInfo) {
        match &constant_name {
            ConstantName::ClassAttribute(class, attribute) => {
                // We are defining a new class attribute.
                self.class_info
                    .entry(class.clone())
                    .or_insert_with(ClassInfo::new)
                    .attributes
                    .insert(attribute.clone(), self.module_id);
            }
            ConstantName::TypeclassAttribute(typeclass, attribute) => {
                self.typeclass_info
                    .entry(typeclass.clone())
                    .or_insert_with(TypeclassInfo::new)
                    .attributes
                    .insert(attribute.clone(), typeclass.clone());
            }
            ConstantName::Unqualified(_, name) => {
                self.unqualified.insert(name.clone(), ());
            }
        }

        self.constant_info.insert(constant_name, info);
    }

    /// Be really careful about this, it seems likely to break things.
    fn remove_constant(&mut self, constant_name: &ConstantName) {
        if let ConstantName::Unqualified(_, word) = constant_name {
            // Remove the unqualified name from the list of unqualified names.
            self.unqualified.remove(word);
        }
        self.constant_info
            .remove(constant_name)
            .expect("constant name not in use");
    }

    /// Adds a local alias for an already-existing constant value.
    /// TODO: is aliasing theorems supposed to work?
    pub fn add_constant_alias(
        &mut self,
        alias: ConstantName,
        canonical: ConstantName,
        value: PotentialValue,
    ) {
        if canonical.module_id() != self.module_id {
            // Prefer this alias locally to using the qualified, canonical name
            self.constant_to_alias
                .entry(canonical)
                .or_insert(alias.to_string());
        }
        let info = ConstantInfo {
            value,
            canonical: false,
            theorem: false,
            definition: None,
            constructor: None,
        };
        self.add_constant_info(alias, info);
    }

    pub fn mark_as_theorem(&mut self, name: &ConstantName) {
        self.constant_info
            .get_mut(name)
            .expect("marking theorem that doesn't exist")
            .theorem = true;
    }

    pub fn is_theorem(&self, name: &ConstantName) -> bool {
        self.constant_info
            .get(name)
            .map_or(false, |info| info.theorem)
    }

    /// Type variables and arbitrary variables should get removed when they go out of scope.
    /// Other sorts of types shouldn't be getting removed.
    pub fn remove_type(&mut self, name: &str) {
        match self.typename_to_type.remove(name) {
            Some(p) => match &p {
                PotentialType::Unresolved(ut) => {
                    panic!("removing type {} which is unresolved", ut.class.name);
                }
                PotentialType::Resolved(t) => {
                    match &t {
                        AcornType::Variable(..) | AcornType::Arbitrary(..) => {}
                        _ => panic!("unexpectedly removing type: {}", name),
                    }
                    self.type_to_typename.remove(&p);
                }
            },
            None => panic!("removing type {} which is already not present", name),
        }
    }

    /// Adds this name to the environment as a module.
    /// Copies over all the class_info from the module's bindings.
    /// This enables "mixins".
    /// Also copy over all the typeclass_info from the module's bindings.
    pub fn import_module(
        &mut self,
        name: &str,
        bindings: &BindingMap,
        source: &dyn ErrorSource,
    ) -> compilation::Result<()> {
        if self
            .name_to_module
            .insert(name.to_string(), bindings.module_id)
            .is_some()
        {
            return Err(source.error(&format!("name {} is already bound", name)));
        }
        self.module_to_name
            .insert(bindings.module_id, name.to_string());

        // Copy over the class info.
        for (class, imported_info) in bindings.class_info.iter() {
            let entry = self
                .class_info
                .entry(class.clone())
                .or_insert_with(ClassInfo::new);
            entry.import(imported_info, &class.name, source)?;
        }

        // Copy over the typeclass info, but drop any aliases.
        for (typeclass, imported_info) in bindings.typeclass_info.iter() {
            if !self.typeclass_info.contains_key(typeclass) {
                let mut imported_info = imported_info.clone();
                imported_info.alias = None;
                self.typeclass_info.insert(typeclass.clone(), imported_info);
            }
        }
        Ok(())
    }

    pub fn is_module(&self, name: &str) -> bool {
        self.name_to_module.contains_key(name)
    }

    /// Whether this value is calling a theorem on some arguments.
    pub fn is_citation(&self, claim: &AcornValue, project: &Project) -> bool {
        match claim.is_named_function_call() {
            Some(constant_name) => {
                let bindings = self.get_bindings(constant_name.module_id(), project);
                bindings.is_theorem(constant_name)
            }
            None => false,
        }
    }

    fn get_typeclass_attribute_completions(
        &self,
        typeclass: &Typeclass,
        prefix: &str,
        project: &Project,
    ) -> Option<Vec<CompletionItem>> {
        let mut answer = vec![];
        if let Some(info) = self
            .get_bindings(typeclass.module_id, project)
            .typeclass_info
            .get(typeclass)
        {
            for key in keys_with_prefix(&info.attributes, &prefix) {
                let completion = CompletionItem {
                    label: key.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    ..Default::default()
                };
                answer.push(completion);
            }
        }
        Some(answer)
    }

    /// Gets completions when we are typing a member name.
    fn get_type_attribute_completions(
        &self,
        t: &AcornType,
        prefix: &str,
    ) -> Option<Vec<CompletionItem>> {
        if let AcornType::Data(class, _) = t {
            let info = self.class_info.get(class)?;
            let mut answer = vec![];
            for key in keys_with_prefix(&info.attributes, prefix) {
                let completion = CompletionItem {
                    label: key.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    ..Default::default()
                };
                answer.push(completion);
            }
            Some(answer)
        } else {
            None
        }
    }

    /// The prefix is just of a single identifier.
    /// If importing is true, we are looking for names to import. This means that we don't
    /// want to suggest names unless this is the canonical location for them, and we don't
    /// want to suggest theorems.
    pub fn get_completions(
        &self,
        prefix: &str,
        importing: bool,
        project: &Project,
    ) -> Option<Vec<CompletionItem>> {
        if prefix.contains('.') {
            if importing {
                // Syntactically invalid
                return None;
            }
            let mut name_chain = prefix.split('.').collect::<Vec<&str>>();
            let partial = name_chain.pop()?;
            let namespace = Evaluator::new(self, project).evaluate_name_chain(&name_chain)?;
            match namespace {
                NamedEntity::Module(module) => {
                    let bindings = project.get_bindings(module)?;
                    return bindings.get_completions(partial, true, project);
                }
                NamedEntity::Type(t) => {
                    return self.get_type_attribute_completions(&t, partial);
                }
                NamedEntity::Value(v) => {
                    let t = v.get_type();
                    return self.get_type_attribute_completions(&t, partial);
                }
                NamedEntity::Typeclass(tc) => {
                    return self.get_typeclass_attribute_completions(&tc, partial, project);
                }
                NamedEntity::UnresolvedValue(u) => {
                    return self.get_type_attribute_completions(&u.generic_type, partial);
                }
                NamedEntity::UnresolvedType(ut) => {
                    let display_type = ut.to_display_type();
                    return self.get_type_attribute_completions(&display_type, partial);
                }
            }
        }

        let first_char = prefix.chars().next();
        let mut answer = vec![];

        if first_char.map(|c| c.is_lowercase()).unwrap_or(true) {
            // Keywords
            if !importing {
                for key in token::keywords_with_prefix(prefix) {
                    let completion = CompletionItem {
                        label: key.to_string(),
                        kind: Some(CompletionItemKind::KEYWORD),
                        ..Default::default()
                    };
                    answer.push(completion);
                }
            }
            // Constants
            for key in keys_with_prefix(&self.unqualified, prefix) {
                if key.contains('.') {
                    continue;
                }
                let constant_name = ConstantName::unqualified(self.module_id, key);
                if importing {
                    match self.constant_info.get(&constant_name) {
                        Some(info) => {
                            if !info.canonical || info.theorem {
                                // Don't suggest aliases or theorems when importing
                                continue;
                            }
                        }
                        None => continue,
                    }
                }
                let completion = CompletionItem {
                    label: key.clone(),
                    kind: Some(CompletionItemKind::CONSTANT),
                    ..Default::default()
                };
                answer.push(completion);
            }
        }

        if first_char.map(|c| c.is_uppercase()).unwrap_or(true) {
            // Types
            for key in keys_with_prefix(&self.typename_to_type, prefix) {
                if importing {
                    let data_type = self.typename_to_type.get(key)?;
                    match data_type {
                        PotentialType::Resolved(AcornType::Data(class, _)) => {
                            if class.module_id != self.module_id || &class.name != key {
                                continue;
                            }
                        }
                        _ => continue,
                    }
                }
                let completion = CompletionItem {
                    label: key.clone(),
                    kind: Some(CompletionItemKind::CLASS),
                    ..Default::default()
                };
                answer.push(completion);
            }
        }

        Some(answer)
    }

    /// Imports a name from another module.
    /// The name could either be a type or a value.
    pub fn import_name(
        &mut self,
        project: &Project,
        module: ModuleId,
        name_token: &Token,
    ) -> compilation::Result<()> {
        // Check if this name is lowercase
        let name = name_token.text();
        if name.chars().next().map(char::is_lowercase).unwrap_or(false) {
            let defined_name = DefinedName::unqualified(module, name);
            self.check_defined_name_available(&defined_name, name_token)?;
        }

        let bindings = match project.get_bindings(module) {
            Some(b) => b,
            None => {
                return Err(
                    name_token.error(&format!("could not load bindings for imported module"))
                )
            }
        };
        let entity =
            Evaluator::new(bindings, project).evaluate_name(name_token, &Stack::new(), None)?;

        match entity {
            NamedEntity::Value(value) => {
                // Add a local alias that mirrors this constant's name in the imported module.
                if let Some(constant_name) = value.as_simple_constant() {
                    self.add_constant_alias(
                        ConstantName::unqualified(self.module_id, name),
                        constant_name.clone(),
                        PotentialValue::Resolved(value),
                    );
                    Ok(())
                } else {
                    // I don't see how this branch can be reached.
                    return Err(name_token.error("cannot import non-constant values"));
                }
            }
            NamedEntity::Type(acorn_type) => {
                self.add_type_alias(&name, PotentialType::Resolved(acorn_type));
                Ok(())
            }
            NamedEntity::Module(_) => Err(name_token.error("cannot import modules indirectly")),
            NamedEntity::Typeclass(tc) => {
                self.add_typeclass_name(&name, tc);
                Ok(())
            }

            NamedEntity::UnresolvedValue(uc) => {
                self.add_constant_alias(
                    ConstantName::unqualified(self.module_id, name),
                    uc.name.clone(),
                    PotentialValue::Unresolved(uc),
                );
                Ok(())
            }

            NamedEntity::UnresolvedType(u) => {
                self.add_type_alias(&name, PotentialType::Unresolved(u));
                Ok(())
            }
        }
    }

    /// Apply a potential value to arguments, inferring the types.
    pub fn apply_potential(
        &self,
        potential: PotentialValue,
        args: Vec<AcornValue>,
        expected_type: Option<&AcornType>,
        source: &dyn ErrorSource,
    ) -> compilation::Result<AcornValue> {
        let value = match potential {
            PotentialValue::Resolved(f) => f.check_apply(args, expected_type, source)?,
            PotentialValue::Unresolved(u) => {
                self.unifier()
                    .resolve_with_inference(u, args, expected_type, source)?
            }
        };
        Ok(value)
    }

    /// Apply an unresolved name to arguments, inferring the types.
    pub fn infer_and_apply(
        &self,
        stack: &mut Stack,
        unresolved: UnresolvedConstant,
        arg_exprs: Vec<&Expression>,
        expected_type: Option<&AcornType>,
        project: &Project,
        source: &dyn ErrorSource,
    ) -> compilation::Result<AcornValue> {
        // Evaluate the arguments
        let mut args = vec![];
        let mut evaluator = Evaluator::new(self, project);
        for arg_expr in &arg_exprs {
            let arg = evaluator.evaluate_value_with_stack(stack, arg_expr, None)?;
            args.push(arg);
        }

        self.unifier()
            .resolve_with_inference(unresolved, args, expected_type, source)
    }

    /// This creates a version of a typeclass condition that is specialized to a particular
    /// class that isn't an instance of the typeclass.
    /// The *class* must be defined in this module. The typeclass may not be.
    ///
    /// We use this when we haven't proven that a type is an instance of a typeclass yet.
    /// So for example we can resolve:
    ///   Ring.add<T: Ring> -> Ring.add<Int>
    /// even though Int has not been proven to be an instance of Ring.
    ///
    /// TODO: does this work right for typeclasses outside this module?
    pub fn unsafe_instantiate_condition(
        &self,
        typeclass: &Typeclass,
        condition_name: &str,
        instance_type: &AcornType,
        project: &Project,
        source: &dyn ErrorSource,
    ) -> compilation::Result<AcornValue> {
        let tc_condition_name = ConstantName::typeclass_attr(typeclass.clone(), condition_name);
        let tc_bindings = self.get_bindings(typeclass.module_id, project);
        let (def, params) = match tc_bindings.get_definition_and_params(&tc_condition_name) {
            Some((def, params)) => (def, params),
            None => {
                return Err(source.error(&format!(
                    "could not find definition for typeclass condition {}",
                    tc_condition_name
                )));
            }
        };
        if params.len() != 1 {
            return Err(source.error(&format!(
                "typeclass condition {} should have one parameter",
                tc_condition_name
            )));
        }
        // We are explicitly instantiating in a way that would fail typechecking.
        let universal = match def.clone() {
            AcornValue::Lambda(args, val) => AcornValue::ForAll(args, val),
            v => v,
        };
        let unsafe_param = (params[0].name.clone(), instance_type.clone());
        let unsafe_instance = universal.instantiate(&[unsafe_param]);

        Ok(unsafe_instance)
    }

    /// Evaluate an expression that creates a new scope for a single value inside it.
    /// This could be the statement of a theorem, the definition of a function, or other similar things.
    ///
    /// It has declarations, introducing new variables and types that exist just for this value,
    /// and it has the value itself, which can use those declarations.
    ///
    /// type_params is a list of tokens for the generic types introduced for this scope.
    /// args is a list of the new variables declared for this scope.
    /// value_type_expr is an optional expression for the type of the value.
    ///   (None means expect a boolean value.)
    /// value_expr is the expression for the value itself.
    /// function_name, when it is provided, can be used recursively.
    ///
    /// This function mutates the binding map but sets it back to its original state when finished.
    ///
    /// Returns a tuple with:
    ///   a list of type parameters
    ///   a list of argument names
    ///   a list of argument types
    ///   an optional unbound value. (None means axiom.)
    ///   the value type
    ///
    /// The type parameters are treated as arbitrary types internally to the new scope, but externally
    /// they are replaced with type variables.
    ///
    /// class_type should be provided, fully instantiated, if this is the definition of a member function.
    ///
    /// The return value is "unbound" in the sense that it has variable atoms that are not
    /// bound within any lambda, exists, or forall value. It also may have references to a
    /// recursive function that is not yet defined.
    pub fn evaluate_scoped_value(
        &mut self,
        type_param_exprs: &[TypeParamExpr],
        args: &[Declaration],
        value_type_expr: Option<&Expression>,
        value_expr: &Expression,
        class_type: Option<&AcornType>,
        function_name: Option<&ConstantName>,
        project: &Project,
    ) -> compilation::Result<(
        Vec<TypeParam>,
        Vec<String>,
        Vec<AcornType>,
        Option<AcornValue>,
        AcornType,
    )> {
        // Bind all the type parameters and arguments
        let type_params = Evaluator::new(self, project).evaluate_type_params(&type_param_exprs)?;
        for param in &type_params {
            self.add_arbitrary_type(param.clone());
        }
        let mut stack = Stack::new();
        let mut evaluator = Evaluator::new(self, project);
        let (arg_names, internal_arg_types) = evaluator.bind_args(&mut stack, args, class_type)?;

        // Figure out types.
        let internal_value_type = match value_type_expr {
            Some(e) => evaluator.evaluate_type(e)?,
            None => AcornType::Bool,
        };

        if let Some(function_name) = function_name {
            let fn_type =
                AcornType::functional(internal_arg_types.clone(), internal_value_type.clone());
            // The function is bound to its name locally, to handle recursive definitions.
            // Internally to the definition, this function is not polymorphic.
            self.add_constant_name(function_name, vec![], fn_type, None, None);
        }

        // Evaluate the internal value using our modified bindings
        let internal_value = if value_expr.is_axiom() {
            None
        } else {
            let value = Evaluator::new(self, project).evaluate_value_with_stack(
                &mut stack,
                value_expr,
                Some(&internal_value_type),
            )?;

            if let Some(function_name) = function_name {
                let mut checker =
                    TerminationChecker::new(function_name.clone(), internal_arg_types.len());
                if !checker.check(&value) {
                    return Err(
                        value_expr.error("the compiler thinks this looks like an infinite loop")
                    );
                }
            }

            Some(value)
        };

        // Reset the bindings
        for param in type_params.iter().rev() {
            self.remove_type(&param.name);
        }
        if let Some(function_name) = function_name {
            self.remove_constant(function_name);
        }

        // We might have types parametrized on this function, or they might be parametrized on the
        // class definition. We only want to genericize the parameters that we created.
        if type_params.is_empty() {
            // Just keep the types as they are.
            Ok((
                type_params,
                arg_names,
                internal_arg_types,
                internal_value,
                internal_value_type,
            ))
        } else {
            // Convert to external type variables
            let external_value = if let Some(internal_value) = internal_value {
                if let Some(function_name) = function_name {
                    // We might have defined this function recursively.
                    // In this case, internally it's not polymorphic. It's just a constant
                    // with a type that depends on the arbitrary types we introduced.
                    // But, externally we need to make it polymorphic.
                    let generic_params = type_params
                        .iter()
                        .map(|param| AcornType::Variable(param.clone()))
                        .collect();
                    let derecursed = internal_value.set_params(function_name, &generic_params);
                    Some(derecursed.genericize(&type_params))
                } else {
                    // There's no name for this function so it can't possibly be recursive.
                    // This is simpler, but we still need to remove arbitrary local types.
                    Some(internal_value.genericize(&type_params))
                }
            } else {
                // No internal value, no external value.
                None
            };
            let external_arg_types = internal_arg_types
                .iter()
                .map(|t| t.genericize(&type_params))
                .collect();
            let external_value_type = internal_value_type.genericize(&type_params);

            Ok((
                type_params,
                arg_names,
                external_arg_types,
                external_value,
                external_value_type,
            ))
        }
    }

    /// Finds the names of all constants that are in this module but unknown to this binding map.
    /// The unknown constants may not be polymorphic.
    pub fn find_unknown_local_constants(
        &self,
        value: &AcornValue,
        answer: &mut HashMap<String, AcornType>,
    ) {
        match value {
            AcornValue::Variable(_, _) | AcornValue::Bool(_) => {}
            AcornValue::Constant(c) => {
                if c.name.module_id() == self.module_id && !self.constant_info.contains_key(&c.name)
                {
                    assert!(c.params.is_empty());
                    answer.insert(c.name.to_string(), c.instance_type.clone());
                }
            }

            AcornValue::Application(app) => {
                self.find_unknown_local_constants(&app.function, answer);
                for arg in &app.args {
                    self.find_unknown_local_constants(arg, answer);
                }
            }
            AcornValue::Lambda(_, value)
            | AcornValue::ForAll(_, value)
            | AcornValue::Exists(_, value) => {
                self.find_unknown_local_constants(value, answer);
            }
            AcornValue::Binary(_, left, right) => {
                self.find_unknown_local_constants(left, answer);
                self.find_unknown_local_constants(right, answer);
            }
            AcornValue::IfThenElse(cond, then_value, else_value) => {
                self.find_unknown_local_constants(cond, answer);
                self.find_unknown_local_constants(then_value, answer);
                self.find_unknown_local_constants(else_value, answer);
            }
            AcornValue::Match(scrutinee, cases) => {
                self.find_unknown_local_constants(scrutinee, answer);
                for (_, pattern, result) in cases {
                    self.find_unknown_local_constants(pattern, answer);
                    self.find_unknown_local_constants(result, answer);
                }
            }
            AcornValue::Not(value) => {
                self.find_unknown_local_constants(value, answer);
            }
        }
    }

    /// Replaces all theorems in the proposition with their definitions.
    /// This is admittedly weird.
    /// Note that it needs to work with templated theorems, which makes it tricky to do the
    /// type inference.
    pub fn expand_theorems(&self, proposition: Proposition, project: &Project) -> Proposition {
        proposition
            .value
            .validate()
            .unwrap_or_else(|e| panic!("invalid claim: {} ({})", proposition.value, e));

        let value = proposition.value.replace_constants(0, &|c| {
            let bindings = self.get_bindings(c.name.module_id(), project);
            if bindings.is_theorem(&c.name) {
                match bindings.get_definition_and_params(&c.name) {
                    Some((def, params)) => {
                        let mut pairs = vec![];
                        for (param, t) in params.iter().zip(c.params.iter()) {
                            pairs.push((param.name.clone(), t.clone()));
                        }
                        Some(def.instantiate(&pairs))
                    }
                    None => None,
                }
            } else {
                None
            }
        });
        proposition.with_value(value)
    }

    ////////////////////////////////////////////////////////////////////////////////
    // Tools for testing.
    ////////////////////////////////////////////////////////////////////////////////

    /// Check that the given name actually does have this type in the environment.
    pub fn expect_type(&self, name: &str, type_string: &str) {
        let name = DefinedName::unqualified(self.module_id, name);
        let value = self
            .get_constant_value(&name, &PanicOnError)
            .expect("no such constant");
        let env_type = value.get_type();
        assert_eq!(env_type.to_string(), type_string);
    }

    /// Check that this code, when converted to a value and back to code, is the same.
    pub fn expect_good_code(&self, input_code: &str) {
        let project = Project::new_mock();
        let expression = Expression::expect_value(input_code);
        let value = Evaluator::new(self, &project)
            .evaluate_value(&expression, None)
            .expect("evaluate_value failed");
        let output_code = CodeGenerator::new(self)
            .value_to_code(&value)
            .expect("value_to_code failed");
        assert_eq!(input_code, output_code);
    }
}

/// Information about a constructor.
#[derive(Clone)]
pub struct ConstructorInfo {
    /// The class that this constructor constructs.
    pub class: Class,

    /// The index of this constructor in the class.
    pub index: usize,

    /// The total number of constructors for this class.
    pub total: usize,
}

/// Information about a class that is accessible from this module.
#[derive(Clone, Debug)]
struct ClassInfo {
    /// What module defines each of the attributes of this class.
    attributes: BTreeMap<String, ModuleId>,

    /// Maps typeclasses this class is an instance of to the module with the instance statement.
    typeclasses: HashMap<Typeclass, ModuleId>,

    /// The preferred local name for this class.
    alias: Option<String>,
}

impl ClassInfo {
    fn new() -> Self {
        ClassInfo {
            attributes: BTreeMap::new(),
            typeclasses: HashMap::new(),
            alias: None,
        }
    }

    fn import(
        &mut self,
        info: &ClassInfo,
        typename: &str,
        source: &dyn ErrorSource,
    ) -> compilation::Result<()> {
        for (attr, other_module_id) in info.attributes.iter() {
            match self.attributes.get(attr) {
                None => {
                    self.attributes.insert(attr.clone(), *other_module_id);
                }
                Some(module_id) => {
                    if *module_id != *other_module_id {
                        return Err(source.error(&format!(
                            "attribute {}.{} is defined in two different modules",
                            typename, attr
                        )));
                    }
                }
            }
        }
        for (typeclass, other_module_id) in info.typeclasses.iter() {
            if let Some(module_id) = self.typeclasses.insert(typeclass.clone(), *other_module_id) {
                if module_id != *other_module_id {
                    return Err(source.error(&format!(
                        "instance relation {}: {} is defined in two different modules",
                        typename, typeclass.name
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Information about a typeclass that is defined in this module.
#[derive(Clone, Debug)]
struct TypeclassInfo {
    /// The attributes available to this typeclass.
    /// The value stores the typeclass on which this attribute was originally defined.
    /// (This can be the typeclass itself.)
    attributes: BTreeMap<String, Typeclass>,

    /// The typeclasses that this typeclass extends.
    extends: HashSet<Typeclass>,

    /// The preferred local name for this typeclass.
    alias: Option<String>,
}

impl TypeclassInfo {
    fn new() -> Self {
        TypeclassInfo {
            attributes: BTreeMap::new(),
            extends: HashSet::new(),
            alias: None,
        }
    }
}

/// Information that the BindingMap stores about a constant.
#[derive(Clone)]
struct ConstantInfo {
    /// The value for this constant. Not the definition, but the constant itself.
    /// If this is a generic constant, this value is unresolved.
    value: PotentialValue,

    /// The definition of this constant, if it has one.
    /// Not included for aliases.
    definition: Option<AcornValue>,

    /// Whether this is the canonical name for a constant, as opposed to an alias or an import.
    canonical: bool,

    /// Whether this is a theorem.
    theorem: bool,

    /// If this constant is a constructor and this is its canonical name, store:
    ///   the class it constructs
    ///   an index of which constructor it is
    ///   how many total constructors there are
    /// Not included for aliases.
    constructor: Option<ConstructorInfo>,
}

/// Helper for autocomplete.
fn keys_with_prefix<'a, T>(
    map: &'a BTreeMap<String, T>,
    prefix: &'a str,
) -> impl Iterator<Item = &'a String> {
    map.range(prefix.to_string()..)
        .take_while(move |(key, _)| key.starts_with(prefix))
        .map(|(key, _)| key)
}

impl TypeclassRegistry for BindingMap {
    fn is_instance_of(&self, class: &Class, typeclass: &Typeclass) -> bool {
        self.class_info
            .get(&class)
            .map_or(false, |info| info.typeclasses.contains_key(typeclass))
    }

    fn extends(&self, typeclass: &Typeclass, base: &Typeclass) -> bool {
        if let Some(info) = self.typeclass_info.get(typeclass) {
            info.extends.contains(base)
        } else {
            false
        }
    }
}
