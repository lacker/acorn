use std::collections::HashMap;

use tower_lsp::lsp_types::Range;

use crate::acorn_type::AcornType;
use crate::acorn_value::{AcornValue, BinaryOp, FunctionApplication};
use crate::atom::AtomId;
use crate::binding_map::{BindingMap, Stack};
use crate::goal_context::{Goal, GoalContext};
use crate::module::ModuleId;
use crate::project::{LoadError, Project};
use crate::proposition::Proposition;
use crate::statement::{Body, DefineStatement, LetStatement, Statement, StatementInfo};
use crate::token::{self, Error, Token, TokenIter, TokenType};

// Each line has a LineType, to handle line-based user interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineType {
    // Only used within subenvironments.
    // The line relates to the block, but is outside the opening brace for this block.
    Opening,

    // This line corresponds to a node inside the environment.
    // The usize is an index into the nodes array.
    // If the node represents a block, this line should also have a line type in the
    // subenvironment within the block.
    Node(usize),

    // Either only whitespace is here, or a comment.
    Empty,

    // Lines with other sorts of statements besides prop statements.
    Other,

    // Only used within subenvironments.
    // The line has the closing brace for this block.
    Closing,
}

// The Environment takes Statements as input and processes them.
// It does not prove anything directly, but it is responsible for determining which
// things need to be proved, and which statements are usable in which proofs.
// It creates subenvironments for nested blocks.
// It does not have to be efficient enough to run in the inner loop of the prover.
pub struct Environment {
    pub module_id: ModuleId,

    // What all the names mean in this environment
    pub bindings: BindingMap,

    // The nodes structure is fundamentally linear.
    // Each node depends only on the nodes before it.
    nodes: Vec<Node>,

    // The region in the source document where a name was defined
    definition_ranges: HashMap<String, Range>,

    // Whether a plain "false" is anywhere in this environment.
    // This indicates that the environment is supposed to have contradictory facts.
    pub includes_explicit_false: bool,

    // For the base environment, first_line is zero.
    // first_line is usually nonzero when the environment is a subenvironment.
    // line_types[0] corresponds to first_line in the source document.
    first_line: u32,
    line_types: Vec<LineType>,

    // Implicit blocks aren't written in the code; they are created for theorems that
    // the user has asserted without proof.
    pub implicit: bool,
}

// Logically, the Environment is arranged like a tree structure.
// It can have blocks that contain subenvironments, which contain more blocks, etc.
// It can also have plain propositions.
// The Node represents either one of these two children of an Environment.
struct Node {
    // Whether this proposition has already been proved structurally.
    // For example, this could be an axiom, or a definition.
    structural: bool,

    // The proposition represented by this tree.
    // If this proposition has a block, this represents the "external claim".
    // It is the value we can assume is true, in the outer environment, when everything
    // in the inner environment has been proven.
    // Besides the claim, nothing else from the block is visible externally.
    //
    // This claim needs to be proved for nonstructural propositions, when there is no block.
    claim: Proposition,

    // The body of the proposition, when it has an associated block.
    // When there is a block, proving every proposition in the block implies that the
    // claim is proven as well.
    block: Option<Block>,
}

// Proofs are structured into blocks.
// The environment specific to this block can have a bunch of propositions that need to be
// proved, along with helper statements to express those propositions, but they are not
// visible to the outside world.
struct Block {
    // The generic types that this block is polymorphic over.
    // Internally to the block, these are opaque data types.
    // Externally, these are generic data types.
    type_params: Vec<String>,

    // The arguments to this block.
    // Internally to the block, the arguments are constants.
    // Externally, these arguments are variables.
    args: Vec<(String, AcornType)>,

    // The goal for a block is relative to its internal environment.
    // Everything in the block can be used to achieve this goal.
    goal: Option<Goal>,

    // The environment created inside the block.
    env: Environment,
}

impl Block {
    // Convert a boolean value from the block's environment to a value in the outer environment.
    fn export_bool(&self, outer_env: &Environment, inner_value: &AcornValue) -> AcornValue {
        // The constants that were block arguments will export as "forall" variables.
        let mut forall_names: Vec<String> = vec![];
        let mut forall_types: Vec<AcornType> = vec![];
        for (name, t) in &self.args {
            forall_names.push(name.clone());
            forall_types.push(t.clone());
        }

        // Find all unexportable constants
        let mut unexportable: HashMap<String, AcornType> = HashMap::new();
        outer_env
            .bindings
            .find_unknown_local_constants(inner_value, &mut unexportable);

        // Unexportable constants that are not arguments export as "exists" variables.
        let mut exists_names = vec![];
        let mut exists_types = vec![];
        for (name, t) in unexportable {
            if forall_names.contains(&name) {
                continue;
            }
            exists_names.push(name);
            exists_types.push(t);
        }

        // Internal variables need to be shifted over
        let shift_amount = (forall_names.len() + exists_names.len()) as AtomId;

        // The forall must be outside the exists, so order stack variables appropriately
        let mut map: HashMap<String, AtomId> = HashMap::new();
        for (i, name) in forall_names
            .into_iter()
            .chain(exists_names.into_iter())
            .enumerate()
        {
            map.insert(name, i as AtomId);
        }

        // Replace all of the constants that only exist in the inside environment
        let replaced = inner_value.clone().insert_stack(0, shift_amount);
        let replaced = replaced.replace_constants_with_vars(outer_env.module_id, &map);
        let replaced = replaced.parametrize(self.env.module_id, &self.type_params);
        AcornValue::new_forall(forall_types, AcornValue::new_exists(exists_types, replaced))
    }

    // Returns a claim usable in the outer environment, and a range where it comes from.
    fn export_last_claim(
        &self,
        outer_env: &Environment,
        token: &Token,
    ) -> token::Result<(AcornValue, Range)> {
        let (inner_claim, range) = match self.env.nodes.last() {
            Some(p) => (&p.claim.value, p.claim.source.range),
            None => {
                return Err(Error::new(token, "expected a claim in this block"));
            }
        };
        let outer_claim = self.export_bool(outer_env, inner_claim);
        Ok((outer_claim, range))
    }

