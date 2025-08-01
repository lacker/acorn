use std::cmp::Ordering;
use std::fmt;

use crate::atom::Atom;
use crate::clause::{Clause, LiteralTrace};
use crate::literal::Literal;
use crate::proposition::MonomorphicProposition;
use crate::source::{Source, SourceType};
use crate::term::Term;

/// The different sorts of proof steps.
#[derive(Debug, Eq, PartialEq, Hash, Clone, Copy)]
pub enum ProofStepId {
    /// A proof step that was activated and exists in the active set.
    Active(usize),

    /// A proof step that was never activated, but was used to find a contradiction.
    Passive(u32),

    /// The final step of a proof.
    /// No active id because it never gets inserted into the active set.
    Final,
}

impl ProofStepId {
    pub fn active_id(&self) -> Option<usize> {
        match self {
            ProofStepId::Active(id) => Some(*id),
            _ => None,
        }
    }
}

/// The "truthiness" categorizes the different types of true statements, relative to a proof.
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum Truthiness {
    /// A "factual" truth is true globally, regardless of this particular proof.
    Factual,

    /// A "hypothetical" truth is something that we are assuming true in the context of this proof.
    /// For example, we might assume that a and b are nonzero, and then prove that a * b != 0.
    Hypothetical,

    /// When we want to prove a goal G, we tell the prover that !G is true, and search
    /// for contradictions.
    /// A "counterfactual" truth is this negated goal, or something derived from it, that we expect
    /// to lead to a contradiction.
    Counterfactual,
}

impl Truthiness {
    /// When combining truthinesses, the result is the "most untruthy" of the two.
    pub fn combine(&self, other: Truthiness) -> Truthiness {
        match (self, other) {
            (Truthiness::Counterfactual, _) => Truthiness::Counterfactual,
            (_, Truthiness::Counterfactual) => Truthiness::Counterfactual,
            (Truthiness::Hypothetical, _) => Truthiness::Hypothetical,
            (_, Truthiness::Hypothetical) => Truthiness::Hypothetical,
            (Truthiness::Factual, Truthiness::Factual) => Truthiness::Factual,
        }
    }
}

/// Information about a resolution inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionInfo {
    /// Which clauses were used as the sources.
    /// The short clause must have only one literal.
    pub short_id: usize,

    /// The long clause will usually have more than one literal. It can have just one literal
    /// if we're finding a contradiction.
    pub long_id: usize,
}

/// Information about a specialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecializationInfo {
    /// The specialization is taking the pattern and substituting in particular values.
    pub pattern_id: usize,

    /// The inspiration isn't mathematically necessary for the specialization to be true,
    /// but we used it to decide which substitutions to make.
    pub inspiration_id: usize,
}

/// Information about a rewrite inference.
/// Rewrites have two parts, the "pattern" that determines what gets rewritten into what,
/// and the "target" which contains the subterm that gets rewritten.
/// Both of these parts are single-literal clauses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteInfo {
    /// Which clauses were used as the sources.
    pub pattern_id: usize,
    pub target_id: usize,

    /// Whether we rewrite the term on the left of the target literal. (As opposed to the right.)
    pub target_left: bool,

    /// The path within the target term that we rewrite.
    pub path: Vec<usize>,

    /// Whether this is a forwards or backwards rewrite.
    /// A forwards rewrite rewrites the left side of the pattern into the right.
    pub forwards: bool,

    /// The single-clause literal initially created by the rewrite.
    /// This is usually redundant, but not always, because the output clause can get simplified.
    pub rewritten: Clause,

    /// Whether the literal was flipped during normalization
    pub flipped: bool,
}

/// Information about a contradiction found by rewriting one side of an inequality into the other.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct MultipleRewriteInfo {
    /// The id of the inequality clause.
    pub inequality_id: usize,
    /// The ids of active clauses used in the rewrite chain.
    pub active_ids: Vec<usize>,
    /// The ids of passive clauses used in the rewrite chain.
    pub passive_ids: Vec<u32>,
}

/// Information about an assumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssumptionInfo {
    /// The source of the assumption.
    pub source: Source,

    /// If this assumption is the definition of a particular atom, this is the atom.
    pub defined_atom: Option<Atom>,
    
    /// The literals of the assumption before any simplification.
    pub literals: Vec<Literal>,
}

