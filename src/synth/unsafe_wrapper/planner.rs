use super::super::api::build_api_descriptors;
use super::super::{
    ApiDescriptor, ApiSafety, ArgPlan, BorrowMode, CallStep, SequencePlan, SynthesizedSuite,
    ValueSource,
};
use super::{
    DEFAULT_BASIC_SEQUENCE_LIMIT, DEFAULT_MUTATION_ROUND_LIMIT, MIN_MUTATION_KIDS_PER_PARENT,
    MutatorPlan, NEAREST_TARGET_THRESHOLD, NO_MUTATOR_AFTER_TARGET, PartialMutatorPlan,
};
use crate::rapx_graph::{ApiDependencyGraph, TyWrapper};
use crate::std_adapters::{seed_value_ty, supports_direct_symbolic_input};
use crate::type_relations::{can_supply_to_input, input_requires_constructor_scan};
use crate::unsafe_analysis::ApiFieldFactsIndex;
use rustc_middle::ty::{self, Ty, TyCtxt, TyKind};
use std::collections::{HashMap, HashSet};

pub(super) struct Planner<'tcx> {
    tcx: TyCtxt<'tcx>,
    apis: Vec<ApiDescriptor<'tcx>>,
    constructors: HashMap<TyWrapper<'tcx>, Vec<usize>>,
    mutators: HashMap<TyWrapper<'tcx>, Vec<(usize, usize)>>,
    basic_cache: HashMap<usize, Vec<Vec<CallStep>>>,
    mutator_plan_cache: HashMap<(usize, usize), Option<Vec<MutatorPlan>>>,
    max_depth: usize,
    max_mutators_per_target: usize,
}

impl<'tcx> Planner<'tcx> {
    pub(super) fn new(
        tcx: TyCtxt<'tcx>,
        graph: &ApiDependencyGraph<'tcx>,
        field_facts_index: &ApiFieldFactsIndex,
        max_depth: usize,
        max_mutators_per_target: usize,
    ) -> Self {
        let apis = build_api_descriptors(tcx, graph, field_facts_index);
        let mut constructors: HashMap<TyWrapper<'tcx>, Vec<usize>> = HashMap::new();
        let mut mutators: HashMap<TyWrapper<'tcx>, Vec<(usize, usize)>> = HashMap::new();

        for api in &apis {
            if !api.supported {
                continue;
            }

            if !is_unit_ty(api.value_output)
                && !api
                    .inputs
                    .iter()
                    .any(|input| normalize_value_ty(*input) == api.value_output_key)
            {
                constructors
                    .entry(api.value_output_key)
                    .or_default()
                    .push(api.index);
            }

            for (param_index, input) in api.inputs.iter().enumerate() {
                if let TyKind::Ref(_, inner, ty::Mutability::Mut) = input.kind() {
                    if relevant_fields_for_param(api, param_index).is_empty() {
                        continue;
                    }
                    mutators
                        .entry(TyWrapper::from(*inner))
                        .or_default()
                        .push((api.index, param_index));
                }
            }
        }

        Self {
            tcx,
            apis,
            constructors,
            mutators,
            basic_cache: HashMap::new(),
            mutator_plan_cache: HashMap::new(),
            max_depth,
            max_mutators_per_target,
        }
    }

    fn max_depth_allows_len(&self, len: usize) -> bool {
        self.max_depth == 0 || len <= self.max_depth
    }

    fn max_depth_allows_prefix(&self, len: usize) -> bool {
        self.max_depth == 0 || len < self.max_depth
    }

    pub(super) fn synthesize(mut self) -> SynthesizedSuite<'tcx> {
        let mut sequences = Vec::new();
        let target_indices = self
            .apis
            .iter()
            .filter(|api| api.supported && api.api_safety == ApiSafety::UnsafeBlock)
            .map(|api| api.index)
            .collect::<Vec<_>>();