    // Checks if this block solves for the given target.
    // If it does, returns an exported proposition with the solution, and the range where it
    // occurs.
    fn solves(&self, outer_env: &Environment, target: &AcornValue) -> Option<(AcornValue, Range)> {
        let (outer_claim, range) = match self.export_last_claim(outer_env, &Token::empty()) {
            Ok((c, r)) => (c, r),
            Err(_) => return None,
        };
        match &outer_claim {
            // We only allow <target> == <solution>, rather than the other way around.
            AcornValue::Binary(BinaryOp::Equals, left, _) => {
                if left.as_ref() == target {
                    Some((outer_claim, range))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

// The different ways to construct a block
enum BlockParams<'a> {
    // (theorem name, premise, goal)
    //
    // The premise and goal are unbound, to be proved based on the args of the theorem.
    //
    // The theorem should already be defined by this name in the external environment.
    // It is either a bool, or a function from something -> bool.
    // The meaning of the theorem is that it is true for all args.
    //
    // The premise is optional.
    Theorem(&'a str, Option<(AcornValue, Range)>, AcornValue),

    // The assumption to be used by the block, and the range of this assumption.
    Conditional(&'a AcornValue, Range),

    // The expression to solve for, and the range of the "solve <target>" component.
    Solve(AcornValue, Range),

    // (unbound goal, function return type, range of condition)
    // This goal has one more unbound variable than the block args account for.
    // The last one, we are trying to prove there exists a variable that satisfies the goal.
    FunctionSatisfy(AcornValue, AcornType, Range),

    // No special params needed
    ForAll,
    Problem,
}

impl Environment {
    pub fn new(module_id: ModuleId) -> Self {
        Environment {
            module_id,
            bindings: BindingMap::new(module_id),
            nodes: Vec::new(),
            definition_ranges: HashMap::new(),
            includes_explicit_false: false,
            first_line: 0,
            line_types: Vec::new(),
            implicit: false,
        }
    }

    // Create a test version of the environment.
    #[cfg(test)]
    pub fn new_test() -> Self {
        use crate::module::FIRST_NORMAL;
        Environment::new(FIRST_NORMAL)
    }

    fn next_line(&self) -> u32 {
        self.line_types.len() as u32 + self.first_line
    }

    fn last_line(&self) -> u32 {
        self.next_line() - 1
    }

    fn theorem_range(&self, name: &str) -> Option<Range> {
        self.definition_ranges.get(name).cloned()
    }

    // Add line types for the given range, inserting empties as needed.
    // If the line already has a type, do nothing.
    fn add_line_types(&mut self, line_type: LineType, first: u32, last: u32) {
        while self.next_line() < first {
            self.line_types.push(LineType::Empty);
        }
        while self.next_line() <= last {
            self.line_types.push(line_type);
        }
    }

    fn add_other_lines(&mut self, statement: &Statement) {
        self.add_line_types(
            LineType::Other,
            statement.first_line(),
            statement.last_line(),
        );
    }

    fn add_prop_lines(&mut self, index: usize, statement: &Statement) {
        self.add_line_types(
            LineType::Node(index),
            statement.first_line(),
            statement.last_line(),
        );
    }

    fn get_line_type(&self, line: u32) -> Option<LineType> {
        if line < self.first_line {
            return None;
        }
        let index = (line - self.first_line) as usize;
        if index < self.line_types.len() {
            Some(self.line_types[index])
        } else {
            None
        }
    }

    // Creates a new block with a subenvironment by copying this environment and adding some stuff.
    //
    // Performance is quadratic because it clones a lot of the existing environment.
    // Using different data structures should improve this when we need to.
    //
    // The types in args must be generic when type params are provided.
    // If no body is provided, the block has no statements in it.
    fn new_block(
        &self,
        project: &mut Project,
        type_params: Vec<String>,
        args: Vec<(String, AcornType)>,
        params: BlockParams,
        first_line: u32,
        last_line: u32,
        body: Option<&Body>,
    ) -> token::Result<Block> {
        let mut subenv = Environment {
            module_id: self.module_id,
            bindings: self.bindings.clone(),
            nodes: Vec::new(),
            definition_ranges: self.definition_ranges.clone(),
            includes_explicit_false: false,
            first_line,
            line_types: Vec::new(),
            implicit: body.is_none(),
        };

        // Inside the block, the type parameters are opaque data types.
        let param_pairs: Vec<(String, AcornType)> = type_params
            .iter()
            .map(|s| (s.clone(), subenv.bindings.add_data_type(&s)))
            .collect();

        // Inside the block, the arguments are constants.
        for (arg_name, generic_arg_type) in &args {
            let specific_arg_type = generic_arg_type.specialize(&param_pairs);
            subenv
                .bindings
                .add_constant(&arg_name, vec![], specific_arg_type, None);
        }

        let goal = match params {
            BlockParams::Conditional(condition, range) => {
                subenv.add_node(
                    project,
                    true,
                    Proposition::premise(condition.clone(), self.module_id, range),
                    None,
                );
                None
            }
            BlockParams::Theorem(theorem_name, premise, unbound_goal) => {
                let theorem_type = self
                    .bindings
                    .get_type_for_identifier(theorem_name)
                    .unwrap()
                    .clone();

                // The theorem as a named function from args -> bool.
                let functional_theorem = AcornValue::new_specialized(
                    self.module_id,
                    theorem_name.to_string(),
                    theorem_type,
                    param_pairs,
                );

                let arg_values = args
                    .iter()
                    .map(|(name, _)| subenv.bindings.get_constant_value(name).unwrap())
                    .collect::<Vec<_>>();

                // Within the theorem block, the theorem is treated like a function,
                // with propositions to define its identity.
                // This is a compromise initially inspired by the desire so to do induction
                // without writing a separate definition for the inductive hypothesis.
                // (Outside the theorem block, theorems are inlined.)
                subenv.add_identity_props(project, theorem_name);

                if let Some((unbound_premise, premise_range)) = premise {
                    // Add the premise to the environment, when proving the theorem.
                    // The premise is unbound, so we need to bind the block's arg values.
                    let bound = unbound_premise.bind_values(0, 0, &arg_values);

                    subenv.add_node(
                        project,
                        true,
                        Proposition::premise(bound, self.module_id, premise_range),
                        None,
                    );
                }

                // We can prove the goal either in bound or in function form
                let bound_goal = unbound_goal.bind_values(0, 0, &arg_values);
                let functional_goal = AcornValue::new_apply(functional_theorem, arg_values);
                let value = AcornValue::new_or(functional_goal, bound_goal);
                Some(Goal::Prove(Proposition::theorem(
                    false,
                    value,
                    self.module_id,
                    self.theorem_range(theorem_name).unwrap(),
                    theorem_name.to_string(),
                )))
            }
            BlockParams::FunctionSatisfy(unbound_goal, return_type, range) => {
                // In the block, we need to prove this goal in bound form, so bind args to it.
                let arg_values = args
                    .iter()
                    .map(|(name, _)| subenv.bindings.get_constant_value(name).unwrap())
                    .collect::<Vec<_>>();
                // The partial goal has variables 0..args.len() bound to the block's args,
                // but there one last variable that needs to be existentially quantified.
                let partial_goal = unbound_goal.bind_values(0, 0, &arg_values);
                let bound_goal = AcornValue::new_exists(vec![return_type], partial_goal);
                let prop = Proposition::anonymous(bound_goal, self.module_id, range);
                Some(Goal::Prove(prop))
            }
            BlockParams::Solve(target, range) => Some(Goal::Solve(target, range)),
            BlockParams::ForAll | BlockParams::Problem => None,
        };

        match body {
            Some(body) => {
                subenv.add_line_types(
                    LineType::Opening,
                    first_line,
                    body.left_brace.line_number as u32,
                );
                for s in &body.statements {
                    subenv.add_statement(project, s)?;
                }
                subenv.add_line_types(
                    LineType::Closing,
                    body.right_brace.line_number as u32,
                    body.right_brace.line_number as u32,
                );
            }
            None => {
                // The subenv is an implicit block, so consider all the lines to be "opening".
                subenv.add_line_types(LineType::Opening, first_line, last_line);
            }
        };
        Ok(Block {
            type_params,
            args,
            env: subenv,
            goal,
        })
    }

    // Adds a node to the environment tree.
    // This also macro-expands theorem names into their definitions.
    // Ideally, that would happen during expression parsing.
    // However, it needs to work with templated theorems, which makes it tricky/hacky to do the
    // type inference.
    fn add_node(
        &mut self,
        project: &Project,
        structural: bool,
        proposition: Proposition,
        block: Option<Block>,
    ) -> usize {
        // Check if we're adding invalid claims.
        proposition
            .value
            .validate()
            .unwrap_or_else(|e| panic!("invalid claim: {} ({})", proposition.value, e));

        if structural {
            assert!(block.is_none());
        }

        let value = proposition
            .value
            .replace_constants_with_values(0, &|module_id, name| {
                let bindings = if self.module_id == module_id {
                    &self.bindings
                } else {
                    &project
                        .get_env(module_id)
                        .expect("missing module during add_proposition")
                        .bindings
                };
                if bindings.is_theorem(name) {
                    bindings.get_definition(name).clone()
                } else {
                    None
                }
            });
        let claim = proposition.with_value(value);
        self.nodes.push(Node {
            structural,
            claim,
            block,
        });
        self.nodes.len() - 1
    }

    // Adds a proposition, or multiple propositions, to represent the definition of the provided
    // constant.
    fn add_identity_props(&mut self, project: &Project, name: &str) {
        let definition = if let Some(d) = self.bindings.get_definition(name) {
            d.clone()
        } else {
            return;
        };

        let constant_type_clone = self.bindings.get_type_for_identifier(name).unwrap().clone();
        let param_names = self.bindings.get_params(name);

        let constant = if param_names.is_empty() {
            AcornValue::Constant(
                self.module_id,
                name.to_string(),
                constant_type_clone,
                vec![],
            )
        } else {
            let params = param_names
                .into_iter()
                .map(|n| (n.clone(), AcornType::Parameter(n)))
                .collect();
            AcornValue::Specialized(
                self.module_id,
                name.to_string(),
                constant_type_clone,
                params,
            )
        };
        let claim = if let AcornValue::Lambda(acorn_types, return_value) = definition {
            let args: Vec<_> = acorn_types
                .iter()
                .enumerate()
                .map(|(i, acorn_type)| AcornValue::Variable(i as AtomId, acorn_type.clone()))
                .collect();
            let app = AcornValue::Application(FunctionApplication {
                function: Box::new(constant),
                args,
            });
            AcornValue::ForAll(
                acorn_types,
                Box::new(AcornValue::Binary(
                    BinaryOp::Equals,
                    Box::new(app),
                    return_value,
                )),
            )
        } else {
            AcornValue::Binary(BinaryOp::Equals, Box::new(constant), Box::new(definition))
        };
        let range = self.definition_ranges.get(name).unwrap().clone();

        self.add_node(
            project,
            true,
            Proposition::definition(claim, self.module_id, range, name.to_string()),
            None,
        );
    }

    pub fn get_definition(&self, name: &str) -> Option<&AcornValue> {
        self.bindings.get_definition(name)
    }

    pub fn get_theorem_claim(&self, name: &str) -> Option<AcornValue> {
        for prop in &self.nodes {
            if let Some(claim_name) = prop.claim.name() {
                if claim_name == name {
                    return Some(prop.claim.value.clone());
                }
            }
        }
        None
    }

    // Adds a conditional block to the environment.
    // Takes the condition, the range to associate with the condition, the first line of
    // the conditional block, and finally the body itself.
    // If this is an "else" block, we pass in the claim from the "if" part of the block.
    // This way, if the claim is the same, we can simplify by combining them when exported.
    // Returns the last claim in the block, if we didn't have an if-claim to match against.
    fn add_conditional(
        &mut self,
        project: &mut Project,
        condition: AcornValue,
        range: Range,
        first_line: u32,
        last_line: u32,
        body: &Body,
        if_claim: Option<AcornValue>,
    ) -> token::Result<Option<AcornValue>> {
        if body.statements.is_empty() {
            // Conditional blocks with an empty body can just be ignored
            return Ok(None);
        }
        let block = self.new_block(
            project,
            vec![],
            vec![],
            BlockParams::Conditional(&condition, range),
            first_line,
            last_line,
            Some(body),
        )?;
        let (outer_claim, claim_range) = block.export_last_claim(self, &body.right_brace)?;

        let matching_branches = if let Some(if_claim) = if_claim {
            if outer_claim == if_claim {
                true
            } else {
                false
            }
        } else {
            false
        };
        let (external_claim, last_claim) = if matching_branches {
            (outer_claim, None)
        } else {
            (
                AcornValue::Binary(
                    BinaryOp::Implies,
                    Box::new(condition),
                    Box::new(outer_claim.clone()),
                ),
                Some(outer_claim),
            )
        };
        let index = self.add_node(
            project,
            false,
            Proposition::anonymous(external_claim, self.module_id, claim_range),
            Some(block),
        );
        self.add_line_types(
            LineType::Node(index),
            first_line,
            body.right_brace.line_number,
        );
        Ok(last_claim)
    }

    // Adds a "let" statement to the environment, that may be within a class block.
    fn add_let_statement(
        &mut self,
        project: &Project,
        class: Option<&str>,
        ls: &LetStatement,
        range: Range,
    ) -> token::Result<()> {
        if class.is_none() && ls.name_token.token_type == TokenType::Numeral {
            return Err(Error::new(
                &ls.name_token,
                "numeric literals may not be defined outside of a class",
            ));
        }
        if ls.name == "self"
            || ls.name == "new"
            || ls.name == "read"
            || (class.is_some() && TokenType::from_magic_method_name(&ls.name).is_some())
        {
            return Err(Error::new(
                &ls.name_token,
                &format!("'{}' is a reserved word. use a different name", ls.name),
            ));
        }
        let name = match class {
            Some(c) => format!("{}.{}", c, ls.name),
            None => ls.name.clone(),
        };
        if self.bindings.name_in_use(&name) {
            return Err(Error::new(
                &ls.name_token,
                &format!("constant name '{}' already defined in this scope", name),
            ));
        }
        let acorn_type = self.bindings.evaluate_type(project, &ls.type_expr)?;
        if ls.name_token.token_type == TokenType::Numeral {
            if acorn_type != AcornType::Data(self.module_id, class.unwrap().to_string()) {
                return Err(Error::new(
                    &ls.type_expr.token(),
                    "numeric class variables must be the class type",
                ));
            }
        }
        let value = if ls.value.token().token_type == TokenType::Axiom {
            AcornValue::Constant(self.module_id, name.clone(), acorn_type.clone(), vec![])
        } else {
            self.bindings
                .evaluate_value(project, &ls.value, Some(&acorn_type))?
        };
        self.bindings
            .add_constant(&name, vec![], acorn_type, Some(value));
        self.definition_ranges.insert(name.clone(), range);
        self.add_identity_props(project, &name);
        Ok(())
    }

    // Adds a "define" statement to the environment, that may be within a class block.
    fn add_define_statement(
        &mut self,
        project: &Project,
        class_name: Option<&str>,
        ds: &DefineStatement,
        range: Range,
    ) -> token::Result<()> {
        if ds.name == "new" || ds.name == "self" {
            return Err(Error::new(
                &ds.name_token,
                &format!("'{}' is a reserved word. use a different name", ds.name),
            ));
        }
        let name = match class_name {
            Some(c) => format!("{}.{}", c, ds.name),
            None => ds.name.clone(),
        };
        if self.bindings.name_in_use(&name) {
            return Err(Error::new(
                &ds.name_token,
                &format!("function name '{}' already defined in this scope", name),
            ));
        }

        // Calculate the function value
        let (param_names, _, arg_types, unbound_value, value_type) =
            self.bindings.evaluate_subvalue(
                project,
                &ds.type_params,
                &ds.args,
                Some(&ds.return_type),
                &ds.return_value,
                class_name,
            )?;

        if let Some(class_name) = class_name {
            let class_type = AcornType::Data(self.module_id, class_name.to_string());
            if arg_types[0] != class_type {
                return Err(Error::new(
                    ds.args[0].token(),
                    "self must be the class type",
                ));
            }

            if ds.name == "read" {
                if arg_types.len() != 2 || arg_types[1] != class_type || value_type != class_type {
                    return Err(Error::new(
                        &ds.name_token,
                        &format!(
                            "{}.read should be type ({}, {}) -> {}",
                            class_name, class_name, class_name, class_name
                        ),
                    ));
                }
            }
        }

        if let Some(v) = unbound_value {
            let fn_value = AcornValue::new_lambda(arg_types, v);
            // Add the function value to the environment
            self.bindings
                .add_constant(&name, param_names, fn_value.get_type(), Some(fn_value));
        } else {
            let new_axiom_type = AcornType::new_functional(arg_types, value_type);
            self.bindings
                .add_constant(&name, param_names, new_axiom_type, None);
        };

        self.definition_ranges.insert(name.clone(), range);
        self.add_identity_props(project, &name);
        Ok(())
    }

    // Adds a statement to the environment.
    // If the statement has a body, this call creates a sub-environment and adds the body
    // to that sub-environment.
    pub fn add_statement(
        &mut self,
        project: &mut Project,
        statement: &Statement,
    ) -> token::Result<()> {
        if self.includes_explicit_false {
            return Err(Error::new(
                &statement.first_token,
                "an explicit 'false' may not be followed by other statements",
            ));
        }
        match &statement.statement {
            StatementInfo::Type(ts) => {
                self.add_other_lines(statement);
                if self.bindings.name_in_use(&ts.name) {
                    return Err(Error::new(
                        &ts.type_expr.token(),
                        &format!("type name '{}' already defined in this scope", ts.name),
                    ));
                }
                if ts.type_expr.token().token_type == TokenType::Axiom {
                    self.bindings.add_data_type(&ts.name);
                } else {
                    let acorn_type = self.bindings.evaluate_type(project, &ts.type_expr)?;
                    self.bindings.add_type_alias(&ts.name, acorn_type);
                };
                Ok(())
            }

            StatementInfo::Let(ls) => {
                self.add_other_lines(statement);
                self.add_let_statement(project, None, ls, statement.range())
            }

            StatementInfo::Define(ds) => {
                self.add_other_lines(statement);
                self.add_define_statement(project, None, ds, statement.range())
            }

            StatementInfo::Theorem(ts) => {
                if self.bindings.name_in_use(&ts.name) {
                    return Err(Error::new(
                        &statement.first_token,
                        &format!("theorem name '{}' already defined in this scope", ts.name),
                    ));
                }

                // Figure out the range for this theorem definition.
                // It's smaller than the whole theorem statement because it doesn't
                // include the proof block.
                let range = Range {
                    start: statement.first_token.start_pos(),
                    end: ts.claim.last_token().end_pos(),
                };
                self.definition_ranges.insert(ts.name.to_string(), range);

                let (type_params, arg_names, arg_types, value, _) = self
                    .bindings
                    .evaluate_subvalue(project, &ts.type_params, &ts.args, None, &ts.claim, None)?;

                let unbound_claim = value.ok_or_else(|| {
                    Error::new(&statement.first_token, "theorems must have values")
                })?;

                let mut block_args = vec![];
                for (arg_name, arg_type) in arg_names.iter().zip(&arg_types) {
                    block_args.push((arg_name.clone(), arg_type.clone()));
                }

                // Externally we use the theorem in unnamed, "forall" form
                let external_claim =
                    AcornValue::new_forall(arg_types.clone(), unbound_claim.clone());

                let (premise, goal) = match &unbound_claim {
                    AcornValue::Binary(BinaryOp::Implies, left, right) => {
                        let premise_range = match ts.claim.premise() {
                            Some(p) => p.range(),
                            None => {
                                // I don't think this should happen, but it's awkward for the
                                // compiler to enforce, so pick a not-too-wrong default.
                                ts.claim.range()
                            }
                        };
                        (Some((*left.clone(), premise_range)), *right.clone())
                    }
                    c => (None, c.clone()),
                };

                // We define the theorem using "lambda" form.
                // The definition happens here, in the outside environment, because the
                // theorem is usable by name in this environment.
                let lambda_claim = AcornValue::new_lambda(arg_types, unbound_claim);
                let theorem_type = lambda_claim.get_type();
                self.bindings.add_constant(
                    &ts.name,
                    type_params.clone(),
                    theorem_type.clone(),
                    Some(lambda_claim.clone()),
                );

                let block = if ts.axiomatic {
                    None
                } else {
                    Some(self.new_block(
                        project,
                        type_params,
                        block_args,
                        BlockParams::Theorem(&ts.name, premise, goal),
                        statement.first_line(),
                        statement.last_line(),
                        ts.body.as_ref(),
                    )?)
                };

                let index = self.add_node(
                    project,
                    ts.axiomatic,
                    Proposition::theorem(
                        ts.axiomatic,
                        external_claim,
                        self.module_id,
                        range,
                        ts.name.to_string(),
                    ),
                    block,
                );
                self.add_prop_lines(index, statement);
                self.bindings.mark_as_theorem(&ts.name);

                Ok(())
            }

            StatementInfo::Prop(ps) => {
                let claim =
                    self.bindings
                        .evaluate_value(project, &ps.claim, Some(&AcornType::Bool))?;
                if claim == AcornValue::Bool(false) {
                    self.includes_explicit_false = true;
                }

                let index = self.add_node(
                    project,
                    false,
                    Proposition::anonymous(claim, self.module_id, statement.range()),
                    None,
                );
                self.add_prop_lines(index, statement);
                Ok(())
            }

            StatementInfo::ForAll(fas) => {
                if fas.body.statements.is_empty() {
                    // ForAll statements with an empty body can just be ignored
                    return Ok(());
                }
                let mut args = vec![];
                for quantifier in &fas.quantifiers {
                    let (arg_name, arg_type) =
                        self.bindings.evaluate_declaration(project, quantifier)?;
                    args.push((arg_name, arg_type));
                }

                let block = self.new_block(
                    project,
                    vec![],
                    args,
                    BlockParams::ForAll,
                    statement.first_line(),
                    statement.last_line(),
                    Some(&fas.body),
                )?;

                let (outer_claim, range) = block.export_last_claim(self, &fas.body.right_brace)?;

                let index = self.add_node(
                    project,
                    false,
                    Proposition::anonymous(outer_claim, self.module_id, range),
                    Some(block),
                );
                self.add_prop_lines(index, statement);
                Ok(())
            }

            StatementInfo::If(is) => {
                let condition =
                    self.bindings
                        .evaluate_value(project, &is.condition, Some(&AcornType::Bool))?;
                let range = is.condition.range();
                let if_claim = self.add_conditional(
                    project,
                    condition.clone(),
                    range,
                    statement.first_line(),
                    statement.last_line(),
                    &is.body,
                    None,
                )?;
                if let Some(else_body) = &is.else_body {
                    self.add_conditional(
                        project,
                        condition.negate(),
                        range,
                        else_body.left_brace.line_number as u32,
                        else_body.right_brace.line_number as u32,
                        else_body,
                        if_claim,
                    )?;
                }
                Ok(())
            }

            StatementInfo::VariableSatisfy(vss) => {
                // We need to prove the general existence claim
                let mut stack = Stack::new();
                let (quant_names, quant_types) =
                    self.bindings
                        .bind_args(&mut stack, project, &vss.declarations, None)?;
                let general_claim_value = self.bindings.evaluate_value_with_stack(
                    &mut stack,
                    project,
                    &vss.condition,
                    Some(&AcornType::Bool),
                )?;
                let general_claim =
                    AcornValue::Exists(quant_types.clone(), Box::new(general_claim_value));
                let index = self.add_node(
                    project,
                    false,
                    Proposition::anonymous(general_claim, self.module_id, statement.range()),
                    None,
                );
                self.add_prop_lines(index, statement);

                // Define the quantifiers as constants
                for (quant_name, quant_type) in quant_names.iter().zip(quant_types.iter()) {
                    self.bindings
                        .add_constant(quant_name, vec![], quant_type.clone(), None);
                }

                // We can then assume the specific existence claim with the named constants
                let specific_claim = self.bindings.evaluate_value(
                    project,
                    &vss.condition,
                    Some(&AcornType::Bool),
                )?;
                self.add_node(
                    project,
                    true,
                    Proposition::anonymous(specific_claim, self.module_id, statement.range()),
                    None,
                );

                Ok(())
            }

            StatementInfo::FunctionSatisfy(fss) => {
                if fss.name == "new" || fss.name == "self" {
                    return Err(Error::new(
                        &fss.name_token,
                        &format!("'{}' is a reserved word. use a different name", fss.name),
                    ));
                }
                if self.bindings.name_in_use(&fss.name) {
                    return Err(Error::new(
                        &statement.first_token,
                        &format!("function name '{}' already defined in this scope", fss.name),
                    ));
                }

                // Figure out the range for this function definition.
                // It's smaller than the whole function statement because it doesn't
                // include the proof block.
                let definition_range = Range {
                    start: statement.first_token.start_pos(),
                    end: fss.satisfy_token.end_pos(),
                };
                self.definition_ranges
                    .insert(fss.name.clone(), definition_range);

                let (_, mut arg_names, mut arg_types, condition, _) =
                    self.bindings.evaluate_subvalue(
                        project,
                        &[],
                        &fss.declarations,
                        None,
                        &fss.condition,
                        None,
                    )?;

                let unbound_condition = condition
                    .ok_or_else(|| Error::new(&statement.first_token, "missing condition"))?;
                if unbound_condition.get_type() != AcornType::Bool {
                    return Err(Error::new(
                        &fss.condition.token(),
                        "condition must be a boolean",
                    ));
                }

                // The return variable shouldn't become a block arg, because we're trying to
                // prove its existence.
                let _return_name = arg_names.pop().unwrap();
                let return_type = arg_types.pop().unwrap();
                let block_args: Vec<_> = arg_names
                    .iter()
                    .cloned()
                    .zip(arg_types.iter().cloned())
                    .collect();
                let num_args = block_args.len() as AtomId;

                let block = self.new_block(
                    project,
                    vec![],
                    block_args,
                    BlockParams::FunctionSatisfy(
                        unbound_condition.clone(),
                        return_type.clone(),
                        fss.condition.range(),
                    ),
                    statement.first_line(),
                    statement.last_line(),
                    fss.body.as_ref(),
                )?;

                // We define this function not with an equality, but via the condition.
                let function_type = AcornType::new_functional(arg_types.clone(), return_type);
                self.bindings
                    .add_constant(&fss.name, vec![], function_type.clone(), None);
                let function_term = AcornValue::new_apply(
                    AcornValue::Constant(self.module_id, fss.name.clone(), function_type, vec![]),
                    arg_types
                        .iter()
                        .enumerate()
                        .map(|(i, t)| AcornValue::Variable(i as AtomId, t.clone()))
                        .collect(),
                );
                let return_bound =
                    unbound_condition.bind_values(num_args, num_args, &[function_term]);
                let external_condition = AcornValue::ForAll(arg_types, Box::new(return_bound));

                let prop = Proposition::definition(
                    external_condition,
                    self.module_id,
                    definition_range,
                    fss.name.clone(),
                );

                let index = self.add_node(project, false, prop, Some(block));
                self.add_prop_lines(index, statement);
                Ok(())
            }

            StatementInfo::Structure(ss) => {
                self.add_other_lines(statement);
                if self.bindings.has_type_name(&ss.name) {
                    return Err(Error::new(
                        &statement.first_token,
                        "type name already defined in this scope",
                    ));
                }

                // Parse the fields before adding the struct type so that we can't have
                // self-referential structs.
                let mut member_fn_names = vec![];
                let mut field_types = vec![];
                for (field_name_token, field_type_expr) in &ss.fields {
                    let field_type = self.bindings.evaluate_type(project, &field_type_expr)?;
                    field_types.push(field_type.clone());
                    if TokenType::from_magic_method_name(&field_name_token.text()).is_some() {
                        return Err(Error::new(
                            field_name_token,
                            &format!(
                                "'{}' is a reserved word. use a different name",
                                field_name_token.text()
                            ),
                        ));
                    }
                    let member_fn_name = format!("{}.{}", ss.name, field_name_token.text());
                    member_fn_names.push(member_fn_name);
                }

                // The member functions take the type itself to a particular member.
                let struct_type = self.bindings.add_data_type(&ss.name);
                let mut member_fns = vec![];
                for (member_fn_name, field_type) in member_fn_names.iter().zip(&field_types) {
                    let member_fn_type =
                        AcornType::new_functional(vec![struct_type.clone()], field_type.clone());
                    self.bindings
                        .add_constant(&member_fn_name, vec![], member_fn_type, None);
                    member_fns.push(self.bindings.get_constant_value(&member_fn_name).unwrap());
                }

                // A "new" function to create one of these struct types.
                let new_fn_name = format!("{}.new", ss.name);
                let new_fn_type =
                    AcornType::new_functional(field_types.clone(), struct_type.clone());
                self.bindings
                    .add_constant(&new_fn_name, vec![], new_fn_type, None);
                let new_fn = self.bindings.get_constant_value(&new_fn_name).unwrap();

                // A struct can be recreated by new'ing from its members. Ie:
                // Pair.new(Pair.first(p), Pair.second(p)) = p.
                // This is the "new equation" for a struct type.
                let new_eq_var = AcornValue::Variable(0, struct_type.clone());
                let new_eq_args = member_fns
                    .iter()
                    .map(|f| {
                        AcornValue::Application(FunctionApplication {
                            function: Box::new(f.clone()),
                            args: vec![new_eq_var.clone()],
                        })
                    })
                    .collect::<Vec<_>>();
                let recreated = AcornValue::Application(FunctionApplication {
                    function: Box::new(new_fn.clone()),
                    args: new_eq_args,
                });
                let new_eq =
                    AcornValue::Binary(BinaryOp::Equals, Box::new(recreated), Box::new(new_eq_var));
                let new_claim = AcornValue::ForAll(vec![struct_type], Box::new(new_eq));
                let range = Range {
                    start: statement.first_token.start_pos(),
                    end: ss.name_token.end_pos(),
                };
                self.add_node(
                    project,
                    true,
                    Proposition::definition(new_claim, self.module_id, range, ss.name.clone()),
                    None,
                );

                // There are also formulas for new followed by member functions. Ie:
                // Pair.first(Pair.new(a, b)) = a.
                // These are the "member equations".
                let var_args = (0..ss.fields.len())
                    .map(|i| AcornValue::Variable(i as AtomId, field_types[i].clone()))
                    .collect::<Vec<_>>();
                let new_application = AcornValue::Application(FunctionApplication {
                    function: Box::new(new_fn),
                    args: var_args,
                });
                for i in 0..ss.fields.len() {
                    let (field_name_token, field_type_expr) = &ss.fields[i];
                    let member_fn = &member_fns[i];
                    let member_eq = AcornValue::Binary(
                        BinaryOp::Equals,
                        Box::new(AcornValue::Application(FunctionApplication {
                            function: Box::new(member_fn.clone()),
                            args: vec![new_application.clone()],
                        })),
                        Box::new(AcornValue::Variable(i as AtomId, field_types[i].clone())),
                    );
                    let member_claim = AcornValue::ForAll(field_types.clone(), Box::new(member_eq));
                    let range = Range {
                        start: field_name_token.start_pos(),
                        end: field_type_expr.last_token().end_pos(),
                    };
                    self.add_node(
                        project,
                        true,
                        Proposition::definition(
                            member_claim,
                            self.module_id,
                            range,
                            ss.name.clone(),
                        ),
                        None,
                    );
                }

                Ok(())
            }

            StatementInfo::Inductive(is) => {
                self.add_other_lines(statement);
                if self.bindings.has_type_name(&is.name) {
                    return Err(Error::new(
                        &statement.first_token,
                        "type name already defined in this scope",
                    ));
                }
                let range = Range {
                    start: statement.first_token.start_pos(),
                    end: is.name_token.end_pos(),
                };

                // Add the new type first, because we can have self-reference in the inductive type.
                let inductive_type = self.bindings.add_data_type(&is.name);

                // Parse (member name, list of arg types) for each constructor.
                let mut constructors = vec![];
                let mut has_base = false;
                for (name_token, type_list_expr) in &is.constructors {
                    let type_list = match type_list_expr {
                        Some(expr) => {
                            let mut type_list = vec![];
                            self.bindings
                                .evaluate_type_list(project, expr, &mut type_list)?;
                            type_list
                        }
                        None => vec![],
                    };
                    if !type_list.contains(&inductive_type) {
                        // This provides a base case
                        has_base = true;
                    }
                    let member_name = format!("{}.{}", is.name, name_token.text());
                    constructors.push((member_name, type_list));
                }
                if !has_base {
                    return Err(Error::new(
                        &statement.first_token,
                        "inductive type must have a base case",
                    ));
                }

                // Define the constructors.
                let mut constructor_fns = vec![];
                for (constructor_name, type_list) in &constructors {
                    let constructor_type =
                        AcornType::new_functional(type_list.clone(), inductive_type.clone());
                    self.bindings
                        .add_constant(constructor_name, vec![], constructor_type, None);
                    constructor_fns
                        .push(self.bindings.get_constant_value(constructor_name).unwrap());
                }

                // The "no confusion" property. Different constructors give different results.
                for i in 0..constructors.len() {
                    let (_, i_arg_types) = &constructors[i];
                    let i_fn = constructor_fns[i].clone();
                    let i_vars: Vec<_> = i_arg_types
                        .iter()
                        .enumerate()
                        .map(|(k, t)| AcornValue::Variable(k as AtomId, t.clone()))
                        .collect();
                    let i_app = AcornValue::new_apply(i_fn, i_vars);
                    for j in 0..i {
                        let (_, j_arg_types) = &constructors[j];
                        let j_fn = constructor_fns[j].clone();
                        let j_vars: Vec<_> = j_arg_types
                            .iter()
                            .enumerate()
                            .map(|(k, t)| {
                                AcornValue::Variable((k + i_arg_types.len()) as AtomId, t.clone())
                            })
                            .collect();
                        let j_app = AcornValue::new_apply(j_fn, j_vars);
                        let inequality = AcornValue::new_not_equals(i_app.clone(), j_app);
                        let mut quantifiers = i_arg_types.clone();
                        quantifiers.extend(j_arg_types.clone());
                        let claim = AcornValue::new_forall(quantifiers, inequality);
                        self.add_node(
                            project,
                            true,
                            Proposition::definition(claim, self.module_id, range, is.name.clone()),
                            None,
                        );
                    }
                }

                // The "canonical form" principle. Any item of this type must be created by one
                // of the constructors.
                // It seems like this is implied by induction but let's just stick it in.
                // x0 is going to be the "generic item of this type".
                let mut disjunction_parts = vec![];
                for (i, constructor_fn) in constructor_fns.iter().enumerate() {
                    let (_, arg_types) = &constructors[i];
                    let args = arg_types
                        .iter()
                        .enumerate()
                        .map(|(k, t)| AcornValue::Variable((k + 1) as AtomId, t.clone()))
                        .collect();
                    let app = AcornValue::new_apply(constructor_fn.clone(), args);
                    let var = AcornValue::Variable(0, inductive_type.clone());
                    let equality = AcornValue::new_equals(var, app);
                    let exists = AcornValue::new_exists(arg_types.clone(), equality);
                    disjunction_parts.push(exists);
                }
                let disjunction = AcornValue::reduce(BinaryOp::Or, disjunction_parts);
                let claim = AcornValue::new_forall(vec![inductive_type.clone()], disjunction);
                self.add_node(
                    project,
                    true,
                    Proposition::definition(claim, self.module_id, range, is.name.clone()),
                    None,
                );

                // The next principle is that each constructor is injective.
                // Ie if Type.construct(x0, x1) = Type.construct(x2, x3) then x0 = x2 and x1 = x3.
                for (i, constructor_fn) in constructor_fns.iter().enumerate() {
                    let (_, arg_types) = &constructors[i];
                    if arg_types.is_empty() {
                        continue;
                    }

                    // First construct the equality.
                    // "Type.construct(x0, x1) = Type.construct(x2, x3)"
                    let left_args = arg_types
                        .iter()
                        .enumerate()
                        .map(|(k, t)| AcornValue::Variable(k as AtomId, t.clone()))
                        .collect();
                    let lhs = AcornValue::new_apply(constructor_fn.clone(), left_args);
                    let right_args = arg_types
                        .iter()
                        .enumerate()
                        .map(|(k, t)| {
                            AcornValue::Variable((k + arg_types.len()) as AtomId, t.clone())
                        })
                        .collect();
                    let rhs = AcornValue::new_apply(constructor_fn.clone(), right_args);
                    let equality = AcornValue::new_equals(lhs, rhs);

                    // Then construct the implication, that the corresponding args are equal.
                    let mut conjunction_parts = vec![];
                    for (i, arg_type) in arg_types.iter().enumerate() {
                        let left = AcornValue::Variable(i as AtomId, arg_type.clone());
                        let right =
                            AcornValue::Variable((i + arg_types.len()) as AtomId, arg_type.clone());
                        let arg_equality = AcornValue::new_equals(left, right);
                        conjunction_parts.push(arg_equality);
                    }
                    let conjunction = AcornValue::reduce(BinaryOp::And, conjunction_parts);
                    let mut forall_types = arg_types.clone();
                    forall_types.extend_from_slice(&arg_types);
                    let claim = AcornValue::new_forall(
                        forall_types,
                        AcornValue::new_implies(equality, conjunction),
                    );
                    self.add_node(
                        project,
                        true,
                        Proposition::definition(claim, self.module_id, range, is.name.clone()),
                        None,
                    );
                }

                // Structural induction.
                // The type for the inductive hypothesis.
                let hyp_type =
                    AcornType::new_functional(vec![inductive_type.clone()], AcornType::Bool);
                // x0 represents the inductive hypothesis.
                // Think of the inductive principle as (conjunction) -> (conclusion).
                // The conjunction is a case for each constructor.
                // The conclusion is that x0 holds for all items of the type.
                let mut conjunction_parts = vec![];
                for (i, constructor_fn) in constructor_fns.iter().enumerate() {
                    let (_, arg_types) = &constructors[i];
                    let mut args = vec![];
                    let mut conditions = vec![];
                    for (j, arg_type) in arg_types.iter().enumerate() {
                        // x0 is the inductive hypothesis so we start at 1 for the
                        // constructor arguments.
                        let id = (j + 1) as AtomId;
                        args.push(AcornValue::Variable(id, arg_type.clone()));
                        if arg_type == &inductive_type {
                            // The inductive case for this constructor includes a condition
                            // that the inductive hypothesis holds for this argument.
                            conditions.push(AcornValue::new_apply(
                                AcornValue::Variable(0, hyp_type.clone()),
                                vec![AcornValue::Variable(id, arg_type.clone())],
                            ));
                        }
                    }

                    let new_instance = AcornValue::new_apply(constructor_fn.clone(), args);
                    let instance_claim = AcornValue::new_apply(
                        AcornValue::Variable(0, hyp_type.clone()),
                        vec![new_instance],
                    );
                    let unbound = if conditions.is_empty() {
                        // This is a base case. We just need to show that the inductive hypothesis
                        // holds for this constructor.
                        instance_claim
                    } else {
                        // This is an inductive case. Given the conditions, we show that
                        // the inductive hypothesis holds for this constructor.
                        AcornValue::new_implies(
                            AcornValue::reduce(BinaryOp::And, conditions),
                            instance_claim,
                        )
                    };
                    let conjunction_part = AcornValue::new_forall(arg_types.clone(), unbound);
                    conjunction_parts.push(conjunction_part);
                }
                let conjunction = AcornValue::reduce(BinaryOp::And, conjunction_parts);
                let conclusion = AcornValue::new_forall(
                    vec![inductive_type.clone()],
                    AcornValue::new_apply(
                        AcornValue::Variable(0, hyp_type.clone()),
                        vec![AcornValue::Variable(1, inductive_type.clone())],
                    ),
                );
                let unbound_claim = AcornValue::new_implies(conjunction, conclusion);

                // The lambda form is the functional form, which we bind in the environment.
                let name = format!("{}.induction", is.name);
                let lambda_claim =
                    AcornValue::new_lambda(vec![hyp_type.clone()], unbound_claim.clone());
                self.bindings.add_constant(
                    &name,
                    vec![],
                    lambda_claim.get_type(),
                    Some(lambda_claim),
                );
                self.bindings.mark_as_theorem(&name);

                // The forall form is the anonymous truth of induction. We add that as a proposition.
                let forall_claim = AcornValue::new_forall(vec![hyp_type], unbound_claim);
                self.add_node(
                    project,
                    true,
                    Proposition::theorem(true, forall_claim, self.module_id, range, name),
                    None,
                );

                Ok(())
            }

            StatementInfo::Import(is) => {
                self.add_other_lines(statement);

                // Give a local name to the imported module
                let local_name = is.components.last().unwrap();
                if self.bindings.name_in_use(local_name) {
                    return Err(Error::new(
                        &statement.first_token,
                        &format!(
                            "imported name '{}' already defined in this scope",
                            local_name
                        ),
                    ));
                }
                let full_name = is.components.join(".");
                let module_id = match project.load_module(&full_name) {
                    Ok(module_id) => module_id,
                    Err(LoadError(s)) => {
                        // The error is with the import statement itself, like a circular import.
                        return Err(Error::new(
                            &statement.first_token,
                            &format!("import error: {}", s),
                        ));
                    }
                };
                if project.get_bindings(module_id).is_none() {
                    // The fundamental error is in the other module, not this one.
                    return Err(Error::external(
                        &statement.first_token,
                        &format!("error in '{}' module", full_name),
                    ));
                }
                self.bindings.add_module(local_name, module_id);

                // Bring the imported names into this environment
                for name in &is.names {
                    if self.bindings.import_name(project, module_id, name)? {
                        self.definition_ranges
                            .insert(name.to_string(), statement.range());
                        self.add_identity_props(project, name.text());
                    }
                }

                Ok(())
            }

            StatementInfo::Class(cs) => {
                self.add_other_lines(statement);
                match self.bindings.get_type_for_name(&cs.name) {
                    Some(AcornType::Data(module, name)) => {
                        if module != &self.module_id {
                            return Err(Error::new(
                                &cs.name_token,
                                "we can only bind members to types in the current module",
                            ));
                        }
                        if name != &cs.name {
                            return Err(Error::new(
                                &cs.name_token,
                                "we cannot bind members to type aliases",
                            ));
                        }
                    }
                    Some(_) => {
                        return Err(Error::new(
                            &cs.name_token,
                            &format!("we can only bind members to data types"),
                        ));
                    }
                    None => {
                        return Err(Error::new(
                            &cs.name_token,
                            &format!("undefined type name '{}'", cs.name),
                        ));
                    }
                };
                for substatement in &cs.body.statements {
                    match &substatement.statement {
                        StatementInfo::Let(ls) => {
                            self.add_let_statement(
                                project,
                                Some(&cs.name),
                                ls,
                                substatement.range(),
                            )?;
                        }
                        StatementInfo::Define(ds) => {
                            self.add_define_statement(
                                project,
                                Some(&cs.name),
                                ds,
                                substatement.range(),
                            )?;
                        }
                        _ => {
                            return Err(Error::new(
                                &substatement.first_token,
                                "only let and define statements are allowed in class bodies",
                            ));
                        }
                    }
                }
                Ok(())
            }

            StatementInfo::Numerals(ds) => {
                self.add_other_lines(statement);
                let acorn_type = self.bindings.evaluate_type(project, &ds.type_expr)?;
                if let AcornType::Data(module, typename) = acorn_type {
                    self.bindings.set_default(module, typename);
                    Ok(())
                } else {
                    Err(Error::new(
                        &ds.type_expr.token(),
                        "numerals type must be a data type",
                    ))
                }
            }

            StatementInfo::Solve(ss) => {
                let target = self.bindings.evaluate_value(project, &ss.target, None)?;
                let solve_range = Range {
                    start: statement.first_token.start_pos(),
                    end: ss.target.last_token().end_pos(),
                };

                let mut block = self.new_block(
                    project,
                    vec![],
                    vec![],
                    BlockParams::Solve(target.clone(), solve_range),
                    statement.first_line(),
                    statement.last_line(),
                    Some(&ss.body),
                )?;

                let prop = match block.solves(self, &target) {
                    Some((outer_claim, claim_range)) => {
                        block.goal = None;
                        Proposition::anonymous(outer_claim, self.module_id, claim_range)
                    }
                    None => {
                        // The block doesn't contain a solution.
                        // So, it has no claim that can be exported. It doesn't really make sense
                        // to export whatever the last proposition is.
                        // A lot of code expects something, though, so put a vacuous "true" in here.
                        Proposition::anonymous(
                            AcornValue::Bool(true),
                            self.module_id,
                            statement.range(),
                        )
                    }
                };

                let index = self.add_node(project, false, prop, Some(block));
                self.add_prop_lines(index, statement);
                Ok(())
            }

            StatementInfo::Problem(body) => {
                let block = self.new_block(
                    project,
                    vec![],
                    vec![],
                    BlockParams::Problem,
                    statement.first_line(),
                    statement.last_line(),
                    Some(body),
                )?;

                // It would be nice to not have to make a vacuous "true" proposition here.
                let vacuous_prop = Proposition::anonymous(
                    AcornValue::Bool(true),
                    self.module_id,
                    statement.range(),
                );

                let index = self.add_node(project, false, vacuous_prop, Some(block));
                self.add_prop_lines(index, statement);
                Ok(())
            }
        }
    }

    // Adds a possibly multi-line statement to the environment.
    // Panics on failure.
    #[cfg(test)]
    pub fn add(&mut self, input: &str) {
        let tokens = Token::scan(input);
        if let Err(e) = self.add_tokens(&mut Project::new_mock(), tokens) {
            panic!("error in add_tokens: {}", e);
        }
    }

    // Parse these tokens and add them to the environment.
    // If project is not provided, we won't be able to handle import statements.
    pub fn add_tokens(&mut self, project: &mut Project, tokens: Vec<Token>) -> token::Result<()> {
        let mut tokens = TokenIter::new(tokens);
        loop {
            match Statement::parse(&mut tokens, false) {
                Ok((Some(statement), _)) => {
                    if let Err(e) = self.add_statement(project, &statement) {
                        return Err(e);
                    }
                }
                Ok((None, _)) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    // Will return a context for a subenvironment if this theorem has a block
    pub fn get_theorem_context(&self, theorem_name: &str) -> GoalContext {
        for (i, p) in self.nodes.iter().enumerate() {
            if let Some(name) = p.claim.name() {
                if name == theorem_name {
                    return self.get_goal_context(&vec![i]).unwrap();
                }
            }
        }
        panic!("no top-level theorem named {}", theorem_name);
    }

    // The "path" to a goal is a list of indices to recursively go into env.nodes.
    // This returns a path for all nodes that correspond to a goal within this environment,
    // or subenvironments, recursively.
    // The order is "proving order", ie the goals inside the block are proved before the
    // root goal of a block.
    pub fn goal_paths(&self) -> Vec<Vec<usize>> {
        self.goal_paths_helper(&vec![])
    }

    // Find all goal paths from this environment, prepending 'prepend' to each path.
    fn goal_paths_helper(&self, prepend: &Vec<usize>) -> Vec<Vec<usize>> {
        let mut paths = Vec::new();
        for (i, prop) in self.nodes.iter().enumerate() {
            if prop.structural {
                continue;
            }
            let path = {
                let mut path = prepend.clone();
                path.push(i);
                path
            };
            if let Some(block) = &prop.block {
                let mut subpaths = block.env.goal_paths_helper(&path);
                paths.append(&mut subpaths);
                if block.goal.is_some() {
                    paths.push(path);
                }
            } else {
                paths.push(path);
            }
        }
        paths
    }

    // Get all facts from this environment.
    pub fn get_facts(&self) -> Vec<Proposition> {
        let mut facts = Vec::new();
        for prop in &self.nodes {
            facts.push(prop.claim.clone());
        }
        facts
    }

    // Gets the proposition at a certain path.
    pub fn get_proposition(&self, path: &Vec<usize>) -> Option<&Proposition> {
        let mut env = self;
        let mut it = path.iter().peekable();
        while let Some(i) = it.next() {
            if it.peek().is_none() {
                return env.nodes.get(*i).map(|p| &p.claim);
            }
            let prop = env.nodes.get(*i)?;
            if let Some(block) = &prop.block {
                env = &block.env;
            } else {
                return None;
            }
        }
        None
    }

    fn make_goal_context(
        &self,
        global_facts: Vec<Proposition>,
        local_facts: Vec<Proposition>,
        goal: Goal,
        proof_insertion_line: u32,
    ) -> GoalContext {
        let name = match &goal {
            Goal::Prove(proposition) => match proposition.name() {
                Some(name) => name.to_string(),
                None => self
                    .bindings
                    .value_to_code(&proposition.value, &mut 0)
                    .unwrap(),
            },
            Goal::Solve(value, _) => {
                let value_str = self.bindings.value_to_code(value, &mut 0).unwrap();
                format!("solve {}", value_str)
            }
        };
        GoalContext::new(
            &self,
            global_facts,
            local_facts,
            name,
            goal,
            proof_insertion_line,
        )
    }

    // Get a list of facts that are available at a certain path, along with the proposition
    // that should be proved there.
    pub fn get_goal_context(&self, path: &Vec<usize>) -> Result<GoalContext, String> {
        let mut global_facts = vec![];
        let mut local_facts = vec![];
        let mut env = self;
        let mut it = path.iter().peekable();
        let mut global = true;
        while let Some(i) = it.next() {
            for previous_prop in &env.nodes[0..*i] {
                let fact = previous_prop.claim.clone();
                if global {
                    global_facts.push(fact);
                } else {
                    local_facts.push(fact);
                }
            }
            global = false;
            let node = match env.nodes.get(*i) {
                Some(p) => p,
                None => return Err(format!("no node at path {:?}", path)),
            };
            if let Some(block) = &node.block {
                if it.peek().is_none() {
                    // This is the last element of the path. It has a block, so we can use the
                    // contents of the block to help prove it.
                    for p in &block.env.nodes {
                        local_facts.push(p.claim.clone());
                    }
                    let goal = match &block.goal {
                        Some(goal) => goal,
                        None => return Err(format!("block at path {:?} has no goal", path)),
                    };

                    return Ok(block.env.make_goal_context(
                        global_facts,
                        local_facts,
                        goal.clone(),
                        block.env.last_line(),
                    ));
                }
                env = &block.env;
            } else {
                // If there's no block on this prop, this must be the last element of the path
                assert!(it.peek().is_none());

                return Ok(env.make_goal_context(
                    global_facts,
                    local_facts,
                    Goal::Prove(node.claim.clone()),
                    node.claim.source.range.start.line,
                ));
            }
        }
        panic!("control should not get here");
    }

    pub fn get_goal_context_by_name(&self, name: &str) -> GoalContext {
        let paths = self.goal_paths();
        let mut names = Vec::new();
        for path in paths {
            let context = self.get_goal_context(&path).unwrap();
            if context.name == name {
                return context;
            }
            names.push(context.name);
        }

        panic!("no context found for {} in:\n{}\n", name, names.join("\n"));
    }

    // Returns the path corresponding to the goal for a given zero-based line.
    // This is a UI heuristic.
    // Either returns a path to a proposition, or an error message explaining why this line
    // is unusable. Error messages use one-based line numbers.
    pub fn get_path_for_line(&self, line: u32) -> Result<Vec<usize>, String> {
        let mut path = vec![];
        let mut block: Option<&Block> = None;
        let mut env = self;
        loop {
            match env.get_line_type(line) {
                Some(LineType::Node(i)) => {
                    path.push(i);
                    let prop = &env.nodes[i];
                    if prop.claim.source.is_axiom() {
                        return Err(format!("line {} is an axiom", line + 1));
                    }
                    match &prop.block {
                        Some(b) => {
                            block = Some(b);
                            env = &b.env;
                            continue;
                        }
                        None => {
                            return Ok(path);
                        }
                    }
                }
                Some(LineType::Opening) | Some(LineType::Closing) => match block {
                    Some(block) => {
                        if block.goal.is_none() {
                            return Err(format!("no claim for block at line {}", line + 1));
                        }
                        return Ok(path);
                    }
                    None => return Err(format!("brace but no block, line {}", line + 1)),
                },
                Some(LineType::Other) => return Err(format!("line {} is not a prop", line + 1)),
                None => return Err(format!("line {} is out of range", line + 1)),
                Some(LineType::Empty) => {
                    // We let the user insert a proof in an area by clicking on an empty
                    // line where the proof would go.
                    // To find the statement we're proving, we "slide" into the next prop.
                    let mut slide = line;
                    loop {
                        slide += 1;
                        match env.get_line_type(slide) {
                            Some(LineType::Node(i)) => {
                                let prop = &env.nodes[i];
                                if prop.claim.source.is_axiom() {
                                    return Err(format!("slide to axiom, line {}", slide + 1));
                                }
                                if prop.block.is_none() {
                                    path.push(i);
                                    return Ok(path);
                                }
                                // We can't slide into a block, because the proof would be
                                // inserted into the block, rather than here.
                                return Err(format!("blocked slide {} -> {}", line + 1, slide + 1));
                            }
                            Some(LineType::Empty) => {
                                // Keep sliding
                                continue;
                            }
                            Some(LineType::Closing) => {
                                // Sliding into the end of our block is okay
                                match block {
                                    Some(block) => {
                                        if block.goal.is_none() {
                                            return Err("slide to end but no claim".to_string());
                                        }
                                        return Ok(path);
                                    }
                                    None => {
                                        return Err(format!(
                                            "close brace but no block, line {}",
                                            slide + 1
                                        ))
                                    }
                                }
                            }
                            Some(LineType::Opening) => {
                                return Err(format!("slide to open brace, line {}", slide + 1));
                            }
                            Some(LineType::Other) => {
                                return Err(format!("slide to non-prop {}", slide + 1));
                            }
                            None => return Err(format!("slide to end, line {}", slide + 1)),
                        }
                    }
                }
            }
        }
    }

    pub fn covers_line(&self, line: u32) -> bool {
        if line < self.first_line {
            return false;
        }
        if line >= self.next_line() {
            return false;
        }
        true
    }

    // Makes sure the lines are self-consistent
    #[cfg(test)]
    fn check_lines(&self) {
        // Check that each proposition's block covers the lines it claims to cover
        for (line, line_type) in self.line_types.iter().enumerate() {
            if let LineType::Node(prop_index) = line_type {
                let prop = &self.nodes[*prop_index];
                if let Some(block) = &prop.block {
                    assert!(block.env.covers_line(line as u32));
                }
            }
        }

        // Recurse
        for prop in &self.nodes {
            if let Some(block) = &prop.block {
                block.env.check_lines();
            }
        }
    }

    // Expects the given line to be bad
    #[cfg(test)]
    fn bad(&mut self, input: &str) {
        if let Ok(statement) = Statement::parse_str(input) {
            assert!(
                self.add_statement(&mut Project::new_mock(), &statement)
                    .is_err(),
                "expected error in: {}",
                input
            );
        }
    }

    // Check that the given name actually does have this type in the environment.
    #[cfg(test)]
    pub fn expect_type(&mut self, name: &str, type_string: &str) {
        self.bindings.expect_type(name, type_string)
    }

    // Check that the given name is defined to be this value
    #[cfg(test)]
    fn expect_def(&mut self, name: &str, value_string: &str) {
        let env_value = match self.bindings.get_definition(name) {
            Some(t) => t,
            None => panic!("{} not found in environment", name),
        };
        assert_eq!(env_value.to_string(), value_string);
    }

    // Assert that these two names are defined to equal the same thing
    #[cfg(test)]
    fn assert_def_eq(&self, name1: &str, name2: &str) {
        let def1 = self.bindings.get_definition(name1).unwrap();
        let def2 = self.bindings.get_definition(name2).unwrap();
        assert_eq!(def1, def2);
    }

    // Assert that these two names are defined to be different things
    #[cfg(test)]
    fn assert_def_ne(&self, name1: &str, name2: &str) {
        let def1 = self.bindings.get_definition(name1).unwrap();
        let def2 = self.bindings.get_definition(name2).unwrap();
        assert_ne!(def1, def2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fn_equality() {
        let mut env = Environment::new_test();
        env.add("define idb1(x: Bool) -> Bool { x }");
        env.expect_type("idb1", "Bool -> Bool");
        env.add("define idb2(y: Bool) -> Bool { y }");
        env.expect_type("idb2", "Bool -> Bool");
        env.assert_def_eq("idb1", "idb2");

        env.add("type Nat: axiom");
        env.add("define idn1(x: Nat) -> Nat { x }");
        env.expect_type("idn1", "Nat -> Nat");
        env.assert_def_ne("idb1", "idn1");
    }

    #[test]
    fn test_forall_equality() {
        let mut env = Environment::new_test();
        env.add("let bsym1: Bool = forall(x: Bool) { x = x }");
        env.expect_type("bsym1", "Bool");
        env.add("let bsym2: Bool = forall(y: Bool) { y = y }");
        env.expect_type("bsym2", "Bool");
        env.assert_def_eq("bsym1", "bsym2");

        env.add("type Nat: axiom");
        env.add("let nsym1: Bool = forall(x: Nat) { x = x }");
        env.expect_type("nsym1", "Bool");
        env.assert_def_ne("bsym1", "nsym1");
    }

    #[test]
    fn test_exists_equality() {
        let mut env = Environment::new_test();
        env.add("let bex1: Bool = exists(x: Bool) { x = x }");
        env.add("let bex2: Bool = exists(y: Bool) { y = y }");
        env.assert_def_eq("bex1", "bex2");

        env.add("type Nat: axiom");
        env.add("let nex1: Bool = exists(x: Nat) { x = x }");
        env.assert_def_ne("bex1", "nex1");
    }

    #[test]
    fn test_arg_binding() {
        let mut env = Environment::new_test();
        env.bad("define qux(x: Bool, x: Bool) -> Bool { x }");
        assert!(!env.bindings.has_identifier("x"));
        env.add("define qux(x: Bool, y: Bool) -> Bool { x }");
        env.expect_type("qux", "(Bool, Bool) -> Bool");

        env.bad("theorem foo(x: Bool, x: Bool) { x }");
        assert!(!env.bindings.has_identifier("x"));
        env.add("theorem foo(x: Bool, y: Bool) { x }");
        env.expect_type("foo", "(Bool, Bool) -> Bool");

        env.bad("let bar: Bool = forall(x: Bool, x: Bool) { x = x }");
        assert!(!env.bindings.has_identifier("x"));
        env.add("let bar: Bool = forall(x: Bool, y: Bool) { x = x }");

        env.bad("let baz: Bool = exists(x: Bool, x: Bool) { x = x }");
        assert!(!env.bindings.has_identifier("x"));
        env.add("let baz: Bool = exists(x: Bool, y: Bool) { x = x }");
    }

    #[test]
    fn test_no_double_grouped_arg_list() {
        let mut env = Environment::new_test();
        env.add("define foo(x: Bool, y: Bool) -> Bool { x }");
        env.add("let b: Bool = axiom");
        env.bad("foo((b, b))");
    }

    #[test]
    fn test_argless_theorem() {
        let mut env = Environment::new_test();
        env.add("let b: Bool = axiom");
        env.add("theorem foo { b or not b }");
        env.expect_def("foo", "(b or not b)");
    }

    #[test]
    fn test_forall_value() {
        let mut env = Environment::new_test();
        env.add("let p: Bool = forall(x: Bool) { x or not x }");
        env.expect_def("p", "forall(x0: Bool) { (x0 or not x0) }");
    }

    #[test]
    fn test_inline_function_value() {
        let mut env = Environment::new_test();
        env.add("define ander(a: Bool) -> (Bool -> Bool) { function(b: Bool) { a and b } }");
        env.expect_def(
            "ander",
            "function(x0: Bool) { function(x1: Bool) { (x0 and x1) } }",
        );
    }

    #[test]
    fn test_empty_if_block() {
        let mut env = Environment::new_test();
        env.add("let b: Bool = axiom");
        env.add("if b {}");
    }

    #[test]
    fn test_empty_forall_statement() {
        // Allowed as statement but not as an expression.
        let mut env = Environment::new_test();
        env.add("forall(b: Bool) {}");
    }

    #[test]
    fn test_nat_ac_piecewise() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.add("let zero: Nat = axiom");
        env.add("let suc: Nat -> Nat = axiom");
        env.add("let one: Nat = suc(zero)");
        env.expect_def("one", "suc(zero)");

        env.add("axiom suc_injective(x: Nat, y: Nat) { suc(x) = suc(y) -> x = y }");
        env.expect_type("suc_injective", "(Nat, Nat) -> Bool");
        env.expect_def(
            "suc_injective",
            "function(x0: Nat, x1: Nat) { ((suc(x0) = suc(x1)) -> (x0 = x1)) }",
        );

        env.add("axiom suc_neq_zero(x: Nat) { suc(x) != zero }");
        env.expect_def("suc_neq_zero", "function(x0: Nat) { (suc(x0) != zero) }");

        assert!(env.bindings.has_type_name("Nat"));
        assert!(!env.bindings.has_identifier("Nat"));

        assert!(!env.bindings.has_type_name("zero"));
        assert!(env.bindings.has_identifier("zero"));

        assert!(!env.bindings.has_type_name("one"));
        assert!(env.bindings.has_identifier("one"));

        assert!(!env.bindings.has_type_name("suc"));
        assert!(env.bindings.has_identifier("suc"));

        assert!(!env.bindings.has_type_name("foo"));
        assert!(!env.bindings.has_identifier("foo"));

        env.add(
            "axiom induction(f: Nat -> Bool, n: Nat) {
            f(zero) and forall(k: Nat) { f(k) -> f(suc(k)) } -> f(n) }",
        );
        env.expect_def("induction", "function(x0: Nat -> Bool, x1: Nat) { ((x0(zero) and forall(x2: Nat) { (x0(x2) -> x0(suc(x2))) }) -> x0(x1)) }");

        env.add("define recursion(f: Nat -> Nat, a: Nat, n: Nat) -> Nat { axiom }");
        env.expect_type("recursion", "(Nat -> Nat, Nat, Nat) -> Nat");

        env.add("axiom recursion_base(f: Nat -> Nat, a: Nat) { recursion(f, a, zero) = a }");
        env.add(
            "axiom recursion_step(f: Nat -> Nat, a: Nat, n: Nat) {
            recursion(f, a, suc(n)) = f(recursion(f, a, n)) }",
        );

        env.add("define add(a: Nat, b: Nat) -> Nat { recursion(suc, a, b) }");
        env.expect_type("add", "(Nat, Nat) -> Nat");

        env.add("theorem add_zero_right(a: Nat) { add(a, zero) = a }");
        env.add("theorem add_zero_left(a: Nat) { add(zero, a) = a }");
        env.add("theorem add_suc_right(a: Nat, b: Nat) { add(a, suc(b)) = suc(add(a, b)) }");
        env.add("theorem add_suc_left(a: Nat, b: Nat) { add(suc(a), b) = suc(add(a, b)) }");
        env.add("theorem add_comm(a: Nat, b: Nat) { add(a, b) = add(b, a) }");
        env.add(
            "theorem add_assoc(a: Nat, b: Nat, c: Nat) { add(add(a, b), c) = add(a, add(b, c)) }",
        );
        env.add("theorem not_suc_eq_zero(x: Nat) { not (suc(x) = zero) }");
    }

    #[test]
    fn test_nat_ac_together() {
        let mut env = Environment::new_test();
        env.add(
            r#"
// The axioms of Peano arithmetic.
        
type Nat: axiom

let zero: Nat = axiom

let suc: Nat -> Nat = axiom
let one: Nat = suc(zero)

axiom suc_injective(x: Nat, y: Nat) { suc(x) = suc(y) -> x = y }

axiom suc_neq_zero(x: Nat) { suc(x) != zero }

axiom induction(f: Nat -> Bool) { f(zero) and forall(k: Nat) { f(k) -> f(suc(k)) } -> forall(n: Nat) { f(n) } }

// The old version. In the modern codebase these are parametric.
define recursion(f: Nat -> Nat, a: Nat, n: Nat) -> Nat { axiom }
axiom recursion_base(f: Nat -> Nat, a: Nat) { recursion(f, a, zero) = a }
axiom recursion_step(f: Nat -> Nat, a: Nat, n: Nat) { recursion(f, a, suc(n)) = f(recursion(f, a, n)) }

define add(a: Nat, b: Nat) -> Nat { recursion(suc, a, b) }

// Now let's have some theorems.

theorem add_zero_right(a: Nat) { add(a, zero) = a }

theorem add_zero_left(a: Nat) { add(zero, a) = a }

theorem add_suc_right(a: Nat, b: Nat) { add(a, suc(b)) = suc(add(a, b)) }

theorem add_suc_left(a: Nat, b: Nat) { add(suc(a), b) = suc(add(a, b)) }

theorem add_comm(a: Nat, b: Nat) { add(a, b) = add(b, a) }

theorem add_assoc(a: Nat, b: Nat, c: Nat) { add(add(a, b), c) = add(a, add(b, c)) }
"#,
        );
    }

    #[test]
    fn test_names_in_subenvs() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            theorem foo(a: Nat, b: Nat) { a = b } by {
                let c: Nat = a
                define d(e: Nat) -> Bool { foo(e, b) }
            }
            "#,
        );
        env.check_lines();
    }

    #[test]
    fn test_forall_subenv() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            forall(x: Nat) {
                x = x
            }
            "#,
        );
    }

    #[test]
    fn test_if_subenv() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            let zero: Nat = axiom
            if zero = zero {
                zero = zero
            }
            "#,
        )
    }

    #[test]
    fn test_let_satisfy_exports_names() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            define foo(x: Nat) -> Bool { axiom }
            theorem goal { true } by {
                let z: Nat satisfy { foo(z) }
                foo(z)
            }
        "#,
        );
    }

