use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tower_lsp::lsp_types::Url;

use crate::acorn_value::AcornValue;
use crate::active_set::ActiveSet;
use crate::binding_map::BindingMap;
use crate::clause::Clause;
use crate::display::DisplayClause;
use crate::fact::Fact;
use crate::goal::{Goal, GoalContext};
use crate::interfaces::{ClauseInfo, InfoResult, Location, ProofStepInfo};
use crate::literal::Literal;
use crate::module::ModuleId;
use crate::normalizer::Normalizer;
use crate::passive_set::PassiveSet;
use crate::project::Project;
use crate::proof::{Difficulty, Proof};
use crate::proof_step::{ProofStep, ProofStepId, Rule, Truthiness};
use crate::term::Term;
use crate::term_graph::TermGraphContradiction;

#[derive(Clone)]
pub struct Prover {
    // The normalizer is used when we are turning the facts and goals from the environment into
    // clauses that we can use internally.
    normalizer: Normalizer,

    // The "active" clauses are the ones we use for reasoning.
    active_set: ActiveSet,

    // The "passive" clauses are a queue of pending clauses that
    // we will add to the active clauses in the future.
    passive_set: PassiveSet,

    // A verbose prover prints out a lot of stuff.
    pub verbose: bool,

    // The last step of the proof search that leads to a contradiction.
    // If we haven't finished the search, this is None.
    final_step: Option<ProofStep>,

    // Clauses that we never activated, but we did use to find a contradiction.
    useful_passive: Vec<ProofStep>,

    // Setting any of these flags to true externally will stop the prover.
    pub stop_flags: Vec<Arc<AtomicBool>>,

    // This error gets set when there is a problem during the construction of the prover.
    // It would be nicer to report the error immediately, but we wait so that we have
    // a reasonable location to attach the error to, when running in the LSP.
    error: Option<String>,

    // Number of proof steps activated, not counting Factual ones.
    nonfactual_activations: i32,

    // The goal of the prover.
    // If this is None, the goal hasn't been set yet.
    goal: Option<NormalizedGoal>,
}

#[derive(Clone)]
enum NormalizedGoal {
    // The value expresses the negation of the goal we are trying to prove.
    // It is normalized in the sense that we would use this form to generate code.
    // The flag indicates whether inconsistencies are okay.
    // Ie, if we find a contradiction, is that Outcome::Success or Outcome::Inconsistent?
    ProveNegated(AcornValue, bool),

    // The normalized term we are solving for, if there is one.
    Solve(Term),
}

// The outcome of a prover operation.
// "Success" means we proved it.
// "Exhausted" means we tried every possibility and couldn't prove it.
// "Inconsistent" means that we found a contradiction just in our initial assumptions.
// "Interrupted" means that the prover was explicitly stopped.
// "Timeout" means that we hit a nondeterministic timing limit.
// "Constrained" means that we hit some deterministic limit.
// "Error" means that we found a problem in the code that needs to be fixed by the user.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Outcome {
    Success,
    Exhausted,
    Inconsistent,
    Interrupted,
    Timeout,
    Constrained,
    Error(String),
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Outcome::Success => write!(f, "Success"),
            Outcome::Exhausted => write!(f, "Exhausted"),
            Outcome::Inconsistent => write!(f, "Inconsistent"),
            Outcome::Interrupted => write!(f, "Interrupted"),
            Outcome::Timeout => write!(f, "Timeout"),
            Outcome::Constrained => write!(f, "Constrained"),
            Outcome::Error(s) => write!(f, "Error: {}", s),
        }
    }
}

impl Prover {
    pub fn new(project: &Project, verbose: bool) -> Prover {
        Prover {
            normalizer: Normalizer::new(),
            active_set: ActiveSet::new(),
            passive_set: PassiveSet::new(),
            verbose,
            final_step: None,
            stop_flags: vec![project.build_stopped.clone()],
            error: None,
            useful_passive: vec![],
            nonfactual_activations: 0,
            goal: None,
        }
    }

    // Add a fact to the prover.
    // The fact can be either polymorphic or monomorphic.
    pub fn add_fact(&mut self, fact: Fact) {
        let mut steps = vec![];
        match self.normalizer.normalize_fact(fact, &mut steps) {
            Ok(()) => {}
            Err(s) => {
                self.error = Some(s);
                return;
            }
        };
        self.passive_set.push_batch(steps);
    }

