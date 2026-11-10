//! Off-screen HTML input (CEF) with alpha. Optional, Phase 4.
//!
//! The application side (commands to `vma-html`, shared memory, feed) comes at step 5 of
//! `docs/specs/phase-4.md`. The CEF helper itself is the `vma-html` binary (`src/bin/vma-html`,
//! feature `cef`): this library never links to CEF.
