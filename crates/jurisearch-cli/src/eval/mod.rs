//! Built-in retrieval-evaluation harness, one submodule per benchmark family:
//! generic (`eval run`/`tune`/`phase1`), france_legi, france_juris, zones, plus the shared
//! artifact helpers. `emit_eval` dispatches each `eval` subcommand to its payload builder.

use crate::*;

pub(crate) mod artifact;
pub(crate) mod france_juris;
pub(crate) mod france_legi;
pub(crate) mod generic;
pub(crate) mod zones;

pub(crate) use artifact::*;
pub(crate) use france_juris::*;
pub(crate) use france_legi::*;
pub(crate) use generic::*;
pub(crate) use zones::*;

pub(crate) fn emit_eval(eval: EvalCommand, index_dir: Option<&Path>) -> anyhow::Result<()> {
    match eval.command {
        Some(EvalSubcommand::Phase1(args)) => {
            match eval_phase1_payload(args.into_request(index_dir.map(Path::to_path_buf))) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(EvalSubcommand::FranceLegi(args)) => {
            let out_path = args.out.clone();
            match eval_france_legi_payload(args, index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        Some(EvalSubcommand::FranceJuris(args)) => {
            let out_path = args.out.clone();
            match eval_france_juris_payload(args, index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        Some(EvalSubcommand::FranceJurisZones(args)) => {
            let out_path = args.out.clone();
            match eval_france_juris_zones_payload(args, index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        Some(EvalSubcommand::Run(args)) => {
            let out_path = args.out.clone();
            match eval_run_payload(args, RetrievalOptions::default(), index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        Some(EvalSubcommand::Tune(args)) => {
            let out_path = args.out.clone();
            match eval_tune_payload(args, index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        None => emit_error(ErrorObject::bad_input(
            "eval requires a subcommand; try `eval phase1`, `eval france-legi`, or `eval run`",
        )),
    }
}
