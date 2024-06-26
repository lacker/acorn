use std::cmp::Ordering;
use std::fmt;

use crate::clause::Clause;
use crate::literal::Literal;
use crate::proposition::{Source, SourceType};
use crate::term::Term;
use crate::term_graph::Justification;

// Use this to toggle experimental algorithm mode
pub const EXPERIMENT: bool = false;

// The "truthiness" categorizes the different types of true statements, relative to a proof.
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum Truthiness {
    // A "factual" truth is true globally, regardless of this particular proof.
    Factual,

    // A "hypothetical" truth is something that we are assuming true in the context of this proof.
    // For example, we might assume that a and b are nonzero, and then prove that a * b != 0.
    Hypothetical,

    // When we want to prove a goal G, we tell the prover that !G is true, and search
    // for contradictions.
    // A "counterfactual" truth is this negated goal, or something derived from it, that we expect
    // to lead to a contradiction.
    Counterfactual,
}

impl Truthiness {
    // When combining truthinesses, the result is the "most untruthy" of the two.
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

// Information about a resolution inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionInfo {
    // Which clauses were used as the sources.
    // Resolution requires one positive and one negative clause.
    pub positive_id: usize,
    pub negative_id: usize,
}

// Information about a rewrite inference.
// Rewrites have two parts, the "pattern" that determines what gets rewritten into what,
// and the "target" which contains the subterm that gets rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteInfo {
    // Which clauses were used as the sources.
    pub pattern_id: usize,
    pub target_id: usize,

    // The truthiness of the source clauses.
    pattern_truthiness: Truthiness,
    target_truthiness: Truthiness,
}

// Information about a substitution inference.
// The original is the clause we started with, and the substitution is the equality clause that
// we used to substitute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstitutionInfo {
    // Which clauses were used as the sources.
    pub original_id: usize,
    pub substitution_id: usize,

    // The truthiness of the source clauses.
    original_truthiness: Truthiness,
    substitution_truthiness: Truthiness,
}

// The rules that can generate new clauses, along with the clause ids used to generate.
#[derive(Debug, PartialEq, Eq)]
pub enum Rule {
    Assumption(Source),

    // Rules based on multiple source clauses
    Resolution(ResolutionInfo),
    Rewrite(RewriteInfo),

    // Rules with only one source clause
    EqualityFactoring(usize),
    EqualityResolution(usize),
    FunctionElimination(usize),

    // A contradiction found by the term graph.
    // We store the ids of the negative literal (always exactly one) and the positive clauses
    // that were used to generate it.
    TermGraph(Justification),
}

impl Rule {
    // The ids of the clauses that this rule directly depends on.
    fn premises(&self) -> Vec<usize> {
        match self {
            Rule::Assumption(_) => vec![],
            Rule::Resolution(info) => vec![info.positive_id, info.negative_id],
            Rule::Rewrite(info) => vec![info.pattern_id, info.target_id],
            Rule::EqualityFactoring(rewritten)
            | Rule::EqualityResolution(rewritten)
            | Rule::FunctionElimination(rewritten) => vec![*rewritten],
            Rule::TermGraph(justification) => {
                let mut premises = justification.rewrite_steps();
                premises.push(justification.inequality_id);
                premises
            }
        }
    }