    pub fn set_goal(&mut self, goal_context: &GoalContext) {
        assert!(self.goal.is_none());

        match &goal_context.goal {
            Goal::Prove(prop) => {
                // Negate the goal and add it as a counterfactual assumption.
                let (hypo, counter) = prop.value.clone().negate_goal();
                if let Some(hypo) = hypo {
                    self.add_fact(Fact::Proposition(
                        prop.with_value(hypo),
                        Truthiness::Hypothetical,
                    ));
                }
                self.add_fact(Fact::Proposition(
                    prop.with_negated_goal(counter.clone()),
                    Truthiness::Counterfactual,
                ));
                self.goal = Some(NormalizedGoal::ProveNegated(
                    counter,
                    goal_context.inconsistency_okay,
                ));
            }
            Goal::Solve(value, _) => match self.normalizer.term_from_value(value, true) {
                Ok(term) => {
                    self.goal = Some(NormalizedGoal::Solve(term));
                }
                Err(s) => {
                    self.error = Some(s);
                }
            },
        }
    }

    pub fn iter_active_steps(&self) -> impl Iterator<Item = (usize, &ProofStep)> {
        self.active_set.iter_steps()
    }

    pub fn print_stats(&self) {
        // Kinda only printing this so that Solve(term) isn't unused
        match &self.goal {
            Some(NormalizedGoal::ProveNegated(v, _)) => {
                println!("goal: disprove {}", v);
            }
            Some(NormalizedGoal::Solve(t)) => {
                println!("goal: solve for {}", t);
            }
            None => {
                println!("no goal set");
            }
        }
        println!("{} clauses in the active set", self.active_set.len());
        println!("{} clauses in the passive set", self.passive_set.len());
    }

    // Prints out the entire active set
    pub fn print_active(&self, substr: Option<&str>) {
        let mut count = 0;
        for clause in self.active_set.iter_clauses() {
            let clause = self.display(clause);
            if let Some(substr) = substr {
                if !clause.to_string().contains(substr) {
                    continue;
                }
            }
            count += 1;
            println!("{}", clause);
        }
        if let Some(substr) = substr {
            println!("{} active clauses matched {}", count, substr);
        } else {
            println!("{} clauses total in the active set", count);
        }
    }

    pub fn print_passive(&self, substr: Option<&str>) {
        let mut count = 0;
        let steps: Vec<_> = self.passive_set.iter_steps().collect();
        // Only print the first ones
        for step in steps.iter().take(500) {
            let clause = self.display(&step.clause);
            if let Some(substr) = substr {
                if !clause.to_string().contains(substr) {
                    continue;
                }
            }
            count += 1;
            println!("{}", clause);
            println!("  {}", step);
        }
        if let Some(substr) = substr {
            println!("{} passive clauses matched {}", count, substr);
        } else {
            if steps.len() > count {
                println!("  ...omitting {} more", steps.len() - count);
            }
            println!("{} clauses total in the passive set", steps.len());
        }
    }

    // Prints out information for a specific term
    pub fn print_term_info(&self, s: &str) {
        let mut count = 0;
        for clause in self.active_set.iter_clauses() {
            let clause_str = self.display(clause).to_string();
            if clause_str.contains(s) {
                println!("{}", clause_str);
                count += 1;
            }
        }
        println!(
            "{} clause{} matched",
            count,
            if count == 1 { "" } else { "s" }
        );
    }

    // (description, id) for every clause this rule depends on.
    // Entries with an id are references to clauses we are using.
    // An entry with no id is like a comment, it won't be linked to anything.
    fn descriptive_dependencies(&self, step: &ProofStep) -> Vec<(String, ProofStepId)> {
        let mut answer = vec![];
        match &step.rule {
            Rule::Assumption(_) => {}
            Rule::Resolution(info) => {
                answer.push((
                    "long resolver".to_string(),
                    ProofStepId::Active(info.long_id),
                ));
                answer.push((
                    "short resolver".to_string(),
                    ProofStepId::Active(info.short_id),
                ));
            }
            Rule::Rewrite(info) => {
                answer.push(("target".to_string(), ProofStepId::Active(info.target_id)));
                answer.push(("pattern".to_string(), ProofStepId::Active(info.pattern_id)));
            }
            Rule::EqualityFactoring(source)
            | Rule::EqualityResolution(source)
            | Rule::FunctionElimination(source) => {
                answer.push(("source".to_string(), ProofStepId::Active(*source)));
            }
            Rule::Specialization(info) => {
                answer.push(("pattern".to_string(), ProofStepId::Active(info.pattern_id)));
            }
            Rule::MultipleRewrite(info) => {
                answer.push((
                    "inequality".to_string(),
                    ProofStepId::Active(info.inequality_id),
                ));
                for &id in &info.active_ids {
                    answer.push(("equality".to_string(), ProofStepId::Active(id)));
                }
                for &id in &info.passive_ids {
                    answer.push(("specialization".to_string(), ProofStepId::Passive(id)));
                }
            }
            Rule::PassiveContradiction(n) => {
                for id in 0..*n {
                    answer.push(("clause".to_string(), ProofStepId::Passive(id)));
                }
            }
        }

        for rule in &step.simplification_rules {
            answer.push(("simplification".to_string(), ProofStepId::Active(*rule)));
        }
        answer
    }

