// A representative run of the prover, to use for profiling.
// Try:
//   cargo flamegraph --bin=profiling

use acorn::project::Project;

fn main() {
    let mut project = Project::new("math");
    assert!(project.add_target("nat"));
    assert!(project.add_target("nat_gcd"));
    assert!(project.add_target("int"));
    project.build(&mut |event| {
        if let Some(m) = event.log_message {
            println!("{}", m);
        }
        if let Some((d, t)) = event.progress {
            if d == t {
                println!("{}/{} done", d, t);
            }
        }
    });
}
