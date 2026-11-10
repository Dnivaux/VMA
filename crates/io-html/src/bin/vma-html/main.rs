//! `vma-html`: the CEF helper process of the HTML input (ADR 0009).
//!
//! The same executable is also every Chromium sub-process (renderer, GPU, utility): CEF relaunches
//! it with `--type=…`, and `execute_process` runs that sub-process and returns its exit code.
//!
//! Modes:
//! - `--vma-connect=ADDR --vma-token=TOKEN`: the service the application launches
//!   (`service.rs`);
//! - `--vma-probe`: the CEF trial of Phase 4 step 4, a report on stdout (`probe.rs`).

mod glue;
mod probe;
mod proc_stats;
mod service;

use std::process::ExitCode;
use std::rc::Rc;

enum Mode {
    /// A Chromium sub-process.
    Chromium,
    Service(vma_io_html::protocol::Connect, Rc<service::Service>),
    Probe(probe::Options, probe::Shared),
}

fn mode(args: &[String]) -> Result<Mode, String> {
    if args.iter().any(|a| a.starts_with("--type=")) {
        return Ok(Mode::Chromium);
    }
    if let Some(options) = vma_io_html::protocol::Connect::from_args(args) {
        return Ok(Mode::Service(options?, Rc::default()));
    }
    let options = probe::Options::from_args(args.iter().skip(1).cloned())?;
    Ok(Mode::Probe(options, probe::shared_state()))
}

fn main() -> ExitCode {
    // Declares the API version the bindings were generated for; must come before any other call.
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    let list: Vec<String> = std::env::args().collect();
    let mode = match mode(&list) {
        Ok(mode) => mode,
        Err(message) => {
            eprintln!("vma-html: {message}");
            eprintln!("{}", probe::USAGE);
            return ExitCode::from(2);
        }
    };
    let events: glue::Shared = match &mode {
        Mode::Chromium => Rc::new(glue::NoEvents),
        Mode::Service(_, service) => Rc::clone(service) as glue::Shared,
        Mode::Probe(_, state) => Rc::clone(state) as glue::Shared,
    };

    let args = cef::args::Args::new();
    let mut app = glue::app(events);
    // Sub-process (`--type=…`): runs until Chromium is done with it.
    let code = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if code >= 0 {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }

    match mode {
        Mode::Chromium => ExitCode::from(1),
        Mode::Service(options, service) => service::run(&args, app, &service, &options),
        Mode::Probe(options, state) => probe::run(&args, app, state, &options),
    }
}
