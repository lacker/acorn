// Searches for a proof for a particular goal in an acorn file.
//
// This is the CLI equivalent of what the IDE runs when you click on a proposition.
//
// The user passes the line in externally-visible line number, which starts at 1.
// Don't forget that the release build is much faster than debug.

const USAGE: &str = "cargo run --release --bin=search <module name> <line number>";

use acorn::block::NodeCursor;
use acorn::project::Project;
use acorn::prover::{Outcome, Prover};

#[tokio::main]
async fn main() {
    // Parse command line arguments
    let mut args = std::env::args().skip(1);
    let module_name = args.next().expect(USAGE);
    let external_line_number = args.next().expect(USAGE).parse::<u32>().expect(USAGE);
    let internal_line_number = external_line_number - 1;

    let current_dir = std::env::current_dir().unwrap();
    let mut project = Project::new_local(&current_dir, false).unwrap();
    let module_id = project.load_module_by_name(&module_name).unwrap();
    let env = project.get_env_by_id(module_id).unwrap();
    let path = match env.path_for_line(internal_line_number) {
        Ok(path) => path,
        Err(s) => {
            eprintln!(
                "no proposition for line {} in {}: {}",
                external_line_number, module_name, s
            );
            return;
        }
    };

    let node = NodeCursor::from_path(env, &path);
    let goal_context = node.goal_context().unwrap();
    println!("proving {} ...", goal_context.description);
    let verbose = true;
    let mut prover = Prover::new(&project, verbose);
    prover.strict_codegen = true;
    for fact in node.usable_facts(&project) {
        prover.add_fact(fact);
    }
    prover.set_goal(&goal_context);

    loop {
        let outcome = prover.partial_search();

        match outcome {
            Outcome::Success => {
                println!("success!");

                prover.get_and_print_proof(&project, &env.bindings);
                let proof = prover.get_condensed_proof().unwrap();
                match proof.to_code(&env.bindings) {
                    Ok(code) => {
                        println!("generated code:\n");
                        for line in &code {
                            println!("{}", line);
                        }
                    }
                    Err(e) => {
                        eprintln!("\nerror generating code: {}", e);
                    }
                }
            }
            Outcome::Inconsistent => {
                println!("Found inconsistency!");
                prover.get_and_print_proof(&project, &env.bindings);
            }
            Outcome::Exhausted => {
                println!("All possibilities have been exhausted.");
            }
            Outcome::Timeout => {
                println!("activated {} steps", prover.num_activated());
                continue;
            }
            Outcome::Interrupted => {
                println!("Interrupted.");
            }
            Outcome::Constrained => {
                println!("Constrained.");
            }
            Outcome::Error(s) => {
                println!("Error: {}", s);
            }
        }

        break;
    }
}
