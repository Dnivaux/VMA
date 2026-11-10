//! `vma-html`: the CEF helper process of the HTML input (ADR 0009).
//!
//! The same executable is also every Chromium sub-process (renderer, GPU, utility): CEF relaunches
//! it with `--type=…`, and `execute_process` runs that sub-process and returns its exit code.
//!
//! Phase 4, step 4 (CEF trial): only `--vma-probe` exists for now. It measures what ADR 0009
//! leaves « à vérifier » and prints a report (see `probe.rs`). The command channel and the
//! shared memory of the real helper come at step 5.

mod glue;
mod probe;
mod proc_stats;

use std::process::ExitCode;

fn main() -> ExitCode {
    // Declares the API version the bindings were generated for; must come before any other call.
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    let args = cef::args::Args::new();
    let state = probe::shared_state();
    let mut app = glue::app(state.clone());

    // Sub-process (`--type=…`): runs until Chromium is done with it.
    let code = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if code >= 0 {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }

    let options = match probe::Options::from_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("vma-html: {message}");
            eprintln!("{}", probe::USAGE);
            return ExitCode::from(2);
        }
    };
    probe::run(&args, app, state, &options)
}