/// Information about what happens to a term during equality factoring.
#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub struct EFTermTrace {
    /// Which literal it goes to
    pub index: usize,

    /// Whether it goes to the left of that literal
    pub left: bool,
}

/// Information about what happens to a literal during equality factoring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EFLiteralTrace {
    pub left: EFTermTrace,
    pub right: EFTermTrace,
}

impl EFLiteralTrace {
    pub fn to_index(index: usize, flipped: bool) -> EFLiteralTrace {
        EFLiteralTrace::to_out(
            EFTermTrace { index, left: true },
            EFTermTrace { index, left: false },
            flipped,
        )
    }

    /// Trace a literal that goes to a provided output. Flip the input if flipped is provided.
    pub fn to_out(left: EFTermTrace, right: EFTermTrace, flipped: bool) -> EFLiteralTrace {
        if flipped {
            EFLiteralTrace::new(right, left)
        } else {
            EFLiteralTrace::new(left, right)
        }
    }

    pub fn new(left: EFTermTrace, right: EFTermTrace) -> EFLiteralTrace {
        EFLiteralTrace { left, right }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqualityFactoringInfo {
    /// The id of the clause that was factored.
    pub id: usize,

    /// The literals that we got immediately after factoring.
    pub literals: Vec<Literal>,

    /// Parallel to literals. Tracks how we got them from the input clause.
    pub ef_trace: Vec<EFLiteralTrace>,
}

/// Information about an equality resolution inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EqualityResolutionInfo {
    /// The id of the clause that was resolved.
    pub id: usize,

    // Which literal in the input clause got resolved away.
    pub index: usize,

    // The literals that we got immediately after resolution.
    pub literals: Vec<Literal>,

    // Parallel to literals. Tracks whether they were flipped or not.
    pub flipped: Vec<bool>,
}

/// Information about a function elimination inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionEliminationInfo {
    /// The id of the clause that had a function eliminated.
    pub id: usize,

    /// The literal that was eliminated.
    pub index: usize,

    /// The literals that we got immediately after function elimination.
    pub literals: Vec<Literal>,

    /// Whether the function-eliminated literal was flipped.
    pub flipped: bool,

    /// The argument to the eliminated function that we kept.
    pub arg: usize,
}

/// The rules that can generate new clauses, along with the clause ids used to generate.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Rule {
    Assumption(AssumptionInfo),

    /// Rules based on multiple source clauses
    Resolution(ResolutionInfo),
    Rewrite(RewriteInfo),
    Specialization(SpecializationInfo),

    /// Rules with only one source clause
    EqualityFactoring(EqualityFactoringInfo),
    EqualityResolution(EqualityResolutionInfo),
    FunctionElimination(FunctionEliminationInfo),

    /// A contradiction found by repeatedly rewriting identical terms.
    MultipleRewrite(MultipleRewriteInfo),

    /// A contradiction between a number of passive clauses.
    PassiveContradiction(u32),
}

impl Rule {
    /// The ids of the clauses that this rule mathematically depends on.
    pub fn premises(&self) -> Vec<ProofStepId> {
        match self {
            Rule::Assumption(_) => vec![],
            Rule::Resolution(info) => vec![
                ProofStepId::Active(info.short_id),
                ProofStepId::Active(info.long_id),
            ],
            Rule::Rewrite(info) => vec![
                ProofStepId::Active(info.pattern_id),
                ProofStepId::Active(info.target_id),
            ],
            Rule::EqualityFactoring(info) => vec![ProofStepId::Active(info.id)],
            Rule::EqualityResolution(info) => vec![ProofStepId::Active(info.id)],
            Rule::FunctionElimination(info) => vec![ProofStepId::Active(info.id)],
            Rule::Specialization(info) => vec![ProofStepId::Active(info.pattern_id)],
            Rule::MultipleRewrite(multi_rewrite_info) => {
                let mut answer = vec![ProofStepId::Active(multi_rewrite_info.inequality_id)];
                for id in &multi_rewrite_info.active_ids {
                    answer.push(ProofStepId::Active(*id));
                }
                for id in &multi_rewrite_info.passive_ids {
                    answer.push(ProofStepId::Passive(*id));
                }
                answer
            }
            Rule::PassiveContradiction(n) => (0..*n).map(|id| ProofStepId::Passive(id)).collect(),
        }
    }