    fn print_proof_step(&self, preface: &str, step: &ProofStep) {
        println!(
            "\n{}{} generated (depth {}):\n    {}",
            preface,
            step.rule.name(),
            step.depth,
            self.display(&step.clause)
        );

        for (description, id) in self.descriptive_dependencies(&step) {
            match id {
                ProofStepId::Active(i) => {
                    let c = self.display(self.active_set.get_clause(i));
                    println!("  using {} {}:\n    {}", description, i, c);
                }
                ProofStepId::Passive(i) => {
                    let c = self.display(&self.useful_passive[i as usize].clause);
                    println!("  using {}:\n    {}", description, c);
                }
                ProofStepId::Final => {
                    println!("  <unexpected dependency on final proof step>");
                }
            }
        }
    }

    pub fn num_activated(&self) -> usize {
        self.active_set.len()
    }

    pub fn num_passive(&self) -> usize {
        self.passive_set.len()
    }

    pub fn get_and_print_proof(&self) -> Option<Proof> {
        let proof = match self.get_condensed_proof() {
            Some(proof) => proof,
            None => {
                println!("we do not have a proof");
                return None;
            }
        };

        println!(
            "in total, we activated {} proof steps.",
            self.active_set.len()
        );
        println!("non-factual activations: {}", self.nonfactual_activations);

        println!("the proof uses {} steps:", proof.all_steps.len());
        for (id, step) in &proof.all_steps {
            let preface = match id {
                ProofStepId::Active(i) => {
                    if step.rule.is_negated_goal() {
                        format!("clause {} (negating goal): ", i)
                    } else {
                        format!("clause {}: ", i)
                    }
                }
                ProofStepId::Passive(_) => "".to_string(),
                ProofStepId::Final => "final step: ".to_string(),
            };
            self.print_proof_step(&preface, &step);
        }
        Some(proof)
    }

    // get_uncondensed_proof gets a proof, if we have one.
    // It does not do any simplification of the proof, it's just exactly how we found it.
    // We always include all of the steps that are mathematically necessary for the proof.
    // The include_inspiration flag determines whether we include the "inspiration" steps,
    // which the prover used to find the proof, but are not needed for the proof to be valid.
    fn get_uncondensed_proof(&self, include_inspiration: bool) -> Option<Proof> {
        let final_step = match &self.final_step {
            Some(step) => step,
            None => return None,
        };
        let mut useful_active = HashSet::new();
        self.active_set
            .find_upstream(&final_step, include_inspiration, &mut useful_active);
        for step in &self.useful_passive {
            self.active_set
                .find_upstream(step, include_inspiration, &mut useful_active);
        }
        let negated_goal = match &self.goal {
            Some(NormalizedGoal::ProveNegated(negated_goal, _)) => negated_goal,
            _ => return None,
        };

        let difficulty = if self.nonfactual_activations > Self::VERIFICATION_LIMIT {
            // Verification mode won't find this proof, so we definitely need a shorter one
            Difficulty::Complicated
        } else if self.nonfactual_activations > 500 {
            // Arbitrary heuristic
            Difficulty::Intermediate
        } else {
            Difficulty::Simple
        };

        let mut proof = Proof::new(&self.normalizer, negated_goal, difficulty);
        let mut active_ids: Vec<_> = useful_active.iter().collect();
        active_ids.sort();
        for i in active_ids {
            let step = self.active_set.get_step(*i);
            proof.add_step(ProofStepId::Active(*i), step);
        }
        for (i, step) in self.useful_passive.iter().enumerate() {
            proof.add_step(ProofStepId::Passive(i as u32), step);
        }
        proof.add_step(ProofStepId::Final, final_step);
        Some(proof)
    }