        for target_api in target_indices {
            let mut seen_signatures = HashSet::new();
            let basic_sequences = rank_and_limit_basic_sequences(
                self.basic_sequences_ended_with(target_api, &mut Vec::new()),
                &self.apis,
            );
            let mut target_sequences = Vec::new();

            for steps in basic_sequences {
                let signature = sequence_signature(&steps);
                if !seen_signatures.insert(signature) {
                    continue;
                }

                target_sequences.push(SequencePlan {
                    target_api,
                    unsafe_wrapper: self.apis[target_api].primary_unsafe_wrapper(),
                    target_step: steps.len() - 1,
                    latest_call: steps.len() - 1,
                    mutated_param: None,
                    ranking_value: steps.len() as f32,
                    predecessor: None,
                    successor: Vec::new(),
                    steps,
                });
            }

            let mut start_index = 0usize;
            for round in 0..self.max_mutators_per_target {
                let current_len = target_sequences.len();
                let mut new_sequences = Vec::new();

                for (parent_index, parent) in target_sequences[start_index..current_len]
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(offset, parent)| (start_index + offset, parent))
                {
                    for mut candidate in self.expand_with_mutators(&parent) {
                        let signature = sequence_signature(&candidate.steps);
                        if !seen_signatures.insert(signature) {
                            continue;
                        }
                        candidate.predecessor = Some(parent_index);
                        candidate.target_api = target_api;
                        new_sequences.push(candidate);
                    }
                }

                let mut selected = rank_and_limit_mutation_candidates(
                    &target_sequences,
                    new_sequences,
                    round,
                    &self.apis,
                );
                if selected.is_empty() {
                    break;
                }
                for (index, sequence) in selected.iter().enumerate() {
                    let predecessor = sequence
                        .predecessor
                        .expect("selected mutator sequence should have a predecessor");
                    target_sequences[predecessor]
                        .successor
                        .push(current_len + index);
                }
                start_index = current_len;
                target_sequences.append(&mut selected);
            }

