use crate::code_generator::Error;
use crate::environment::Environment;
use crate::module::LoadState;
use crate::project::Project;
use crate::prover::{Outcome, Prover};

// Helper to do a proof for a particular goal.
fn prove<'a>(
    project: &'a mut Project,
    module_name: &str,
    goal_name: &str,
) -> (&'a Project, &'a Environment, Prover, Outcome) {
    let module_id = project
        .load_module_by_name(module_name)
        .expect("load failed");
    let load_state = project.get_module_by_id(module_id);
    let env = match load_state {
        LoadState::Ok(env) => env,
        LoadState::Error(e) => panic!("module loading error: {}", e),
        _ => panic!("no module"),
    };
    let node = env.get_node_by_description(goal_name);
    let facts = node.usable_facts(project);
    let goal_context = node.goal_context().unwrap();
    let mut prover = Prover::new(&project, false);
    for fact in facts {
        prover.add_fact(fact);
    }
    prover.set_goal(&goal_context);
    prover.verbose = true;
    prover.strict_codegen = true;
    let outcome = prover.quick_search();
    if let Outcome::Error(s) = outcome {
        panic!("prover error: {}", s);
    }
    (project, env, prover, outcome)
}

// Tries to prove one thing from the project.
// If the proof is successful, try to generate the code.
pub fn prove_with_code(
    project: &mut Project,
    module_name: &str,
    goal_name: &str,
) -> (Prover, Outcome, Result<Vec<String>, Error>) {
    let (project, env, prover, outcome) = prove(project, module_name, goal_name);
    let code = match prover.get_and_print_proof(project, &env.bindings) {
        Some(proof) => proof.to_code(&env.bindings),
        None => Err(Error::NoProof),
    };
    (prover, outcome, code)
}

/// Expects the proof to succeed, and a concrete proof to be generated.
pub fn prove_concrete(
    project: &mut Project,
    module_name: &str,
    goal_name: &str,
) -> Vec<String> {
    let (project, base_env, mut prover, outcome) = prove(project, module_name, goal_name);
    assert_eq!(outcome, Outcome::Success);
    let node = base_env.get_node_by_description(goal_name);
    let env = node.goal_env().unwrap();
    
    match prover.check_concrete(project, &env.bindings, true) {
        Ok(concrete_proof) => concrete_proof,
        Err(e) => panic!("concrete proof check failed: {}", e),
    }
}

pub fn prove_as_main(text: &str, goal_name: &str) -> (Prover, Outcome, Result<Vec<String>, Error>) {
    let mut project = Project::new_mock();
    project.mock("/mock/main.ac", text);
    prove_with_code(&mut project, "main", goal_name)
}

// Does one proof on the provided text.
pub fn prove_text(text: &str, goal_name: &str) -> Outcome {
    prove_as_main(text, goal_name).1
}

// Verifies all the goals in the provided text, returning any non-Success outcome.
pub fn verify(text: &str) -> Outcome {
    let mut project = Project::new_mock();
    project.mock("/mock/main.ac", text);
    let module_id = project.load_module_by_name("main").expect("load failed");
    let env = match project.get_module_by_id(module_id) {
        LoadState::Ok(env) => env,
        LoadState::Error(e) => panic!("error: {}", e),
        _ => panic!("no module"),
    };
    for cursor in env.iter_goals() {
        let facts = cursor.usable_facts(&project);
        let goal_context = cursor.goal_context().unwrap();
        println!("proving: {}", goal_context.description);
        let mut prover = Prover::new(&project, false);
        for fact in facts {
            prover.add_fact(fact);
        }
        prover.set_goal(&goal_context);
        prover.verbose = true;
        // This is a key difference between our verification tests, and our real verification.
        // This helps us test that verification fails in cases where we do have an
        // infinite rabbit hole we could go down.
        let outcome = prover.quick_shallow_search();
        if let Outcome::Error(s) = &outcome {
            println!("prover error: {}", s);
        }
        if outcome != Outcome::Success {
            return outcome;
        }
    }
    Outcome::Success
}

pub fn verify_succeeds(text: &str) {
    let outcome = verify(text);
    if outcome != Outcome::Success {
        panic!(
            "We expected verification to return Success, but we got {}.",
            outcome
        );
    }
}

pub fn verify_fails(text: &str) {
    let outcome = verify(text);
    if outcome != Outcome::Exhausted {
        panic!(
            "We expected verification to return Exhausted, but we got {}.",
            outcome
        );
    }
}

pub fn expect_proof(text: &str, goal_name: &str, expected: &[&str]) {
    let (_, outcome, code) = prove_as_main(text, goal_name);
    assert_eq!(outcome, Outcome::Success);
    let actual = code.expect("code generation failed");
    assert_eq!(actual, expected);
}

// Expects the prover to find a proof that's one of the provided ones.
pub fn expect_proof_in(text: &str, goal_name: &str, expected: &[&[&str]]) {
    let (_, outcome, code) = prove_as_main(text, goal_name);
    assert_eq!(outcome, Outcome::Success);
    let actual = code.expect("code generation failed");

    // There's multiple things it could be that would be fine.
    for e in expected {
        if actual == *e {
            return;
        }
    }

    println!("unexpected code:");
    for line in &actual {
        println!("{}", line);
    }
    panic!("as vec: {:?}", actual);
}

pub const THING: &str = r#"
    type Thing: axiom
    let t: Thing = axiom
    let t2: Thing = axiom
    let f: Thing -> Bool = axiom
    let g: (Thing, Thing) -> Thing = axiom
    let h: Thing -> Thing = axiom
    "#;

// Does one proof in the "thing" environment.
pub fn prove_thing(text: &str, goal_name: &str) -> Outcome {
    let text = format!("{}\n{}", THING, text);
    prove_text(&text, goal_name)
}