    /// Returns a human-readable name for this rule.
    pub fn name(&self) -> &str {
        match self {
            Rule::Assumption(_) => "Assumption",
            Rule::Resolution(_) => "Resolution",
            Rule::Rewrite(_) => "Rewrite",
            Rule::EqualityFactoring(_) => "Equality Factoring",
            Rule::EqualityResolution(_) => "Equality Resolution",
            Rule::FunctionElimination(_) => "Function Elimination",
            Rule::Specialization(_) => "Specialization",
            Rule::MultipleRewrite(..) => "Multiple Rewrite",
            Rule::PassiveContradiction(..) => "Passive Contradiction",
        }
    }

    pub fn is_rewrite(&self) -> bool {
        match self {
            Rule::Rewrite(_) => true,
            _ => false,
        }
    }

    pub fn is_assumption(&self) -> bool {
        match self {
            Rule::Assumption(_) => true,
            _ => false,
        }
    }

    pub fn is_negated_goal(&self) -> bool {
        match self {
            Rule::Assumption(info) => matches!(info.source.source_type, SourceType::NegatedGoal),
            _ => false,
        }
    }
}

/// A proof is made up of ProofSteps.
/// Each ProofStep contains an output clause, plus a bunch of information we track about it.
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct ProofStep {
    /// The proof step is primarily defined by a clause that it proves.
    /// Semantically, this clause is implied by the input clauses (activated and existing).
    pub clause: Clause,

    /// Whether this clause is the normal sort of true, or just something we're hypothesizing for
    /// the sake of the proof.
    pub truthiness: Truthiness,

    /// How this clause was generated.
    pub rule: Rule,

    /// Clauses that we used for additional simplification.
    pub simplification_rules: Vec<usize>,

    /// The number of proof steps that this proof step depends on.
    /// The size includes this proof step itself, but does not count assumptions and definitions.
    /// So the size for any assumption or definition is zero.
    /// This does not deduplicate among different branches, so it may be an overestimate.
    pub proof_size: u32,

    /// Not every proof step counts toward depth.
    /// When we use a new long clause to resolve against, that counts toward depth, because
    /// it roughly corresponds to "using a theorem".
    /// When we use a rewrite backwards, increasing KBO, that also counts toward depth.
    pub depth: u32,

    /// A printable proof step is one that we are willing to turn into a line of code in a proof.
    /// Unprintable proof steps are things like halfway resolved theorems, or expressions
    /// that use anonymous skolem variables.
    pub printable: bool,

    /// Information about this step that will let us reconstruct the variable mappings.
    pub traces: Option<Vec<LiteralTrace>>,
}

impl fmt::Display for ProofStep {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} ; rule = {:?}", self.clause, self.rule)
    }
}

impl ProofStep {
    /// Construct a new assumption ProofStep that is not dependent on any other steps.
    /// Assumptions are always depth zero, but eventually we may have to revisit that.
    pub fn assumption(
        proposition: &MonomorphicProposition,
        clause: Clause,
        defined_atom: Option<Atom>,
    ) -> ProofStep {
        let source = proposition.source.clone();
        let literals = clause.literals.clone();
        let rule = Rule::Assumption(AssumptionInfo {
            source,
            defined_atom,
            literals,
        });
        
        // Create traces to indicate that no literals have been moved around
        let traces = Some(
            clause
                .literals
                .iter()
                .enumerate()
                .map(|(i, _)| LiteralTrace::Output {
                    index: i,
                    flipped: false,
                })
                .collect(),
        );
        
        ProofStep {
            clause,
            truthiness: proposition.source.truthiness(),
            rule,
            simplification_rules: vec![],
            proof_size: 0,
            depth: 0,
            printable: false,
            traces,
        }
    }

    /// Construct a new ProofStep that is a direct implication of a single activated step,
    /// not requiring any other clauses.
    pub fn direct(
        _activated_id: usize,
        activated_step: &ProofStep,
        rule: Rule,
        clause: Clause,
        literal_traces: Vec<crate::clause::LiteralTrace>,
    ) -> ProofStep {
        // Direct implication does not add to depth.
        let depth = activated_step.depth;
        let printable = clause.is_printable();
        
        ProofStep {
            clause,
            truthiness: activated_step.truthiness,
            rule,
            simplification_rules: vec![],
            proof_size: activated_step.proof_size + 1,
            depth,
            printable,
            traces: Some(literal_traces),
        }
    }