    // Human-readable.
    pub fn name(&self) -> &str {
        match self {
            Rule::Assumption(_) => "Assumption",
            Rule::Resolution(_) => "Resolution",
            Rule::Rewrite(_) => "Rewrite",
            Rule::EqualityFactoring(_) => "Equality Factoring",
            Rule::EqualityResolution(_) => "Equality Resolution",
            Rule::FunctionElimination(_) => "Function Elimination",
            Rule::TermGraph(..) => "Term Graph",
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
}

// A proof is made up of ProofSteps.
// Each ProofStep contains an output clause, plus a bunch of heuristic information about it, to
// decide if we should "activate" the proof step or not.
#[derive(Debug, Eq, PartialEq)]
pub struct ProofStep {
    // The proof step is primarily defined by a clause that it proves.
    // Semantically, this clause is implied by the input clauses (activated and existing).
    pub clause: Clause,

    // Whether this clause is the normal sort of true, or just something we're hypothesizing for
    // the sake of the proof.
    pub truthiness: Truthiness,

    // How this clause was generated.
    pub rule: Rule,

    // Clauses that we used for additional simplification.
    pub simplification_rules: Vec<usize>,

    // The number of proof steps that this proof step depends on.
    // The size includes this proof step itself, but does not count assumptions and definitions.
    // So the size for any assumption or definition is zero.
    // This does not deduplicate among different branches, so it may be an overestimate.
    // This also ignores rewrites, which may or may not be the ideal behavior.
    proof_size: u32,

    // Whether this proof step is considered "cheap".
    // Cheapness can be amortized. We don't want it to be possible to create an infinite
    // chain of cheap proof steps.
    // The idea is that in the future, we can consider more and more steps to be "cheap".
    // Any step that the AI considers to be "obvious", we can call it "cheap".
    pub cheap: bool,

    // The depth is the number of serial non-cheap steps required to reach this step.
    pub depth: u32,

    // Cached for simplicity
    atom_count: u32,
}

// The better the score, the more we want to activate this proof step.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub enum Score {
    // The first element of a regular score is the negative depth.
    // It's bounded at -MAX_DEPTH so after that we don't use depth for scoring any more.
    //
    // The second element of the score is a deterministic ordering:
    //
    //   Global facts, both explicit and deductions
    //   The negated goal
    //   Explicit hypotheses
    //   Local deductions
    //
    // The third element of the score is heuristic.
    Regular(i32, i32, i32),

    // Contradictions immediately end the proof and thus score highest.
    Contradiction,
}

// Don't bother differentiating depth for score purposes after this point.
const MAX_DEPTH: i32 = 3;

impl Score {
    pub fn is_basic(&self) -> bool {
        match self {
            Score::Regular(negadepth, _, _) => *negadepth > -MAX_DEPTH,
            Score::Contradiction => true,
        }
    }
}

impl Ord for ProofStep {
    // The heuristic used to decide which clause is the most promising.
    // The passive set is a "max heap", so we want the best clause to compare as the largest.
    fn cmp(&self, other: &ProofStep) -> Ordering {
        self.score().cmp(&other.score())
    }
}

impl PartialOrd for ProofStep {
    fn partial_cmp(&self, other: &ProofStep) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for ProofStep {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} ; rule = {:?}", self.clause, self.rule)
    }
}

impl ProofStep {
    fn new(
        clause: Clause,
        truthiness: Truthiness,
        rule: Rule,
        simplification_rules: Vec<usize>,
        proof_size: u32,
        cheap: bool,
        depth: u32,
    ) -> ProofStep {
        let atom_count = clause.atom_count();
        ProofStep {
            clause,
            truthiness,
            rule,
            simplification_rules,
            proof_size,
            cheap,
            depth,
            atom_count,
        }
    }

    // Construct a new assumption ProofStep that is not dependent on any other steps.
    pub fn new_assumption(clause: Clause, truthiness: Truthiness, source: &Source) -> ProofStep {
        let rule = Rule::Assumption(source.clone());
        ProofStep::new(clause, truthiness, rule, vec![], 0, true, 0)
    }

    // Construct a new ProofStep that is a direct implication of a single activated step,
    // not requiring any other clauses.
    pub fn new_direct(activated_step: &ProofStep, rule: Rule, clause: Clause) -> ProofStep {
        ProofStep::new(
            clause,
            activated_step.truthiness,
            rule,
            vec![],
            activated_step.proof_size + 1,
            true,
            activated_step.depth,
        )
    }

    // Construct a new ProofStep via resolution.
    pub fn new_resolution(
        positive_id: usize,
        positive_step: &ProofStep,
        negative_id: usize,
        negative_step: &ProofStep,
        clause: Clause,
    ) -> ProofStep {
        let rule = Rule::Resolution(ResolutionInfo {
            positive_id,
            negative_id,
        });

        let cheap =
            positive_step.clause.contains(&clause) || negative_step.clause.contains(&clause);

        let depth =
            std::cmp::max(positive_step.depth, negative_step.depth) + if cheap { 0 } else { 1 };

        ProofStep::new(
            clause,
            positive_step.truthiness.combine(negative_step.truthiness),
            rule,
            vec![],
            positive_step.proof_size + negative_step.proof_size + 1,
            cheap,
            depth,
        )
    }

