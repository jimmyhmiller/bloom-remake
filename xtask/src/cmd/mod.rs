//! One module per task. Each file is owned by the WP that implements the task (PLAN §8); this list is a dispatch
//! file, frozen after M1.

pub mod bench_report;
pub mod bless;
pub mod check_codegen_abi;
pub mod check_codes;
pub mod check_layers;
pub mod check_sans_io;
pub mod corpus;
pub mod coverage;
pub mod crashcheck;
pub mod fetch_datasets;
pub mod gen_ast;
pub mod gen_codegen_corpus;
pub mod gen_docs;
pub mod release;