    /// Construct a ProofStep that is a specialization of a general pattern.
    pub fn specialization(
        pattern_id: usize,
        inspiration_id: usize,
        pattern_step: &ProofStep,
        clause: Clause,
        traces: Vec<LiteralTrace>,
    ) -> ProofStep {
        let info = SpecializationInfo {
            pattern_id,
            inspiration_id,
        };
        ProofStep {
            clause,
            truthiness: pattern_step.truthiness,
            rule: Rule::Specialization(info),
            simplification_rules: vec![],
            proof_size: pattern_step.proof_size + 1,
            depth: pattern_step.depth,
            printable: true,
            traces: Some(traces),
        }
    }

    /// Construct a new ProofStep via resolution.
    pub fn resolution(
        long_id: usize,
        long_step: &ProofStep,
        short_id: usize,
        short_step: &ProofStep,
        clause: Clause,
    ) -> ProofStep {
        let rule = Rule::Resolution(ResolutionInfo { short_id, long_id });

        let truthiness = short_step.truthiness.combine(long_step.truthiness);
        let proof_size = short_step.proof_size + long_step.proof_size + 1;

        let depth = if long_step.depth <= short_step.depth {
            if long_step.clause.contains(&clause) {
                // This is just a simplification
                short_step.depth
            } else {
                // This resolution is a new "theorem" that we are using.
                // So we need to add one to depth.
                short_step.depth + 1
            }
        } else {
            // This resolution is essentially continuing to resolve a theorem
            // statement that we have already fetched.
            long_step.depth
        };

        let printable = clause.is_printable();

        ProofStep {
            clause,
            truthiness,
            rule,
            simplification_rules: vec![],
            proof_size,
            depth,
            printable,
            traces: None,
        }
    }

    /// Create a replacement for this clause that has extra simplification rules.
    /// The long step doesn't have an id because it isn't activated.
    pub fn simplified(
        long_step: ProofStep,
        short_steps: &[(usize, &ProofStep)],
        clause: Clause,
        traces: Option<Vec<LiteralTrace>>,
    ) -> ProofStep {
        let mut truthiness = long_step.truthiness;
        let mut simplification_rules = long_step.simplification_rules;
        let mut proof_size = long_step.proof_size;
        let mut depth = long_step.depth;
        for (short_id, short_step) in short_steps {
            truthiness = truthiness.combine(short_step.truthiness);
            simplification_rules.push(*short_id);
            proof_size += short_step.proof_size;
            depth = u32::max(depth, short_step.depth);
        }

        let printable = clause.is_printable();

        ProofStep {
            clause,
            truthiness,
            rule: long_step.rule,
            simplification_rules,
            proof_size,
            depth,
            printable,
            traces,
        }
    }

    /// Construct a new ProofStep via rewriting.
    /// We are replacing a subterm of the target literal with a new subterm.
    /// Note that the target step will always be a concrete single literal.
    /// The pattern and the output may have variables in them.
    /// It seems weird for the output to have variables, but it does.
    ///
    /// A "forwards" rewrite goes left-to-right in the pattern.
    ///
    /// The trace will capture everything that happens *after* the rewrite.
    pub fn rewrite(
        pattern_id: usize,
        pattern_step: &ProofStep,
        target_id: usize,
        target_step: &ProofStep,
        target_left: bool,
        path: &[usize],
        forwards: bool,
        new_subterm: &Term,
    ) -> ProofStep {
        assert_eq!(target_step.clause.literals.len(), 1);

        let target_literal = &target_step.clause.literals[0];
        let (new_literal, flipped) =
            target_literal.replace_at_path(target_left, path, new_subterm.clone());

        let simplifying = new_literal.extended_kbo_cmp(&target_literal) == Ordering::Less;
        let (clause, traces) = Clause::from_literal(new_literal, false);

        let truthiness = pattern_step.truthiness.combine(target_step.truthiness);

        let rule = Rule::Rewrite(RewriteInfo {
            pattern_id,
            target_id,
            target_left,
            path: path.to_vec(),
            forwards,
            rewritten: clause.clone(),
            flipped,
        });

        let proof_size = pattern_step.proof_size + target_step.proof_size + 1;

        let dependency_depth = std::cmp::max(pattern_step.depth, target_step.depth);
        let depth = if simplifying {
            dependency_depth
        } else {
            dependency_depth + 1
        };

        let printable = clause.is_printable();

        ProofStep {
            clause,
            truthiness,
            rule,
            simplification_rules: vec![],
            proof_size,
            depth,
            printable,
            traces: Some(traces),
        }
    }