    #[test]
    fn test_environment_with_function_satisfy() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            let flip(a: Bool) -> b: Bool satisfy {
                a != b
            }
        "#,
        );
    }

    #[test]
    fn test_if_block_ending_with_exists() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/main.ac",
            r#"
            let a: Bool = axiom
            theorem goal { a } by {
                if a {
                    exists(b: Bool) { b = b }
                }
            }
            "#,
        );
        let module = p.expect_ok("main");
        let env = p.get_env(module).unwrap();
        for path in env.goal_paths() {
            env.get_goal_context(&path).unwrap();
        }
    }

    #[test]
    fn test_forall_block_ending_with_exists() {
        let mut p = Project::new_mock();
        p.mock(
            "/mock/main.ac",
            r#"
            let a: Bool = axiom
            theorem goal { a } by {
                forall(b: Bool) {
                    exists(c: Bool) { c = c }
                }
            }
            "#,
        );
        let module = p.expect_ok("main");
        let env = p.get_env(module).unwrap();
        for path in env.goal_paths() {
            env.get_goal_context(&path).unwrap();
        }
    }

    #[test]
    fn test_structure_new_definition() {
        let mut env = Environment::new_test();
        env.add(
            r#"
        structure BoolPair {
            first: Bool
            second: Bool
        }
        theorem goal(p: BoolPair) {
            p = BoolPair.new(BoolPair.first(p), BoolPair.second(p))
        }
        "#,
        );
    }

    #[test]
    fn test_structure_cant_contain_itself() {
        // If you want a type to contain itself, it has to be inductive, not a structure.
        let mut env = Environment::new_test();
        env.bad(
            r#"
        structure InfiniteBools {
            head: Bool
            tail: InfiniteBools
        }
        "#,
        );
    }

    #[test]
    fn test_inductive_new_definition() {
        let mut env = Environment::new_test();
        env.add(
            r#"
        inductive Nat {
            zero
            suc(Nat)
        }
        theorem goal(n: Nat) {
            n = Nat.zero or exists(k: Nat) { n = Nat.suc(k) }
        }
        "#,
        );
    }

    #[test]
    fn test_inductive_constructor_can_be_member() {
        let mut env = Environment::new_test();
        env.add(
            r#"
        inductive Nat {
            zero
            suc(Nat)
        }
        theorem goal(n: Nat) {
            n = Nat.zero or exists(k: Nat) { n = k.suc }
        }
        "#,
        );
    }

    #[test]
    fn test_inductive_statements_must_have_base_case() {
        let mut env = Environment::new_test();
        env.bad(
            r#"
        inductive Nat {
            suc(Nat)
        }"#,
        );
    }

    #[test]
    fn test_no_russell_paradox() {
        let mut env = Environment::new_test();
        env.bad(
            r#"
        structure NaiveSet {
            set: NaiveSet -> Bool 
        }
        "#,
        );
    }

    #[test]
    fn test_parametric_types_required_in_function_args() {
        let mut env = Environment::new_test();
        env.bad("define foo<T>(a: Bool) -> Bool { a }");
    }

    #[test]
    fn test_parametric_types_required_in_theorem_args() {
        let mut env = Environment::new_test();
        env.bad("theorem foo<T>(a: Bool) { a or not a }");
    }

    #[test]
    fn test_template_typechecking() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.add("let zero: Nat = axiom");
        env.add("define eq<T>(a: T, b: T) -> Bool { a = b }");
        env.add("theorem t1 { eq(zero, zero) }");
        env.add("theorem t2 { eq(zero = zero, zero = zero) }");
        env.add("theorem t3 { eq(zero = zero, eq(zero, zero)) }");
        env.bad("theorem t4 { eq(zero, zero = zero) }");
        env.bad("theorem t5 { zero = eq(zero, zero) }");
    }

    #[test]
    fn test_type_params_cleaned_up() {
        let mut env = Environment::new_test();
        env.add("define foo<T>(a: T) -> Bool { axiom }");
        assert!(env.bindings.get_type_for_name("T").is_none());
    }

    #[test]
    fn test_if_condition_must_be_bool() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.add("let zero: Nat = axiom");
        env.add("let b: Bool = axiom");
        env.add("if b { zero = zero }");
        env.bad("if zero { zero = zero }");
    }

    #[test]
    fn test_reusing_type_name_as_var_name() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("let Nat: Bool = axiom");
    }

    #[test]
    fn test_reusing_var_name_as_type_name() {
        let mut env = Environment::new_test();
        env.add("let x: Bool = axiom");
        env.bad("type x: axiom");
    }

    #[test]
    fn test_reusing_type_name_as_fn_name() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("define Nat(x: Bool) -> Bool { x }");
    }

    #[test]
    fn test_reusing_type_name_as_theorem_name() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("theorem Nat(x: Bool): x = x");
    }

    #[test]
    fn test_reusing_type_name_as_exists_arg() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("let b: Bool = exists(x: Bool, Nat: Bool) { x = x }");
    }

    #[test]
    fn test_reusing_type_name_as_forall_arg() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("let b: Bool = forall(x: Bool, Nat: Bool) { x = x }");
    }

    #[test]
    fn test_reusing_type_name_as_lambda_arg() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("let f: (bool, bool) -> Bool = function(x: Bool, Nat: Bool) { x = x }");
    }

    #[test]
    fn test_parsing_true_false_keywords() {
        let mut env = Environment::new_test();
        env.add("let b: Bool = true or false");
    }

    #[test]
    fn test_nothing_after_explicit_false() {
        let mut env = Environment::new_test();
        env.add("let b: Bool = axiom");
        env.bad(
            r#"
            if b = not b {
                false
                b
            }
        "#,
        );
    }

    #[test]
    fn test_condition_must_be_valid() {
        let mut env = Environment::new_test();
        env.bad(
            r#"
            if a {
            }
        "#,
        );
    }

    #[test]
    fn test_inline_function_in_forall_block() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.add("let zero: Nat = axiom");
        env.add("let suc: Nat -> Nat = axiom");
        env.add(
            r#"
            axiom induction(f: Nat -> Bool) {
                f(zero) and forall(k: Nat) { f(k) -> f(suc(k)) } -> forall(n: Nat) { f(n) }
            }
            "#,
        );
        env.add(
            r#"
            forall(f: (Nat, Bool) -> Bool) {
                induction(function(x: Nat) { f(x, true) })
            }
        "#,
        );
    }

    #[test]
    fn test_structs_must_be_capitalized() {
        let mut env = Environment::new_test();
        env.bad(
            r#"
            struct foo {
                bar: Bool
            }
        "#,
        );
    }

    #[test]
    fn test_axiomatic_types_must_be_capitalized() {
        let mut env = Environment::new_test();
        env.bad("type foo: axiom");
    }

    #[test]
    fn test_functional_definition_typechecking() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("define foo(f: Nat -> Nat) -> Bool { function(x: Nat) { true } }");
    }

    #[test]
    fn test_partial_application() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.add("let zero: Nat = axiom");
        env.add("define add3(a: Nat, b: Nat, c: Nat) -> Nat { axiom }");
        env.add("let add0: (Nat, Nat) -> Nat = add3(zero)");
        env.add("let add00: Nat -> Nat = add3(zero, zero)");
        env.add("let add00_alt: Nat -> Nat = add0(zero)");
    }

    #[test]
    fn test_else_on_new_line() {
        // This is ugly but it should work.
        let mut env = Environment::new_test();
        env.add(
            r#"
        let b: Bool = axiom
        if b {
            b
        }
        else {
            not b
        }
        "#,
        );
    }

    #[test]
    fn test_arg_names_lowercased() {
        let mut env = Environment::new_test();
        env.bad("let f: Bool -> Bool = function(A: Bool) { true }");
        env.add("let f: Bool -> Bool = function(a: Bool) { true }");
        env.bad("forall(A: Bool) { true }");
        env.add("forall(a: Bool) { true }");
        env.bad("define foo(X: Bool) -> Bool { true }");
        env.add("define foo(x: Bool) -> Bool { true }");
        env.bad("theorem bar(X: Bool) { true }");
        env.add("theorem bar(x: Bool) { true }");
    }

    #[test]
    fn test_undefined_class_name() {
        let mut env = Environment::new_test();
        env.bad("class Foo {}");
    }

    #[test]
    fn test_class_variables() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                let zero: Nat = axiom
                let 1: Nat = axiom
            }

            axiom zero_neq_one(x: Nat) { Nat.zero = Nat.1 }
        "#,
        );

        // Class variables shouldn't get bound at module scope
        env.bad("let alsozero: Nat = zero");
    }

    #[test]
    fn test_instance_methods() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define add(self, other: Nat) -> Nat { axiom }
            }
        "#,
        );
    }

    #[test]
    fn test_no_methods_on_type_aliases() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.add("type NatFn: Nat -> Nat");
        env.bad("class NatFn {}");
    }

    #[test]
    fn test_first_arg_must_be_self() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad(
            r#"
            class Nat {
                define add(a: Nat, b: Nat) -> Nat { axiom }
            }
            "#,
        );
    }

    #[test]
    fn test_no_self_variables() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("let foo: Bool = exists(self) { true }");
        env.bad("let foo: Bool = forall(self) { true }");
        env.bad("let self: Nat = axiom");
    }

    #[test]
    fn test_no_self_args_outside_class() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("define foo(self) -> Bool { true }");
    }

    #[test]
    fn test_no_self_as_forall_arg() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("forall(self) { true }");
    }

    #[test]
    fn test_no_self_as_exists_arg() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("exists(self) { true }");
    }

    #[test]
    fn test_no_self_as_lambda_arg() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("let f: Nat -> Bool = lambda(self) { true }");
    }

    #[test]
    fn test_using_member_function() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define add(self, other: Nat) -> Nat { axiom }
            }
            theorem goal(a: Nat, b: Nat) {
                a.add(b) = b.add(a)
            }
        "#,
        );
    }

    #[test]
    fn test_infix_add() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define add(self, other: Nat) -> Nat { axiom }
            }
            theorem goal(a: Nat, b: Nat) { a + b = b + a }
        "#,
        );
    }

    #[test]
    fn test_infix_sub() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define sub(self, other: Nat) -> Nat { axiom }
            }
            theorem goal(a: Nat, b: Nat) { a - b = b - a }
        "#,
        );
    }

    #[test]
    fn test_infix_mul() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define mul(self, other: Nat) -> Nat { axiom }
            }
            theorem goal(a: Nat, b: Nat) { a * b = b * a }
        "#,
        );
    }

    #[test]
    fn test_infix_div() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define div(self, other: Nat) -> Nat { axiom }
            }
            theorem goal(a: Nat, b: Nat) { a / b = b / a }
        "#,
        );
    }

    #[test]
    fn test_infix_mod() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define mod(self, other: Nat) -> Nat { axiom }
            }
            theorem goal(a: Nat, b: Nat) { a % b = b % a }
        "#,
        );
    }

    #[test]
    fn test_infix_lt() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define lt(self, other: Nat) -> Bool { axiom }
            }
            theorem goal(a: Nat, b: Nat) { a < b = b < a }
        "#,
        );
    }

    #[test]
    fn test_infix_gt() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define gt(self, other: Nat) -> Bool { axiom }
            }
            theorem goal(a: Nat, b: Nat) { a > b = b > a }
        "#,
        );
    }

    #[test]
    fn test_infix_lte() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define lte(self, other: Nat) -> Bool { axiom }
            }
            theorem goal(a: Nat, b: Nat) { a <= b = b <= a }
        "#,
        );
    }

    #[test]
    fn test_infix_gte() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define gte(self, other: Nat) -> Bool { axiom }
            }
            theorem goal(a: Nat, b: Nat) { a >= b = b >= a }
        "#,
        );
    }

    #[test]
    fn test_self_must_have_correct_type() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad(
            r#"
            class Nat {
                define add(self: Bool, other: Nat) -> Nat { axiom }
            }
        "#,
        );
    }

    #[test]
    fn test_no_dot_new() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            structure NatPair {
                first: Nat
                second: Nat
            }
        "#,
        );
        env.bad("theorem goal(p: NatPair): p.new = p.new");
    }

    #[test]
    fn test_no_defining_new() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad(
            r#"
            class Nat {
                define new(self: Bool, other: Nat) -> Bool { true }
            }
        "#,
        );
    }

    #[test]
    fn test_no_using_methods_with_mismatched_self() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            let zero: Nat = axiom
            class Nat {
                let foo: Bool -> Bool = function(b: Bool) { b }
            }
        "#,
        );
        env.bad("theorem goal: zero.foo(true)");
    }

    #[test]
    fn test_infix_codegen() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                define add(self, other: Nat) -> Nat { axiom }
                define sub(self, other: Nat) -> Nat { axiom }
                define mul(self, other: Nat) -> Nat { axiom }
                define div(self, other: Nat) -> Nat { axiom }
                define mod(self, other: Nat) -> Nat { axiom }
                define lt(self, other: Nat) -> Bool { axiom }
                define gt(self, other: Nat) -> Bool { axiom }
                define lte(self, other: Nat) -> Bool { axiom }
                define gte(self, other: Nat) -> Bool { axiom }
                define suc(self) -> Nat { axiom }
                define foo(self, other: Nat) -> Nat { axiom }
                let 0: Nat = axiom
                let 1: Nat = axiom
            }
            numerals Nat
        "#,
        );
        env.bindings.expect_good_code("0 + 1");
        env.bindings.expect_good_code("0 - 1");
        env.bindings.expect_good_code("0 * 1");
        env.bindings.expect_good_code("0 / 1");
        env.bindings.expect_good_code("0 % 1");
        env.bindings.expect_good_code("0 < 1");
        env.bindings.expect_good_code("0 > 1");
        env.bindings.expect_good_code("0 <= 1");
        env.bindings.expect_good_code("0 >= 1");
        env.bindings.expect_good_code("0 + 0 * 0");
        env.bindings.expect_good_code("(0 + 0) * 0");
        env.bindings.expect_good_code("0 + 0 + 0");
        env.bindings.expect_good_code("1 - (1 - 1)");
        env.bindings.expect_good_code("(0 + 1).suc = 1 + 1");
        env.bindings.expect_good_code("1 + 1 * 1");
        env.bindings.expect_good_code("0.suc = 1");
        env.bindings.expect_good_code("0.foo(1)");
    }

    #[test]
    fn test_no_magic_names_for_constants() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad(
            r#"
            class Nat {
                let add: Nat = axiom
            }
        "#,
        );
    }

    #[test]
    fn test_no_magic_names_for_struct_fields() {
        let mut env = Environment::new_test();
        env.bad(
            r#"
            struct MyStruct {
                add: Bool
            }
        "#,
        );
    }

    #[test]
    fn test_numerals_statement() {
        let mut env = Environment::new_test();
        env.add("type Foo: axiom");
        env.add("numerals Foo");
        env.bad("numerals Bar");
        env.bad("numerals Bool");
        env.bad("numerals Foo -> Foo");
    }

    #[test]
    fn test_no_defining_top_level_numbers() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad("let 0: Nat = axiom");
    }

    #[test]
    fn test_no_top_level_numbers_without_a_numerals() {
        let mut env = Environment::new_test();
        env.bad("let foo: Bool = (0 = 0)");
    }

    #[test]
    fn test_multi_digit_unary() {
        let mut env = Environment::new_test();
        env.add("type Unary: axiom");
        env.add(
            r#"
            class Unary {
                let 1: Unary = axiom 
                define suc(self) -> Unary { axiom }
                define read(self, digit: Unary) -> Unary { self.suc }
            }
        "#,
        );
        env.add("numerals Unary");
        env.add("let two: Unary = 11");
    }

    #[test]
    fn test_digits_must_be_correct_type() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad(
            r#"
            class Nat {
                let 1: Bool = axiom
            }
        "#,
        );
    }

    #[test]
    fn test_read_must_have_correct_args() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad(
            r#"
            class Nat {
                let 1: Nat = axiom
                define suc(self) -> Nat: axiom
                define read(self, digit: Bool) -> Nat: Nat.1
            }
        "#,
        );
    }

    #[test]
    fn test_read_must_return_correct_type() {
        let mut env = Environment::new_test();
        env.add("type Nat: axiom");
        env.bad(
            r#"
            class Nat {
                let 1: Nat = axiom
                define suc(self) -> Nat: axiom
                define read(self, digit: Nat) -> Bool: true
            }
        "#,
        );
    }

    #[test]
    fn test_numeric_literal_codegen() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                let 0: Nat = axiom
                define suc(self) -> Nat { axiom }
                let 1: Nat = Nat.0.suc
                let 2: Nat = Nat.1.suc
                let 3: Nat = Nat.2.suc
                let 4: Nat = Nat.3.suc
                let 5: Nat = Nat.4.suc
                let 6: Nat = Nat.5.suc
                let 7: Nat = Nat.6.suc
                let 8: Nat = Nat.7.suc
                let 9: Nat = Nat.8.suc
                let 10: Nat = Nat.9.suc
                define read(self, other: Nat) -> Nat { axiom }
                define add(self, other: Nat) -> Nat { axiom }
            }
            numerals Nat
        "#,
        );
        env.bindings.expect_good_code("7");
        env.bindings.expect_good_code("10");
        env.bindings.expect_good_code("12");
        env.bindings.expect_good_code("123 + 456");
    }

    #[test]
    fn test_non_default_numeric_literals() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            type Nat: axiom
            class Nat {
                let 0: Nat = axiom
                define suc(self) -> Nat { axiom }
                let 1: Nat = Nat.0.suc
                let 2: Nat = Nat.1.suc
                let 3: Nat = Nat.2.suc
                let 4: Nat = Nat.3.suc
                let 5: Nat = Nat.4.suc
                let 6: Nat = Nat.5.suc
                let 7: Nat = Nat.6.suc
                let 8: Nat = Nat.7.suc
                let 9: Nat = Nat.8.suc
                let 10: Nat = Nat.9.suc
                define read(self, other: Nat) -> Nat { axiom }
                define add(self, other: Nat) -> Nat { axiom }
            }
        "#,
        );
        env.bindings.expect_good_code("Nat.7");
        env.bindings.expect_good_code("Nat.10");
        env.bindings.expect_good_code("Nat.12");
        env.bindings.expect_good_code("Nat.123 + Nat.456");
    }

    #[test]
    fn test_root_level_solve() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            let b: Bool = true or false
            solve b by {
                b = true
            }
            "#,
        );
    }

    #[test]
    fn test_nested_solve() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            let b: Bool = true or false
            if b or b {
                solve b by {
                    b = true
                }
            }
            "#,
        );
    }

    #[test]
    fn test_infix_solve() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            let b: Bool = true or false
            solve b or b by {
                b or b = b
            }
            "#,
        );
    }

    #[test]
    fn test_solve_block_has_a_goal_path() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            let b: Bool = true or false
            solve b by {
            }
            "#,
        );
        let goal_paths = env.goal_paths();
        assert_eq!(goal_paths.len(), 1);
    }

    #[test]
    fn test_basic_problem_statement() {
        let mut env = Environment::new_test();
        env.add(
            r#"
            problem {
                let b: Bool = true or false
                solve b by {
                }
            }
            "#,
        );
    }
}