    // Construct a new ProofStep via rewriting.
    // We are replacing a subterm of the target literal with a new subterm.
    pub fn new_rewrite(
        pattern_id: usize,
        pattern_step: &ProofStep,
        target_id: usize,
        target_step: &ProofStep,
        target_left: bool,
        path: &[usize],
        new_subterm: &Term,
    ) -> ProofStep {
        assert_eq!(target_step.clause.literals.len(), 1);
        let target_literal = &target_step.clause.literals[0];
        let (u, v) = if target_left {
            (&target_literal.left, &target_literal.right)
        } else {
            (&target_literal.right, &target_literal.left)
        };
        let new_u = u.replace_at_path(path, new_subterm.clone());
        let new_literal = Literal::new(target_literal.positive, new_u, v.clone());
        let clause = Clause::new(vec![new_literal]);

        let rule = Rule::Rewrite(RewriteInfo {
            pattern_id,
            target_id,
            pattern_truthiness: pattern_step.truthiness,
            target_truthiness: target_step.truthiness,
        });

        // We only compare against the target
        let cheap = if clause.is_impossible() {
            true
        } else {
            assert_eq!(clause.literals.len(), 1);
            assert_eq!(target_step.clause.literals.len(), 1);
            clause.literals[0].extended_kbo_cmp(&target_step.clause.literals[0]) == Ordering::Less
        };
        let depth =
            std::cmp::max(pattern_step.depth, target_step.depth) + if cheap { 0 } else { 1 };

        ProofStep::new(
            clause,
            pattern_step.truthiness.combine(target_step.truthiness),
            rule,
            vec![],
            pattern_step.proof_size + target_step.proof_size + 1,
            cheap,
            depth,
        )
    }

    // A proof step for when the term graph tells us it found a contradiction.
    // The proof size and depth seem kind of wrong.
    pub fn new_term_graph_contradiction(
        last_step: &ProofStep,
        justification: Justification,
    ) -> ProofStep {
        let rule = Rule::TermGraph(justification);
        ProofStep::new(
            Clause::impossible(),
            Truthiness::Counterfactual,
            rule,
            vec![],
            last_step.proof_size + 1,
            true,
            last_step.depth,
        )
    }

    // Create a replacement for this clause that has extra simplification rules.
    // It's hard to handle depth well, here.
    pub fn simplify(
        self,
        new_clause: Clause,
        new_rules: Vec<usize>,
        new_truthiness: Truthiness,
    ) -> ProofStep {
        let rules = self
            .simplification_rules
            .iter()
            .chain(new_rules.iter())
            .cloned()
            .collect();
        ProofStep::new(
            new_clause,
            new_truthiness,
            self.rule,
            rules,
            self.proof_size,
            self.cheap,
            self.depth,
        )
    }

    // Construct a ProofStep with fake heuristic data for testing
    pub fn mock(s: &str) -> ProofStep {
        let clause = Clause::parse(s);
        ProofStep::new(
            clause,
            Truthiness::Factual,
            Rule::Assumption(Source::mock()),
            vec![],
            0,
            true,
            0,
        )
    }

    // The ids of the other clauses that this clause depends on.
    pub fn dependencies(&self) -> Vec<usize> {
        let mut answer = self.rule.premises();
        for rule in &self.simplification_rules {
            answer.push(*rule);
        }
        answer
    }

    pub fn depends_on(&self, id: usize) -> bool {
        self.dependencies().iter().any(|i| *i == id)
    }

    // Whether this is the last step of the proof
    pub fn finishes_proof(&self) -> bool {
        self.clause.is_impossible()
    }

    // Whether this step is created by the normalization of the negated goal
    pub fn is_negated_goal(&self) -> bool {
        if let Rule::Assumption(source) = &self.rule {
            matches!(source.source_type, SourceType::NegatedGoal)
        } else {
            false
        }
    }

    // A lot of heuristics here that perhaps could be simpler.
    pub fn score(&self) -> Score {
        if self.clause.is_impossible() {
            return Score::Contradiction;
        }

        // Higher = more important, for the deterministic tier.
        let deterministic_tier = match self.truthiness {
            Truthiness::Counterfactual => {
                if self.is_negated_goal() {
                    3
                } else {
                    1
                }
            }
            Truthiness::Hypothetical => {
                if let Rule::Assumption(_) = self.rule {
                    2
                } else {
                    1
                }
            }
            Truthiness::Factual => 4,
        };

        let mut heuristic = 0;
        heuristic -= self.atom_count as i32;
        heuristic -= 2 * self.proof_size as i32;
        if self.truthiness == Truthiness::Hypothetical {
            heuristic -= 3;
        }

        let negadepth = -(self.depth as i32).max(-MAX_DEPTH);
        return Score::Regular(negadepth, deterministic_tier, heuristic);
    }

    // We have to strictly limit deduction that happens between two library facts, because
    // the plan is for the library to grow very large.
    pub fn automatic_reject(&self) -> bool {
        if self.truthiness == Truthiness::Factual && self.proof_size > 2 {
            // Only do one step of deduction with global facts.
            return true;
        }

        false
    }
}