    /// A proof step for finding a contradiction via a series of rewrites.
    pub fn multiple_rewrite(
        inequality_id: usize,
        active_ids: Vec<usize>,
        passive_ids: Vec<u32>,
        truthiness: Truthiness,
        depth: u32,
    ) -> ProofStep {
        let rule = Rule::MultipleRewrite(MultipleRewriteInfo {
            inequality_id,
            active_ids,
            passive_ids,
        });

        // proof size is wrong but we don't use it for a contradiction.
        ProofStep {
            clause: Clause::impossible(),
            truthiness,
            rule,
            simplification_rules: vec![],
            proof_size: 0,
            depth,
            printable: true,
            traces: None,
        }
    }

    /// Assumes the provided steps are indexed by passive id, and that we use all of them.
    pub fn passive_contradiction(passive_steps: &[ProofStep]) -> ProofStep {
        let rule = Rule::PassiveContradiction(passive_steps.len() as u32);
        let mut truthiness = Truthiness::Factual;
        let mut depth = 0;
        let mut proof_size = 0;
        for step in passive_steps {
            truthiness = truthiness.combine(step.truthiness);
            depth = std::cmp::max(depth, step.depth);
            proof_size += step.proof_size;
        }

        ProofStep {
            clause: Clause::impossible(),
            truthiness,
            rule,
            simplification_rules: vec![],
            proof_size,
            depth,
            printable: true,
            traces: None,
        }
    }

    /// Construct a ProofStep with fake heuristic data for testing
    pub fn mock(s: &str) -> ProofStep {
        let clause = Clause::parse(s);
        Self::mock_from_clause(clause)
    }

    pub fn mock_from_clause(clause: Clause) -> ProofStep {
        let truthiness = Truthiness::Factual;
        let literals = clause.literals.clone();
        let rule = Rule::Assumption(AssumptionInfo {
            source: Source::mock(),
            defined_atom: None,
            literals,
        });
        ProofStep {
            clause,
            truthiness,
            rule,
            simplification_rules: vec![],
            proof_size: 0,
            depth: 0,
            printable: true,
            traces: None,
        }
    }

    /// The ids of the other clauses that this clause depends on.
    pub fn dependencies(&self) -> Vec<ProofStepId> {
        let mut answer = self.rule.premises();
        for rule in &self.simplification_rules {
            answer.push(ProofStepId::Active(*rule));
        }
        answer
    }

    /// include_inspiration is whether we should include the inspiration clause in the dependencies.
    pub fn active_dependencies(&self, include_inspiration: bool) -> Vec<usize> {
        let mut answer: Vec<_> = self
            .dependencies()
            .iter()
            .filter_map(|id| match id {
                ProofStepId::Active(id) => Some(*id),
                _ => None,
            })
            .collect();
        if include_inspiration {
            if let Rule::Specialization(info) = &self.rule {
                answer.push(info.inspiration_id);
            }
        }
        answer
    }

    pub fn depends_on_active(&self, id: usize) -> bool {
        self.dependencies()
            .iter()
            .any(|i| *i == ProofStepId::Active(id))
    }

    /// Whether this is the last step of the proof
    pub fn finishes_proof(&self) -> bool {
        self.clause.is_impossible()
    }

    pub fn automatic_reject(&self) -> bool {
        // We have to strictly limit deduction that happens between two library facts, because
        // the plan is for the library to grow very large.
        if self.truthiness == Truthiness::Factual && self.proof_size > 2 {
            // Only do one step of deduction with global facts.
            return true;
        }

        // In some degenerate cases going very deep can crash the prover.
        if self.depth >= 30 {
            return true;
        }

        false
    }
}
