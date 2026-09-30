#![feature(rustc_private)]

extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_infer;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_public;
extern crate rustc_session;
extern crate rustc_span;
extern crate rustc_trait_selection;

mod backend;
mod cargo_phase;
mod cli;
mod driver;
mod logging;
mod pipeline;
mod rapx_graph;
mod rapx_mono;
mod rapx_resolve;
mod std_adapters;
mod synth;
mod toolchain;
mod type_relations;
mod unsafe_analysis;
mod workspace;

pub use cargo_phase::run_from_env;