    // Returns a condensed proof, if we have a proof.
    // The condensed proof is what we recommend inserting into the code.
    pub fn get_condensed_proof(&self) -> Option<Proof> {
        let mut proof = self.get_uncondensed_proof(false)?;
        proof.condense();
        Some(proof)
    }

    fn report_term_graph_contradiction(&mut self, contradiction: TermGraphContradiction) {
        let mut active_ids = vec![];
        let mut passive_ids = vec![];
        let mut new_clauses = HashSet::new();
        let mut max_depth = 0;
        let inequality_step = self.active_set.get_step(contradiction.inequality_id);
        let mut truthiness = inequality_step.truthiness;
        for (left, right, rewrite_info) in contradiction.rewrite_chain {
            let rewrite_step = self.active_set.get_step(rewrite_info.pattern_id);
            truthiness = truthiness.combine(rewrite_step.truthiness);

            // Check whether we need to explicitly add a specialized clause to the proof.
            let inspiration_id = match rewrite_info.inspiration_id {
                Some(id) => id,
                None => {
                    // No extra specialized clause needed
                    active_ids.push(rewrite_info.pattern_id);
                    max_depth = max_depth.max(rewrite_step.depth);
                    continue;
                }
            };

            // Create a new proof step, without activating it, to express the
            // specific equality used by this rewrite.
            let literal = Literal::equals(left, right);
            let clause = Clause::new(vec![literal]);
            if new_clauses.contains(&clause) {
                // We already created a step for this equality
                // TODO: is it really okay to not insert any sort of id here?
                continue;
            }
            new_clauses.insert(clause.clone());
            let step = ProofStep::specialization(
                rewrite_info.pattern_id,
                inspiration_id,
                rewrite_step,
                clause,
            );
            max_depth = max_depth.max(step.depth);
            let passive_id = self.useful_passive.len() as u32;
            self.useful_passive.push(step);
            passive_ids.push(passive_id);
        }

        active_ids.sort();
        active_ids.dedup();

        self.final_step = Some(ProofStep::multiple_rewrite(
            contradiction.inequality_id,
            active_ids,
            passive_ids,
            truthiness,
            max_depth,
        ));
    }

    fn report_passive_contradiction(&mut self, passive_steps: Vec<ProofStep>) {
        assert!(self.useful_passive.is_empty());
        for mut passive_step in passive_steps {
            passive_step.printable = false;
            self.useful_passive.push(passive_step);
        }
        self.final_step = Some(ProofStep::passive_contradiction(&self.useful_passive));
    }

    // Activates the next clause from the queue, unless we're already done.
    // Returns whether the prover finished.
    pub fn activate_next(&mut self) -> bool {
        if self.final_step.is_some() {
            return true;
        }

        if let Some(passive_steps) = self.passive_set.get_contradiction() {
            self.report_passive_contradiction(passive_steps);
            return true;
        }

        let step = match self.passive_set.pop() {
            Some(step) => step,
            None => {
                // We're out of clauses to process, so we can't make any more progress.
                return true;
            }
        };

        if step.truthiness != Truthiness::Factual {
            self.nonfactual_activations += 1;
        }

        if step.clause.is_impossible() {
            self.final_step = Some(step);
            return true;
        }

        if self.verbose {
            let prefix = match step.truthiness {
                Truthiness::Factual => " fact",
                Truthiness::Hypothetical => " hypothesis",
                Truthiness::Counterfactual => {
                    if step.rule.is_negated_goal() {
                        " negated goal"
                    } else {
                        ""
                    }
                }
            };
            println!("activating{}: {}", prefix, self.display(&step.clause));
        }
        self.activate(step)
    }