            let base_offset = sequences.len();
            for sequence in &mut target_sequences {
                if let Some(predecessor) = sequence.predecessor {
                    sequence.predecessor = Some(base_offset + predecessor);
                }
                for successor in &mut sequence.successor {
                    *successor += base_offset;
                }
            }
            sequences.extend(target_sequences);
        }

        SynthesizedSuite {
            apis: self.apis,
            sequences,
        }
    }

    fn basic_sequences_ended_with(
        &mut self,
        api_index: usize,
        api_stack: &mut Vec<usize>,
    ) -> Vec<Vec<CallStep>> {
        if let Some(cached) = self.basic_cache.get(&api_index) {
            return cached.clone();
        }

        if api_stack.contains(&api_index) {
            return Vec::new();
        }

        let api = &self.apis[api_index];
        if !api.supported {
            self.basic_cache.insert(api_index, Vec::new());
            return Vec::new();
        }

        api_stack.push(api_index);
        let mut prefixes = vec![Vec::new()];

        for (param_index, input) in api.inputs.clone().into_iter().enumerate() {
            let mut next_prefixes = Vec::new();
            if self.prefers_direct_symbolic_input(input) {
                next_prefixes.extend(prefixes.iter().cloned());
                prefixes = dedupe_step_sequences(next_prefixes);
                continue;
            }

            let mut constructor_sequences = Vec::new();
            for constructor in self.constructor_candidates_for_input(api_index, param_index, input)
            {
                constructor_sequences
                    .extend(self.basic_sequences_ended_with(constructor, api_stack));
            }

            if constructor_sequences.is_empty() {
                if self.symbolic_seed_expr(input).is_some() {
                    next_prefixes.extend(prefixes.iter().cloned());
                }
                if !next_prefixes.is_empty() {
                    prefixes = dedupe_step_sequences(next_prefixes);
                }
                continue;
            }

            for prefix in &prefixes {
                for constructor_steps in &constructor_sequences {
                    let merged = merge_steps(prefix, constructor_steps);
                    if self.max_depth_allows_prefix(merged.len()) {
                        next_prefixes.push(merged);
                    }
                }
            }

            prefixes = dedupe_step_sequences(next_prefixes);
        }

        api_stack.pop();

        let mut sequences = Vec::new();
        for prefix in prefixes {
            if !self.max_depth_allows_prefix(prefix.len()) {
                continue;
            }

            let Some(steps) = self.append_api_call(api_index, &prefix) else {
                continue;
            };
            if !self.max_depth_allows_len(steps.len()) {
                continue;
            }
            if !prefix.is_empty()
                && !steps
                    .last()
                    .expect("appended call should exist")
                    .args
                    .iter()
                    .any(|arg| {
                        matches!(&arg.source, ValueSource::Step(source) if *source < prefix.len())
                    })
            {
                continue;
            }
            if has_unused_prefix_steps(&steps) {
                continue;
            }
            sequences.push(steps);
        }

        let sequences = dedupe_step_sequences(sequences);
        self.basic_cache.insert(api_index, sequences.clone());
        sequences
    }

    fn append_api_call(&self, api_index: usize, prefix: &[CallStep]) -> Option<Vec<CallStep>> {
        let api = &self.apis[api_index];
        let mut steps = prefix.to_vec();
        let mut args = Vec::with_capacity(api.inputs.len());
        let mut moved_steps = moved_steps_in_prefix(prefix);
        let mut local_mutable_borrows = HashSet::new();
        let mut local_shared_borrows = HashSet::new();
        let mut next_param_index = symbolic_param_count(prefix);

        for input in &api.inputs {
            let borrow = borrow_mode_for_input(*input);
            let mut satisfied = None;

            for (source_step, source) in prefix.iter().enumerate() {
                if moved_steps.contains(&source_step) {
                    continue;
                }
                if !can_supply_to_input(self.tcx, self.apis[source.api_index].value_output, *input)
                {
                    continue;
                }
                if !borrow_is_available(
                    borrow,
                    source_step,
                    &local_mutable_borrows,
                    &local_shared_borrows,
                ) {
                    continue;
                }

                match borrow {
                    BorrowMode::Move => {
                        moved_steps.insert(source_step);
                    }
                    BorrowMode::Shared | BorrowMode::RawConst => {
                        local_shared_borrows.insert(source_step);
                    }
                    BorrowMode::Mutable | BorrowMode::RawMutable => {
                        local_mutable_borrows.insert(source_step);
                    }
                }

                satisfied = Some(ArgPlan {
                    source: ValueSource::Step(source_step),
                    borrow,
                });
                break;
            }

            if let Some(arg) = satisfied {
                args.push(arg);
                continue;
            }

            self.symbolic_seed_expr(*input)?;
            args.push(ArgPlan {
                source: ValueSource::Param(next_param_index),
                borrow,
            });
            next_param_index += 1;
        }

        steps.push(CallStep { api_index, args });
        Some(steps)
    }

    fn constructor_candidates_for_input(
        &self,
        consumer_api: usize,
        param_index: usize,
        input: Ty<'tcx>,
    ) -> Vec<usize> {
        let required_fields = required_fields_for_param(&self.apis[consumer_api], param_index);
        let candidates = if input_requires_constructor_scan(input) {
            self.constructors
                .values()
                .flat_map(|bucket| bucket.iter().copied())
                .collect::<Vec<_>>()
        } else {
            let key = normalize_value_ty(input);
            self.constructors.get(&key).cloned().unwrap_or_default()
        };
        candidates
            .into_iter()
            .filter(|api_index| {
                can_supply_to_input(self.tcx, self.apis[*api_index].value_output, input)
            })
            .filter(|api_index| {
                required_fields.is_empty()
                    || self.apis[*api_index]
                        .field_facts
                        .output_overlaps_relevant(&required_fields)
            })
            .collect()
    }

    fn symbolic_seed_expr(&self, ty: Ty<'tcx>) -> Option<String> {
        if supports_direct_symbolic_input(self.tcx, ty) {
            return Some(seed_value_ty(ty).to_string());
        }
        None
    }

    fn prefers_direct_symbolic_input(&self, input: Ty<'tcx>) -> bool {
        self.symbolic_seed_expr(input).is_some() && !is_local_user_adt(seed_value_ty(input))
    }

    fn expand_with_mutators(&mut self, base: &SequencePlan) -> Vec<SequencePlan> {
        let mut sequences = Vec::new();

        for call_index in (0..base.steps.len()).rev() {
            let call = &base.steps[call_index];
            let inputs = self.apis[call.api_index].inputs.clone();

            for (param_index, arg) in call.args.iter().enumerate() {
                let Some(input) = inputs.get(param_index) else {
                    continue;
                };
                let TyKind::Ref(_, inner, ty::Mutability::Mut) = input.kind() else {
                    continue;
                };
                let Some(mutator_candidates) = self.mutators.get(&TyWrapper::from(*inner)).cloned()
                else {
                    continue;
                };
                let required_fields =
                    required_fields_for_param(&self.apis[call.api_index], param_index);
                if required_fields.is_empty() {
                    continue;
                }

                match &arg.source {
                    ValueSource::Step(source_step) => {
                        for (mutator_api, mutated_param_index) in mutator_candidates {
                            if !self.apis[mutator_api]
                                .field_facts
                                .input_overlaps_mutated(mutated_param_index, &required_fields)
                            {
                                continue;
                            }
                            let Some(plans) =
                                self.build_mutator_plans(mutator_api, mutated_param_index)
                            else {
                                continue;
                            };
                            for plan in &plans {
                                let Some(sequence) = self.insert_mutator_after(
                                    base,
                                    *source_step,
                                    mutator_api,
                                    mutated_param_index,
                                    plan,
                                ) else {
                                    continue;
                                };
                                sequences.push(sequence);
                            }
                        }
                    }
                    ValueSource::Param(symbolic_param) => {
                        for (mutator_api, mutated_param_index) in mutator_candidates {
                            if !self.apis[mutator_api]
                                .field_facts
                                .input_overlaps_mutated(mutated_param_index, &required_fields)
                            {
                                continue;
                            }
                            let Some(plans) =
                                self.build_mutator_plans(mutator_api, mutated_param_index)
                            else {
                                continue;
                            };
                            for plan in &plans {
                                let Some(sequence) = self.insert_mutator_before_call(
                                    base,
                                    call_index,
                                    *symbolic_param,
                                    mutator_api,
                                    mutated_param_index,
                                    plan,
                                ) else {
                                    continue;
                                };
                                sequences.push(sequence);
                            }
                        }
                    }
                }
            }
        }

        if !NO_MUTATOR_AFTER_TARGET {
            let target_output = self.apis[base.target_api].value_output;
            if !is_unit_ty(target_output)
                && let Some(mutator_candidates) =
                    self.mutators.get(&TyWrapper::from(target_output)).cloned()
            {
                for (mutator_api, mutated_param_index) in mutator_candidates {
                    let Some(plans) = self.build_mutator_plans(mutator_api, mutated_param_index)
                    else {
                        continue;
                    };
                    for plan in &plans {
                        let Some(sequence) = self.insert_mutator_after(
                            base,
                            base.target_step,
                            mutator_api,
                            mutated_param_index,
                            plan,
                        ) else {
                            continue;
                        };
                        sequences.push(sequence);
                    }
                }
            }
        }

        sequences
    }

    fn insert_mutator_after(
        &mut self,
        base: &SequencePlan,
        insertion_after: usize,
        mutator_api: usize,
        mutated_param_index: usize,
        plan: &MutatorPlan,
    ) -> Option<SequencePlan> {
        let mut new_steps = base.steps[..=insertion_after].to_vec();
        let prefix_offset = new_steps.len();
        let param_offset = insertion_param_offset(base);
        let mut extra_steps = plan.prefix.clone();
        shift_steps(&mut extra_steps, prefix_offset, param_offset);
        new_steps.extend(extra_steps);

        let mut args = Vec::with_capacity(plan.args.len());
        for (param_index, arg) in plan.args.iter().enumerate() {
            if param_index == mutated_param_index {
                args.push(ArgPlan {
                    source: ValueSource::Step(insertion_after),
                    borrow: BorrowMode::Mutable,
                });
                continue;
            }

            let mut arg = arg.clone()?;
            shift_arg_refs(&mut arg, prefix_offset, param_offset);
            args.push(arg);
        }

        new_steps.push(CallStep {
            api_index: mutator_api,
            args,
        });
        let latest_call = new_steps.len() - 1;
        let inserted_count = latest_call - insertion_after;

        for step in &base.steps[insertion_after + 1..] {
            let mut shifted = step.clone();
            shift_step_refs_after(&mut shifted, insertion_after, inserted_count);
            new_steps.push(shifted);
        }

        if !self.max_depth_allows_len(new_steps.len()) {
            return None;
        }

        Some(SequencePlan {
            target_api: base.target_api,
            unsafe_wrapper: base.unsafe_wrapper.clone(),
            target_step: if base.target_step > insertion_after {
                base.target_step + inserted_count
            } else {
                base.target_step
            },
            predecessor: None,
            successor: Vec::new(),
            latest_call,
            mutated_param: Some(mutated_param_index),
            ranking_value: new_steps.len() as f32,
            steps: new_steps,
        })
    }

    fn insert_mutator_before_call(
        &mut self,
        base: &SequencePlan,
        call_index: usize,
        symbolic_param: usize,
        mutator_api: usize,
        mutated_param_index: usize,
        plan: &MutatorPlan,
    ) -> Option<SequencePlan> {
        let mut new_steps = base.steps[..call_index].to_vec();
        let step_offset = new_steps.len();
        let param_offset = insertion_param_offset(base);
        let mut extra_steps = plan.prefix.clone();
        shift_steps(&mut extra_steps, step_offset, param_offset);
        new_steps.extend(extra_steps);

        let mut args = Vec::with_capacity(plan.args.len());
        for (param_index, arg) in plan.args.iter().enumerate() {
            if param_index == mutated_param_index {
                args.push(ArgPlan {
                    source: ValueSource::Param(symbolic_param),
                    borrow: BorrowMode::Mutable,
                });
                continue;
            }

            let mut arg = arg.clone()?;
            shift_arg_refs(&mut arg, step_offset, param_offset);
            args.push(arg);
        }

        new_steps.push(CallStep {
            api_index: mutator_api,
            args,
        });
        let latest_call = new_steps.len() - 1;
        let inserted_count = latest_call + 1 - call_index;

        for step in &base.steps[call_index..] {
            let mut shifted = step.clone();
            shift_step_refs_at_or_after(&mut shifted, call_index, inserted_count);
            new_steps.push(shifted);
        }

        if !self.max_depth_allows_len(new_steps.len()) {
            return None;
        }

        Some(SequencePlan {
            target_api: base.target_api,
            unsafe_wrapper: base.unsafe_wrapper.clone(),
            target_step: if base.target_step >= call_index {
                base.target_step + inserted_count
            } else {
                base.target_step
            },
            predecessor: None,
            successor: Vec::new(),
            latest_call,
            mutated_param: Some(mutated_param_index),
            ranking_value: new_steps.len() as f32,
            steps: new_steps,
        })
    }

    fn build_mutator_plans(
        &mut self,
        mutator_api: usize,
        mutated_param_index: usize,
    ) -> Option<Vec<MutatorPlan>> {
        let cache_key = (mutator_api, mutated_param_index);
        if let Some(cached) = self.mutator_plan_cache.get(&cache_key) {
            return cached.clone();
        }

        let inputs = self.apis[mutator_api].inputs.clone();
        let mut partials = vec![PartialMutatorPlan {
            prefix: Vec::new(),
            args: vec![None; inputs.len()],
            next_param_index: 0,
        }];

        for (param_index, input) in inputs.iter().enumerate() {
            if param_index == mutated_param_index {
                continue;
            }

            let borrow = borrow_mode_for_input(*input);
            if self.prefers_direct_symbolic_input(*input) {
                for partial in &mut partials {
                    partial.args[param_index] = Some(ArgPlan {
                        source: ValueSource::Param(partial.next_param_index),
                        borrow,
                    });
                    partial.next_param_index += 1;
                }
                continue;
            }

            let mut constructor_sequences = Vec::new();
            for constructor in
                self.constructor_candidates_for_input(mutator_api, param_index, *input)
            {
                constructor_sequences
                    .extend(self.basic_sequences_ended_with(constructor, &mut Vec::new()));
            }
            if constructor_sequences.is_empty() {
                if self.symbolic_seed_expr(*input).is_some() {
                    for partial in &mut partials {
                        partial.args[param_index] = Some(ArgPlan {
                            source: ValueSource::Param(partial.next_param_index),
                            borrow,
                        });
                        partial.next_param_index += 1;
                    }
                    continue;
                }

                self.mutator_plan_cache.insert(cache_key, None);
                return None;
            }

            let mut next_partials = Vec::new();
            for partial in partials {
                for constructor_steps in &constructor_sequences {
                    let mut prefix = partial.prefix.clone();
                    let offset = prefix.len();
                    let param_offset = partial.next_param_index;
                    let mut shifted_steps = constructor_steps.clone();
                    shift_steps(&mut shifted_steps, offset, param_offset);
                    prefix.extend(shifted_steps);

                    let mut args = partial.args.clone();
                    args[param_index] = Some(ArgPlan {
                        source: ValueSource::Step(prefix.len() - 1),
                        borrow,
                    });

                    next_partials.push(PartialMutatorPlan {
                        prefix,
                        args,
                        next_param_index: partial.next_param_index
                            + symbolic_param_count(constructor_steps),
                    });
                }
            }
            partials = next_partials;
        }

        let mut deduped = Vec::new();
        let mut seen = HashSet::new();
        for partial in partials {
            let plan = MutatorPlan {
                prefix: partial.prefix,
                args: partial.args,
            };
            if seen.insert(plan.clone()) {
                deduped.push(plan);
            }
        }

        self.mutator_plan_cache
            .insert(cache_key, Some(deduped.clone()));
        Some(deduped)
    }
}

