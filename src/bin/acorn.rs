// The Acorn CLI.
// You can run a language server, verify a file, or verify the whole project.

use acorn::server::{run_server, ServerArgs};
use acorn::verifier::{Verifier, VerifierMode};
use clap::Parser;

const VERSION: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/VERSION"));

#[derive(Parser)]
struct Args {
    // When set, print the version and exit.
    #[clap(long, short)]
    version: bool,

    // The root folder the user has open.
    // Only relevant in language server mode.
    #[clap(long)]
    workspace_root: Option<String>,

    // The root folder of the extension.
    // Presence of this flag indicates that we should run in language server mode.
    #[clap(long)]
    extension_root: Option<String>,

    // The following flags only apply in CLI mode.

    // Verify a single module.
    // Can be either a filename or a module name.
    #[clap()]
    target: Option<String>,

    // Create a dataset from the prover logs.
    #[clap(long)]
    dataset: bool,

    // If --full is set, ignore the cache and do a full reverify.
    #[clap(long)]
    full: bool,

    // Use the cache, but only for the filtered prover, not for hash checking.
    // Incompatible with --full.
    #[clap(long)]
    filtered: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Print the version and exit.
    if args.version {
        println!("{}", VERSION);
        return;
    }

    // Check for language server mode.
    if let Some(extension_root) = args.extension_root {
        let server_args = ServerArgs {
            workspace_root: args.workspace_root,
            extension_root,
        };
        run_server(&server_args).await;
        return;
    }

    if args.workspace_root.is_some() {
        println!("--workspace-root is only relevant in language server mode.");
        std::process::exit(1);
    }

    // Run the verifier.
    let mode = if args.full {
        if args.filtered {
            println!("--full and --filtered are incompatible.");
            std::process::exit(1);
        }
        VerifierMode::Full
    } else if args.filtered {
        VerifierMode::Filtered
    } else {
        VerifierMode::Standard
    };
    
    let current_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            println!("Error getting current directory: {}", e);
            std::process::exit(1);
        }
    };
    
    let verifier = Verifier::new(current_dir, mode, args.target, args.dataset);
    if let Err(e) = verifier.run() {
        println!("{}", e);
        std::process::exit(1);
    }
}