    // Generates new passive clauses, simplifying appropriately, and adds them to the passive set.
    //
    // This does two forms of simplification. It simplifies all existing passive clauses based on
    // the newly activated clause, and simplifies the new passive clauses based on all
    // existing active clauses.
    //
    // This double simplification ensures that every passive clause is always simplified with
    // respect to every active clause.
    //
    // Returns whether the prover finished.
    fn activate(&mut self, activated_step: ProofStep) -> bool {
        // Use the step for simplification
        let activated_id = self.active_set.next_id();
        if activated_step.clause.literals.len() == 1 {
            self.passive_set.simplify(activated_id, &activated_step);
        }

        // Generate new clauses
        let (alt_activated_id, generated_steps) = self.active_set.activate(activated_step);
        assert_eq!(activated_id, alt_activated_id);

        let len = generated_steps.len();
        if self.verbose {
            println!(
                "  generated {} new clause{}",
                len,
                if len == 1 { "" } else { "s" }
            );
        }
        let mut new_steps = vec![];
        for step in generated_steps {
            if step.finishes_proof() {
                self.final_step = Some(step);
                return true;
            }

            if step.automatic_reject() {
                continue;
            }

            if let Some(simple_step) = self.active_set.simplify(step) {
                if simple_step.clause.is_impossible() {
                    self.final_step = Some(simple_step);
                    return true;
                }
                new_steps.push(simple_step);
            }
        }
        self.passive_set.push_batch(new_steps);

        // Sometimes we find a bunch of contradictions at once.
        // It doesn't really matter what we pick, so we guess which is most likely
        // to be aesthetically pleasing.
        // First regular contradictions (in the loop above), then term graph.

        if let Some(contradiction) = self.active_set.graph.get_contradiction() {
            self.report_term_graph_contradiction(contradiction);
            return true;
        }

        false
    }

    // The activation_limit to use for verification mode.
    const VERIFICATION_LIMIT: i32 = 2000;

    // Searches with a short duration.
    // Designed to be called multiple times in succession.
    // The time-based limit is set low, so that it feels interactive.
    pub fn partial_search(&mut self) -> Outcome {
        self.search_for_contradiction(5000, 0.1, false)
    }

    // Search in verification mode to see if this goal can be easily proven.
    // The time-based limit is set high enough so that hopefully it will not apply,
    // because we don't want the result of verification to be machine-dependent.
    pub fn verification_search(&mut self) -> Outcome {
        self.search_for_contradiction(Self::VERIFICATION_LIMIT, 5.0, false)
    }

    // A fast search, for testing.
    pub fn quick_search(&mut self) -> Outcome {
        self.search_for_contradiction(500, 0.2, false)
    }

    // A fast search that only uses shallow steps, for testing.
    pub fn quick_shallow_search(&mut self) -> Outcome {
        self.search_for_contradiction(500, 0.2, true)
    }