fn merge_steps(left: &[CallStep], right: &[CallStep]) -> Vec<CallStep> {
    let mut merged = left.to_vec();
    let mut right = right.to_vec();
    shift_steps(&mut right, merged.len(), symbolic_param_count(left));
    merged.extend(right);
    merged
}

// Rank constructor-rooted basic sequences so we keep short, target-adjacent
// and relatively rare producer paths when the cross-product gets too large.
fn rank_and_limit_basic_sequences(
    sequences: Vec<Vec<CallStep>>,
    _apis: &[ApiDescriptor<'_>],
) -> Vec<Vec<CallStep>> {
    let max_len = sequences.iter().map(Vec::len).max().unwrap_or(0);
    let size_limit = basic_sequence_limit(max_len);
    if sequences.len() <= size_limit {
        return sequences;
    }

    let overall_counts = function_counts_across_sequences(&sequences);
    let mut ranked = sequences
        .into_iter()
        .map(|steps| {
            let distances = distances_to_target(&steps, steps.len().saturating_sub(1));
            let per_sequence_counts = function_counts_in_sequence(&steps);
            let mut retention_priority = 0.0f32;

            for (index, step) in steps.iter().enumerate() {
                if index + 1 == steps.len() {
                    continue;
                }
                if distances[index] > NEAREST_TARGET_THRESHOLD {
                    retention_priority += 1.0;
                    continue;
                }

                let overall_usage = *overall_counts.get(&step.api_index).unwrap_or(&1) as f32;
                let sequence_usage = *per_sequence_counts.get(&step.api_index).unwrap_or(&1) as f32;
                let rarity = sequence_usage / (overall_usage - sequence_usage + 0.2);
                retention_priority += 1.0 - rarity;
            }

            (retention_priority.max(0.0), steps)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| left.0.partial_cmp(&right.0).unwrap());
    ranked.truncate(size_limit);
    ranked.into_iter().map(|(_, steps)| steps).collect()
}

// Keep a bounded but diverse set of mutator-extended sequences by combining
// target-distance priority with per-parent minimum coverage.
fn rank_and_limit_mutation_candidates(
    existing_sequences: &[SequencePlan],
    sequences: Vec<SequencePlan>,
    round: usize,
    _apis: &[ApiDescriptor<'_>],
) -> Vec<SequencePlan> {
    let size_limit = mutation_round_limit(round);
    if sequences.len() <= size_limit {
        return sequences;
    }

    let basic_function_counts = function_counts_across_sequence_plans(
        existing_sequences
            .iter()
            .filter(|sequence| sequence.predecessor.is_none()),
    );
    let mut ranked = sequences
        .into_iter()
        .map(|mut sequence| {
            let predecessor = sequence
                .predecessor
                .expect("mutated sequence should have a predecessor");
            let predecessor_sequence = &existing_sequences[predecessor];
            sequence.ranking_value = predecessor_sequence.ranking_value
                + (sequence.steps.len() - predecessor_sequence.steps.len()) as f32;
            let distances = distances_to_target_for_sequence(&sequence);
            if sequence
                .mutated_param
                .and_then(|_| distances.get(sequence.latest_call))
                .is_some_and(|distance| *distance <= NEAREST_TARGET_THRESHOLD)
            {
                let mutator_counts = sequence
                    .steps
                    .iter()
                    .take(sequence.latest_call + 1)
                    .filter(|step| step.api_index == sequence.steps[sequence.latest_call].api_index)
                    .count() as u32;
                let overall_usage = *basic_function_counts
                    .get(&sequence.steps[sequence.latest_call].api_index)
                    .unwrap_or(&0) as f32;
                let previous_usage = mutator_counts.saturating_sub(1) as f32;
                sequence.ranking_value -= 1.0 / (previous_usage + 1.0) / (overall_usage + 0.2);
            }
            sequence
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .ranking_value
            .partial_cmp(&left.ranking_value)
            .unwrap()
    });

    let mut selected = vec![false; ranked.len()];
    let mut predecessor_counts = vec![0usize; existing_sequences.len()];
    let mut count = 0usize;

    for (index, sequence) in ranked.iter().enumerate() {
        if count >= size_limit {
            break;
        }
        if let Some(predecessor) = sequence.predecessor
            && predecessor_counts[predecessor] < MIN_MUTATION_KIDS_PER_PARENT
        {
            selected[index] = true;
            predecessor_counts[predecessor] += 1;
            count += 1;
        }
    }

    for (index, sequence) in ranked.iter().enumerate() {
        if count >= size_limit {
            break;
        }
        if sequence.predecessor.is_some() && !selected[index] {
            selected[index] = true;
            count += 1;
        }
    }

    ranked
        .into_iter()
        .enumerate()
        .filter_map(|(index, sequence)| selected[index].then_some(sequence))
        .collect()
}

fn dedupe_step_sequences(sequences: Vec<Vec<CallStep>>) -> Vec<Vec<CallStep>> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for steps in sequences {
        let signature = sequence_signature(&steps);
        if seen.insert(signature) {
            deduped.push(steps);
        }
    }
    deduped
}

fn sequence_signature(steps: &[CallStep]) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    steps.hash(&mut hasher);
    hasher.finish()
}

fn normalize_value_ty(ty: Ty<'_>) -> TyWrapper<'_> {
    match ty.kind() {
        TyKind::Ref(_, inner, _) | TyKind::RawPtr(inner, _) => TyWrapper::from(*inner),
        _ => TyWrapper::from(ty),
    }
}

fn borrow_mode_for_input(ty: Ty<'_>) -> BorrowMode {
    match ty.kind() {
        TyKind::Ref(_, _, ty::Mutability::Mut) => BorrowMode::Mutable,
        TyKind::Ref(_, _, ty::Mutability::Not) => BorrowMode::Shared,
        TyKind::RawPtr(_, ty::Mutability::Mut) => BorrowMode::RawMutable,
        TyKind::RawPtr(_, ty::Mutability::Not) => BorrowMode::RawConst,
        _ => BorrowMode::Move,
    }
}

fn required_fields_for_param(
    api: &ApiDescriptor<'_>,
    param_index: usize,
) -> std::collections::BTreeSet<Vec<u32>> {
    relevant_fields_for_param(api, param_index)
}

fn relevant_fields_for_param(
    api: &ApiDescriptor<'_>,
    param_index: usize,
) -> std::collections::BTreeSet<Vec<u32>> {
    let contract_fields = api
        .field_facts
        .contract_inputs
        .get(param_index)
        .cloned()
        .unwrap_or_default();
    if !contract_fields.is_empty() {
        return contract_fields;
    }
    let propagated_fields = api
        .field_facts
        .propagated_inputs
        .get(param_index)
        .cloned()
        .unwrap_or_default();
    if !propagated_fields.is_empty() {
        return propagated_fields;
    }
    api.field_facts
        .mutated_inputs
        .get(param_index)
        .cloned()
        .unwrap_or_default()
}

fn is_local_user_adt(value_ty: Ty<'_>) -> bool {
    let TyKind::Adt(def, _) = value_ty.kind() else {
        return false;
    };
    def.did().is_local() && (def.is_struct() || def.is_enum())
}

fn moved_steps_in_prefix(prefix: &[CallStep]) -> HashSet<usize> {
    let mut moved = HashSet::new();
    for step in prefix {
        for arg in &step.args {
            if matches!(arg.borrow, BorrowMode::Move)
                && let ValueSource::Step(index) = &arg.source
            {
                moved.insert(*index);
            }
        }
    }
    moved
}

fn symbolic_param_count(steps: &[CallStep]) -> usize {
    steps
        .iter()
        .flat_map(|step| step.args.iter())
        .filter_map(|arg| match &arg.source {
            ValueSource::Param(index) => Some(*index),
            _ => None,
        })
        .max()
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn insertion_param_offset(base: &SequencePlan) -> usize {
    symbolic_param_count(&base.steps)
}

fn basic_sequence_limit(max_len: usize) -> usize {
    match max_len {
        1 => 1,
        2..=4 => 32,
        5..=7 => 16,
        _ => DEFAULT_BASIC_SEQUENCE_LIMIT,
    }
}

fn mutation_round_limit(round: usize) -> usize {
    match round {
        0 => DEFAULT_MUTATION_ROUND_LIMIT,
        1 => 64,
        2 | 3 => 32,
        4..=6 => 16,
        _ => DEFAULT_MUTATION_ROUND_LIMIT,
    }
}

fn function_counts_across_sequences(sequences: &[Vec<CallStep>]) -> HashMap<usize, u32> {
    let mut counts = HashMap::new();
    for steps in sequences {
        for step in steps {
            *counts.entry(step.api_index).or_insert(0) += 1;
        }
    }
    counts
}

fn function_counts_in_sequence(steps: &[CallStep]) -> HashMap<usize, u32> {
    let mut counts = HashMap::new();
    for step in steps {
        *counts.entry(step.api_index).or_insert(0) += 1;
    }
    counts
}

fn function_counts_across_sequence_plans<'a>(
    sequences: impl IntoIterator<Item = &'a SequencePlan>,
) -> HashMap<usize, u32> {
    let mut counts = HashMap::new();
    for sequence in sequences {
        for step in &sequence.steps {
            *counts.entry(step.api_index).or_insert(0) += 1;
        }
    }
    counts
}

fn borrow_is_available(
    borrow: BorrowMode,
    source_step: usize,
    mutable_borrows: &HashSet<usize>,
    shared_borrows: &HashSet<usize>,
) -> bool {
    match borrow {
        BorrowMode::Move => {
            !mutable_borrows.contains(&source_step) && !shared_borrows.contains(&source_step)
        }
        BorrowMode::Shared | BorrowMode::RawConst => !mutable_borrows.contains(&source_step),
        BorrowMode::Mutable | BorrowMode::RawMutable => {
            !mutable_borrows.contains(&source_step) && !shared_borrows.contains(&source_step)
        }
    }
}

fn has_unused_prefix_steps(steps: &[CallStep]) -> bool {
    if steps.len() <= 1 {
        return false;
    }

    let used_steps = steps
        .iter()
        .flat_map(|step| &step.args)
        .filter_map(|arg| match arg.source {
            ValueSource::Step(index) => Some(index),
            ValueSource::Param(_) => None,
        })
        .collect::<HashSet<_>>();

    (0..steps.len() - 1).any(|index| !used_steps.contains(&index))
}

fn distances_to_target(steps: &[CallStep], target_step: usize) -> Vec<u32> {
    if steps.is_empty() {
        return Vec::new();
    }

    let mut distances = vec![u32::MAX; steps.len()];
    let mut queue = std::collections::VecDeque::new();
    distances[target_step] = 0;
    queue.push_back(target_step);

    while let Some(step_index) = queue.pop_front() {
        let next_distance = distances[step_index].saturating_add(1);
        for arg in &steps[step_index].args {
            let ValueSource::Step(source_index) = &arg.source else {
                continue;
            };
            if distances[*source_index] != u32::MAX {
                continue;
            }
            distances[*source_index] = next_distance;
            queue.push_back(*source_index);
        }
    }

    for distance in &mut distances {
        if *distance == u32::MAX {
            *distance = 0;
        }
    }
    distances
}

// Re-anchor the latest inserted mutator onto the mutated value's path so
// scoring prefers mutators that stay close to the target object.
fn distances_to_target_for_sequence(sequence: &SequencePlan) -> Vec<u32> {
    let mut distances = distances_to_target(&sequence.steps, sequence.target_step);
    let Some(mutated_param_index) = sequence.mutated_param else {
        return distances;
    };
    let Some(mutator_step) = sequence.steps.get(sequence.latest_call) else {
        return distances;
    };
    let Some(ArgPlan {
        source: ValueSource::Step(source_step),
        ..
    }) = mutator_step.args.get(mutated_param_index)
    else {
        return distances;
    };
    let Some(distance) = distances.get(*source_step).copied() else {
        return distances;
    };
    if let Some(slot) = distances.get_mut(sequence.latest_call) {
        *slot = distance;
    }

    let mut queue = std::collections::VecDeque::new();
    queue.push_back(sequence.latest_call);
    while let Some(step_index) = queue.pop_front() {
        let next_distance = distances[step_index].saturating_add(1);
        for (arg_index, arg) in sequence.steps[step_index].args.iter().enumerate() {
            if step_index == sequence.latest_call && arg_index == mutated_param_index {
                continue;
            }
            let ValueSource::Step(source_index) = &arg.source else {
                continue;
            };
            if distances[*source_index] != u32::MAX && distances[*source_index] != 0 {
                continue;
            }
            distances[*source_index] = next_distance;
            queue.push_back(*source_index);
        }
    }
    distances
}

fn shift_steps(steps: &mut [CallStep], step_offset: usize, param_offset: usize) {
    for step in steps {
        for arg in &mut step.args {
            shift_arg_refs(arg, step_offset, param_offset);
        }
    }
}

fn shift_arg_refs(arg: &mut ArgPlan, step_offset: usize, param_offset: usize) {
    match &mut arg.source {
        ValueSource::Step(index) => {
            *index += step_offset;
        }
        ValueSource::Param(index) => {
            *index += param_offset;
        }
    }
}

fn shift_step_refs_after(step: &mut CallStep, insertion_after: usize, delta: usize) {
    for arg in &mut step.args {
        if let ValueSource::Step(index) = &mut arg.source
            && *index > insertion_after
        {
            *index += delta;
        }
    }
}

fn shift_step_refs_at_or_after(step: &mut CallStep, insertion_at: usize, delta: usize) {
    for arg in &mut step.args {
        if let ValueSource::Step(index) = &mut arg.source
            && *index >= insertion_at
        {
            *index += delta;
        }
    }
}

fn is_unit_ty(ty: Ty<'_>) -> bool {
    matches!(ty.kind(), TyKind::Tuple(list) if list.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        ArgPlan, BorrowMode, CallStep, SequencePlan, ValueSource, distances_to_target_for_sequence,
        insertion_param_offset,
    };

    #[test]
    fn mutator_distances_follow_mutated_object_and_extra_constructors() {
        let sequence = SequencePlan {
            steps: vec![
                CallStep {
                    api_index: 0,
                    args: Vec::new(),
                },
                CallStep {
                    api_index: 1,
                    args: vec![ArgPlan {
                        source: ValueSource::Param(0),
                        borrow: BorrowMode::Move,
                    }],
                },
                CallStep {
                    api_index: 2,
                    args: vec![
                        ArgPlan {
                            source: ValueSource::Step(0),
                            borrow: BorrowMode::Mutable,
                        },
                        ArgPlan {
                            source: ValueSource::Step(1),
                            borrow: BorrowMode::Move,
                        },
                    ],
                },
                CallStep {
                    api_index: 3,
                    args: vec![ArgPlan {
                        source: ValueSource::Step(0),
                        borrow: BorrowMode::Mutable,
                    }],
                },
            ],
            target_api: 3,
            unsafe_wrapper: "target".to_owned(),
            target_step: 3,
            predecessor: Some(0),
            successor: Vec::new(),
            latest_call: 2,
            mutated_param: Some(0),
            ranking_value: 0.0,
        };

        assert_eq!(
            distances_to_target_for_sequence(&sequence),
            vec![1, 2, 1, 0]
        );
    }

    #[test]
    fn insertion_param_offset_accounts_for_suffix_symbolic_params() {
        let sequence = SequencePlan {
            steps: vec![
                CallStep {
                    api_index: 0,
                    args: Vec::new(),
                },
                CallStep {
                    api_index: 1,
                    args: vec![ArgPlan {
                        source: ValueSource::Param(0),
                        borrow: BorrowMode::Move,
                    }],
                },
                CallStep {
                    api_index: 2,
                    args: vec![ArgPlan {
                        source: ValueSource::Param(1),
                        borrow: BorrowMode::Shared,
                    }],
                },
            ],
            target_api: 2,
            unsafe_wrapper: "target".to_owned(),
            target_step: 2,
            predecessor: None,
            successor: Vec::new(),
            latest_call: 2,
            mutated_param: None,
            ranking_value: 0.0,
        };

        assert_eq!(insertion_param_offset(&sequence), 2);
    }
}
