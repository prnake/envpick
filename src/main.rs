//! Entry point. All the work lives in the library so it can be tested; this
//! only wires the command line to [`handlers::dispatch`].

use clap::Parser;

fn main() {
    let cli = envpick::cli::Cli::parse();
    if let Err(e) = envpick::handlers::dispatch(cli) {
        // `{:#}` prints the whole context chain, so the outermost message the
        // user sees still names the file or URL that actually failed.
        eprintln!("envpick: {e:#}");
        std::process::exit(1);
    }
}
