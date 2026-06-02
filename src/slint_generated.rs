//! Re-export of the types generated from `ui/app.slint`.
//!
//! `slint::include_modules!()` can only be called from one site in the
//! crate. Centralising it here lets every module refer to `AppWindow`,
//! `AgentItem`, and `ModelItem` by name.

slint::include_modules!();