    // The prover will exit with Outcome::Constrained if it hits a constraint:
    //   Activating activation_limit nonfactual clauses
    //   Going over the time limit, in seconds
    //   Activating all shallow steps, if shallow_only is set
    pub fn search_for_contradiction(
        &mut self,
        activation_limit: i32,
        seconds: f32,
        shallow_only: bool,
    ) -> Outcome {
        if let Some(s) = &self.error {
            return Outcome::Error(s.clone());
        }
        let start_time = std::time::Instant::now();
        loop {
            if shallow_only && !self.passive_set.all_shallow {
                return Outcome::Exhausted;
            }
            if self.activate_next() {
                // The prover terminated. Determine which outcome that is.
                if let Some(final_step) = &self.final_step {
                    if final_step.truthiness == Truthiness::Counterfactual {
                        // The normal success case
                        return Outcome::Success;
                    }
                    if let Some(NormalizedGoal::ProveNegated(_, true)) = self.goal {
                        // We found an inconsistency in our assumptions, but it's okay
                        return Outcome::Success;
                    }
                    // We found an inconsistency and it's not okay
                    return Outcome::Inconsistent;
                }
                return Outcome::Exhausted;
            }
            for stop_flag in &self.stop_flags {
                if stop_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    return Outcome::Interrupted;
                }
            }
            if self.nonfactual_activations >= activation_limit {
                if self.verbose {
                    println!("activations hit the limit: {}", activation_limit);
                }
                return Outcome::Constrained;
            }
            let elapsed = start_time.elapsed().as_secs_f32();
            if elapsed >= seconds {
                if self.verbose {
                    println!("active set size: {}", self.active_set.len());
                    println!("nonfactual activations: {}", self.nonfactual_activations);
                    println!("prover hit time limit after {} seconds", elapsed);
                }
                return Outcome::Timeout;
            }
        }
    }

    fn display<'a>(&'a self, clause: &'a Clause) -> DisplayClause<'a> {
        DisplayClause {
            clause,
            normalizer: &self.normalizer,
        }
    }

    fn get_clause(&self, id: ProofStepId) -> &Clause {
        match id {
            ProofStepId::Active(i) => self.active_set.get_clause(i),
            ProofStepId::Passive(i) => &self.useful_passive[i as usize].clause,
            ProofStepId::Final => {
                let final_step = self.final_step.as_ref().unwrap();
                &final_step.clause
            }
        }
    }

    // Attempts to convert this clause to code, but shows the clause form if that's all we can.
    fn clause_to_code(&self, bindings: &BindingMap, clause: &Clause) -> String {
        let denormalized = self.normalizer.denormalize(clause);
        if let Ok(code) = bindings.value_to_code(&denormalized) {
            return code;
        }
        self.display(clause).to_string()
    }

    // Convert a clause to a jsonable form
    // We only take active ids, because the others have no external meaning.
    // If we are given a binding map, use it to make a nicer-looking display.
    pub fn to_clause_info(
        &self,
        bindings: &BindingMap,
        id: Option<usize>,
        clause: &Clause,
    ) -> ClauseInfo {
        let text = if clause.is_impossible() {
            None
        } else {
            Some(self.clause_to_code(bindings, clause))
        };
        ClauseInfo { text, id }
    }

    fn to_proof_step_info(
        &self,
        project: &Project,
        bindings: &BindingMap,
        active_id: Option<usize>,
        step: &ProofStep,
    ) -> ProofStepInfo {
        let clause = self.to_clause_info(bindings, active_id, &step.clause);
        let mut premises = vec![];
        for (description, id) in self.descriptive_dependencies(&step) {
            let clause = self.get_clause(id);
            let clause_info = self.to_clause_info(bindings, id.active_id(), clause);
            premises.push((description, clause_info));
        }
        let (rule, location) = match &step.rule {
            Rule::Assumption(info) => {
                let location = project
                    .path_from_module_id(info.source.module)
                    .and_then(|path| Url::from_file_path(path).ok())
                    .map(|uri| Location {
                        uri,
                        range: info.source.range,
                    });

                (info.source.description(), location)
            }
            _ => (step.rule.name().to_lowercase(), None),
        };
        ProofStepInfo {
            clause,
            premises,
            rule,
            location,
            depth: step.depth,
        }
    }

    // Call this after the prover succeeds to get the proof steps in jsonable form.
    pub fn to_proof_info(
        &self,
        project: &Project,
        bindings: &BindingMap,
        proof: &Proof,
    ) -> Vec<ProofStepInfo> {
        let mut result = vec![];
        for (step_id, step) in &proof.all_steps {
            result.push(self.to_proof_step_info(project, bindings, step_id.active_id(), step));
        }
        result
    }

    // Generates information about a clause in jsonable format.
    // Returns None if we don't have any information about this clause.
    pub fn info_result(
        &self,
        project: &Project,
        bindings: &BindingMap,
        id: usize,
    ) -> Option<InfoResult> {
        // Information for the step that proved this clause
        if !self.active_set.has_step(id) {
            return None;
        }
        let step =
            self.to_proof_step_info(project, bindings, Some(id), self.active_set.get_step(id));
        let mut consequences = vec![];
        let mut num_consequences = 0;
        let limit = 100;

        // Check if the final step is a consequence of this clause
        if let Some(final_step) = &self.final_step {
            if final_step.depends_on_active(id) {
                consequences.push(self.to_proof_step_info(project, bindings, None, &final_step));
                num_consequences += 1;
            }
        }

        // Check the active set for consequences
        for (i, step) in self.active_set.find_consequences(id) {
            if consequences.len() < limit {
                consequences.push(self.to_proof_step_info(project, bindings, Some(i), step));
            }
            num_consequences += 1;
        }

        // Check the passive set for consequences
        for step in self.passive_set.find_consequences(id) {
            if consequences.len() < limit {
                consequences.push(self.to_proof_step_info(project, bindings, None, step));
            }
            num_consequences += 1;
        }

        Some(InfoResult {
            step,
            consequences,
            num_consequences,
        })
    }

    // Should only be called after proving completes successfully.
    // Gets the qualified name of every fact that was used in the proof.
    // This includes the "inspiration" facts that were used to find the proof but are
    // not mathematically necessary for the proof to be valid.
    pub fn get_useful_fact_names(&self, names: &mut HashSet<(ModuleId, String)>) {
        let proof = match self.get_uncondensed_proof(true) {
            Some(proof) => proof,
            None => return,
        };
        for (_, step) in proof.all_steps {
            if let Rule::Assumption(ai) = &step.rule {
                if !ai.source.importable {
                    // Non-importable facts are local ones that don't count.
                    continue;
                }
                if let Some(qn) = ai.source.qualified_fact_name() {
                    names.insert(qn);
                }
            }
        }
    }
}
