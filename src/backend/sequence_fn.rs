use super::common::qualify_api_path;
use crate::synth::{ArgPlan, CallStep, OutputAdapter, SynthesizedSuite};
use rustc_middle::ty::Ty;
use std::collections::{HashMap, HashSet};

pub(super) trait SequenceFunctionParam {
    fn name(&self) -> &str;
    fn ty(&self) -> &str;
}

pub(super) trait SequenceFunctionRenderer<'tcx> {
    type Param: Clone + SequenceFunctionParam;

    fn prepare_step(
        &mut self,
        _step_index: usize,
        _step: &CallStep,
        _input_tys: &[Ty<'tcx>],
        _output_ty: Ty<'tcx>,
        _lines: &mut Vec<String>,
    ) -> HashMap<usize, String> {
        HashMap::new()
    }

    fn render_arg(
        &mut self,
        step_index: usize,
        arg_index: usize,
        arg: &ArgPlan,
        input_ty: Ty<'tcx>,
        binding_override: Option<&str>,
        lines: &mut Vec<String>,
    ) -> String;

    fn adapt_call_output(&self, adapter: OutputAdapter, call: String) -> String;

    fn wrap_body_lines(&self, body_lines: Vec<String>) -> Vec<String> {
        body_lines
    }

    fn params(&self) -> Vec<Self::Param>;
}

pub(super) struct RenderedSequenceFunction<P> {
    pub params: Vec<P>,
    pub lines: Vec<String>,
}

pub(super) fn render_sequence_function<'tcx, R>(
    renderer: &mut R,
    dependency_crate: &str,
    suite: &SynthesizedSuite<'tcx>,
    index: usize,
) -> RenderedSequenceFunction<R::Param>
where
    R: SequenceFunctionRenderer<'tcx>,
{
    let sequence = &suite.sequences[index];
    let mut body_lines = Vec::new();
    let future_used_steps = future_used_steps(&sequence.steps);

    for (step_index, step) in sequence.steps.iter().enumerate() {
        let api = &suite.apis[step.api_index];
        let binding_overrides = renderer.prepare_step(
            step_index,
            step,
            &api.inputs,
            api.value_output,
            &mut body_lines,
        );
        let rendered_args = step
            .args
            .iter()
            .enumerate()
            .map(|(arg_index, arg)| {
                renderer.render_arg(
                    step_index,
                    arg_index,
                    arg,
                    api.inputs[arg_index],
                    binding_overrides.get(&arg_index).map(String::as_str),
                    &mut body_lines,
                )
            })
            .collect::<Vec<_>>();
        let call = format!(
            "{}({})",
            qualify_api_path(dependency_crate, &api.path),
            rendered_args.join(", ")
        );
        let call = renderer.adapt_call_output(api.output_adapter, call);
        if api.value_output.is_unit() {
            body_lines.push(format!("    {call};"));
        } else if future_used_steps.contains(&step_index) {
            body_lines.push(format!("    let mut value_{step_index} = {call};"));
        } else {
            body_lines.push(format!("    let _ = {call};"));
        }
    }

    let body_lines = renderer.wrap_body_lines(body_lines);
    let params = renderer.params();
    let signature = params
        .iter()
        .map(|param| format!("{}: {}", param.name(), param.ty()))
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = vec![format!(
        "fn test_function{index}({signature}) -> Option<()> {{"
    )];
    lines.extend(body_lines);
    lines.push("    Some(())".to_owned());
    lines.push("}".to_owned());

    RenderedSequenceFunction { params, lines }
}

fn future_used_steps(steps: &[CallStep]) -> HashSet<usize> {
    let mut used = HashSet::new();
    for step in steps {
        for arg in &step.args {
            if let crate::synth::ValueSource::Step(source_step) = arg.source {
                used.insert(source_step);
            }
        }
    }
    used
}
