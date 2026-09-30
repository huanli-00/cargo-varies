// SPDX-License-Identifier: MPL-2.0
// Derived from RAPx; see NOTICE.
// Minimal source-level reuse of RAPx's API dependency graph model. This keeps
// cargo-varies lightweight while preserving the graph contract the synthesizer
// consumes.
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::{DefId, LOCAL_CRATE, LocalDefId, LocalModDefId};
use rustc_hir::intravisit::{FnKind, Visitor};
use rustc_middle::ty::{self, Ty, TyCtxt, TyKind};
use rustc_span::Span;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::Instant;

#[derive(Debug, Clone, Copy, Eq, PartialEq, PartialOrd, Default)]
/// Controls which APIs and generic forms are recorded into the dependency
/// graph.
pub struct Config {
    pub pub_only: bool,
    pub resolve_generic: bool,
    pub ignore_const_generic: bool,
}

#[derive(Clone, Copy, Eq, PartialEq, Debug, Hash)]
/// Describes graph-level type transforms that connect produced values to
/// borrowed forms.
pub enum TransformKind {
    Ref(ty::Mutability),
}

impl TransformKind {
    pub fn all() -> &'static [TransformKind] {
        static ALL: [TransformKind; 2] = [
            TransformKind::Ref(ty::Mutability::Not),
            TransformKind::Ref(ty::Mutability::Mut),
        ];
        &ALL
    }
}

#[derive(Clone, Copy, Eq, Debug)]
/// Hash- and equality-stable wrapper around `Ty` for graph indexing.
pub struct TyWrapper<'tcx> {
    ty: Ty<'tcx>,
}

impl<'tcx> TyWrapper<'tcx> {
    pub fn ty(self) -> Ty<'tcx> {
        self.ty
    }

    pub fn transform(self, kind: TransformKind, tcx: TyCtxt<'tcx>) -> Self {
        match kind {
            TransformKind::Ref(mutability) => match mutability {
                ty::Mutability::Not => {
                    Ty::new_ref(tcx, tcx.lifetimes.re_erased, self.ty, mutability).into()
                }
                ty::Mutability::Mut => {
                    Ty::new_ref(tcx, tcx.lifetimes.re_erased, self.ty, mutability).into()
                }
            },
        }
    }
}

impl<'tcx> From<Ty<'tcx>> for TyWrapper<'tcx> {
    fn from(value: Ty<'tcx>) -> Self {
        Self { ty: value }
    }
}

impl PartialEq for TyWrapper<'_> {
    fn eq(&self, other: &Self) -> bool {
        eq_ty(self.ty, other.ty)
    }
}

impl Hash for TyWrapper<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_ty(self.ty, state, &mut 0);
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
/// Node kind in the API dependency graph: either a callable API or a value
/// type.
pub enum DepNode<'tcx> {
    Api(DefId, ty::GenericArgsRef<'tcx>),
    Ty(TyWrapper<'tcx>),
}

impl<'tcx> DepNode<'tcx> {
    pub fn api(id: DefId, args: ty::GenericArgsRef<'tcx>) -> Self {
        Self::Api(id, args)
    }

    pub fn ty(ty: Ty<'tcx>) -> Self {
        Self::Ty(ty.into())
    }

    pub fn expect_api(self) -> (DefId, ty::GenericArgsRef<'tcx>) {
        match self {
            Self::Api(def_id, args) => (def_id, args),
            _ => panic!("expected api node"),
        }
    }

    pub fn expect_ty(self) -> TyWrapper<'tcx> {
        match self {
            Self::Ty(ty) => ty,
            _ => panic!("expected ty node"),
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
/// Edge kind in the API dependency graph.
pub enum DepEdge {
    Arg(usize),
    Ret,
    Transform(TransformKind),
}

impl DepEdge {
    pub fn arg(no: usize) -> Self {
        Self::Arg(no)
    }

    pub fn ret() -> Self {
        Self::Ret
    }

    pub fn transform(kind: TransformKind) -> Self {
        Self::Transform(kind)
    }
}

#[derive(Clone)]
/// Lightweight API dependency graph reused from RAPx-style analysis and used
/// as the synthesizer's input.
pub struct ApiDependencyGraph<'tcx> {
    nodes: Vec<DepNode<'tcx>>,
    outgoing: Vec<Vec<(usize, DepEdge)>>,
    incoming: Vec<Vec<(usize, DepEdge)>>,
    node_indices: HashMap<DepNode<'tcx>, usize>,
    ty_nodes: Vec<usize>,
    api_nodes: Vec<usize>,
    all_apis: HashSet<DefId>,
    public_paths: HashMap<DefId, String>,
    tcx: TyCtxt<'tcx>,
}

impl<'tcx> ApiDependencyGraph<'tcx> {
    pub fn new(tcx: TyCtxt<'tcx>) -> Self {
        Self {
            nodes: Vec::new(),
            outgoing: Vec::new(),
            incoming: Vec::new(),
            node_indices: HashMap::new(),
            ty_nodes: Vec::new(),
            api_nodes: Vec::new(),
            all_apis: HashSet::new(),
            public_paths: HashMap::new(),
            tcx,
        }
    }

    pub fn build(&mut self, config: Config) {
        let tcx = self.tcx;
        let build_start = Instant::now();
        let mut visitor = FnVisitor::new(self, config, tcx);
        tcx.hir_visit_all_item_likes_in_crate(&mut visitor);
        for trait_item_id in tcx.hir_crate_items(()).trait_items() {
            let trait_item = tcx.hir_trait_item(trait_item_id);
            if matches!(
                trait_item.kind,
                rustc_hir::TraitItemKind::Fn(_, rustc_hir::TraitFn::Provided(_))
            ) {
                visitor.maybe_record_fn(trait_item.owner_id.to_def_id());
            }
        }
        self.all_apis = visitor.apis();
        log::info!(
            target: "varies::rapx_graph",
            "api graph collected {} callable defs and {} concrete api instances in {:.2?}",
            self.all_apis.len(),
            self.num_api(),
            build_start.elapsed()
        );
        if config.resolve_generic {
            let resolve_start = Instant::now();
            crate::rapx_resolve::resolve_generic_apis(self);
            log::info!(
                target: "varies::rapx_graph",
                "api graph generic resolution completed in {:.2?}; api instances={}",
                resolve_start.elapsed(),
                self.num_api()
            );
        }
        let transform_start = Instant::now();
        self.update_transform_edges();
        log::info!(
            target: "varies::rapx_graph",
            "api graph transform edges completed in {:.2?}",
            transform_start.elapsed()
        );
    }

    pub fn num_api(&self) -> usize {
        self.api_nodes.len()
    }

    pub fn api_at(&self, idx: usize) -> (DefId, ty::GenericArgsRef<'tcx>) {
        self.nodes[self.api_nodes[idx]].expect_api()
    }

    pub fn get_index(&self, node: DepNode<'tcx>) -> Option<usize> {
        self.node_indices.get(&node).copied()
    }

    pub fn node_at(&self, index: usize) -> DepNode<'tcx> {
        self.nodes[index]
    }

    pub fn incoming_edges(&self, index: usize) -> &[(usize, DepEdge)] {
        &self.incoming[index]
    }

    pub fn outgoing_edges(&self, index: usize) -> &[(usize, DepEdge)] {
        &self.outgoing[index]
    }

    pub fn public_path(&self, def_id: DefId) -> Option<&str> {
        self.public_paths.get(&def_id).map(String::as_str)
    }

    pub(crate) fn tcx(&self) -> TyCtxt<'tcx> {
        self.tcx
    }

    pub(crate) fn all_apis(&self) -> &HashSet<DefId> {
        &self.all_apis
    }

    pub fn record_public_path(&mut self, def_id: DefId, path: String) {
        match self.public_paths.get(&def_id) {
            Some(existing) if existing.len() <= path.len() => {}
            _ => {
                self.public_paths.insert(def_id, path);
            }
        }
    }

    pub fn add_api(&mut self, fn_did: DefId, args: &[ty::GenericArg<'tcx>]) -> bool {
        let args = self.tcx.mk_args(args);
        let api_node = DepNode::api(fn_did, args);
        if self.node_indices.contains_key(&api_node) {
            return false;
        }

        let api_index = self.get_or_create_index(api_node);
        let fn_sig = fn_sig_with_generic_args(fn_did, args, self.tcx);

        for (slot, input_ty) in fn_sig.inputs().iter().enumerate() {
            let input_index = self.get_or_create_index(DepNode::ty(*input_ty));
            self.add_edge(input_index, api_index, DepEdge::arg(slot));
        }

        let output_index = self.get_or_create_index(DepNode::ty(fn_sig.output()));
        self.add_edge(api_index, output_index, DepEdge::ret());
        true
    }

    fn get_or_create_index(&mut self, node: DepNode<'tcx>) -> usize {
        if let Some(index) = self.node_indices.get(&node) {
            return *index;
        }
        let index = self.nodes.len();
        self.nodes.push(node);
        self.outgoing.push(Vec::new());
        self.incoming.push(Vec::new());
        self.node_indices.insert(node, index);
        match node {
            DepNode::Api(..) => self.api_nodes.push(index),
            DepNode::Ty(..) => self.ty_nodes.push(index),
        }
        index
    }

    fn add_edge(&mut self, src: usize, dst: usize, edge: DepEdge) {
        self.outgoing[src].push((dst, edge));
        self.incoming[dst].push((src, edge));
    }

    fn add_edge_once(&mut self, src: usize, dst: usize, edge: DepEdge) {
        if self.outgoing[src]
            .iter()
            .any(|(next, existing)| *next == dst && *existing == edge)
        {
            return;
        }
        self.add_edge(src, dst, edge);
    }

    fn update_transform_edges(&mut self) {
        let ty_nodes = self.ty_nodes.clone();
        for index in ty_nodes {
            let current_ty = self.nodes[index].expect_ty();
            self.add_possible_transform::<3>(current_ty, 0);
        }
    }

    fn add_possible_transform<const MAX_DEPTH: usize>(
        &mut self,
        current_ty: TyWrapper<'tcx>,
        depth: usize,
    ) -> Option<usize> {
        if depth > 0
            && let Some(index) = self.get_index(DepNode::Ty(current_ty))
        {
            return Some(index);
        }
        if depth >= MAX_DEPTH {
            return None;
        }

        let mut ret = None;
        for kind in TransformKind::all() {
            let next_ty = current_ty.transform(*kind, self.tcx);
            if let Some(next_index) = self.add_possible_transform::<MAX_DEPTH>(next_ty, depth + 1) {
                let current_index = self.get_or_create_index(DepNode::Ty(current_ty));
                self.add_edge_once(current_index, next_index, DepEdge::transform(*kind));
                ret = Some(current_index);
            }
        }
        ret
    }
}

struct FnVisitor<'tcx, 'a> {
    tcx: TyCtxt<'tcx>,
    config: Config,
    graph: &'a mut ApiDependencyGraph<'tcx>,
    apis: HashSet<DefId>,
}

impl<'tcx, 'a> FnVisitor<'tcx, 'a> {
    fn new(graph: &'a mut ApiDependencyGraph<'tcx>, config: Config, tcx: TyCtxt<'tcx>) -> Self {
        Self {
            tcx,
            config,
            graph,
            apis: HashSet::new(),
        }
    }

    fn apis(self) -> HashSet<DefId> {
        self.apis
    }

    fn maybe_record_fn(&mut self, fn_did: DefId) {
        let Some(local_def_id) = fn_did.as_local() else {
            return;
        };
        if self.tcx.hir_maybe_body_owned_by(local_def_id).is_none() {
            return;
        }

        let generics = self.tcx.generics_of(fn_did);

        if self.config.pub_only && !is_def_id_public(fn_did, self.tcx) {
            return;
        }
        if self.config.ignore_const_generic && has_const_generics(generics, self.tcx) {
            return;
        }

        let is_generic = generics.requires_monomorphization(self.tcx);
        if !self.config.resolve_generic && is_generic {
            return;
        }

        if !is_generic {
            let args = ty::GenericArgs::identity_for_item(self.tcx, fn_did);
            self.graph.add_api(fn_did, args);
        }
        self.apis.insert(fn_did);
    }
}

impl<'tcx, 'a> Visitor<'tcx> for FnVisitor<'tcx, 'a> {
    fn visit_item(&mut self, item: &'tcx rustc_hir::Item<'tcx>) -> Self::Result {
        let item_def_id = item.owner_id.to_def_id();
        if let rustc_hir::ItemKind::Use(path, kind) = &item.kind
            && (!self.config.pub_only || is_def_id_public(item_def_id, self.tcx))
        {
            let target_res = path.res.type_ns.or(path.res.value_ns).or(path.res.macro_ns);
            match kind {
                rustc_hir::UseKind::Single(alias) => {
                    if let Some(Res::Def(_, target_def_id)) = target_res {
                        self.graph.record_public_path(
                            target_def_id,
                            public_alias_path(self.tcx, item_def_id, alias.as_str()),
                        );
                    }
                }
                rustc_hir::UseKind::Glob => {
                    if let Some(Res::Def(DefKind::Mod, target_def_id)) = target_res {
                        record_public_glob_paths(self.graph, self.tcx, item_def_id, target_def_id);
                    }
                }
                _ => {}
            }
        }
        rustc_hir::intravisit::walk_item(self, item);
    }

    fn visit_fn<'v>(
        &mut self,
        _fk: FnKind<'v>,
        _fd: &'v rustc_hir::FnDecl<'v>,
        _b: rustc_hir::BodyId,
        _span: Span,
        id: LocalDefId,
    ) -> Self::Result {
        self.maybe_record_fn(id.to_def_id());
    }

    fn visit_trait_item(&mut self, item: &'tcx rustc_hir::TraitItem<'tcx>) -> Self::Result {
        if matches!(
            item.kind,
            rustc_hir::TraitItemKind::Fn(_, rustc_hir::TraitFn::Provided(_))
        ) {
            self.maybe_record_fn(item.owner_id.to_def_id());
        }
        rustc_hir::intravisit::walk_trait_item(self, item);
    }

    fn visit_impl_item(&mut self, item: &'tcx rustc_hir::ImplItem<'tcx>) -> Self::Result {
        if matches!(item.kind, rustc_hir::ImplItemKind::Fn(..)) {
            self.maybe_record_fn(item.owner_id.to_def_id());
        }
        rustc_hir::intravisit::walk_impl_item(self, item);
    }
}

fn is_def_id_public(fn_def_id: DefId, tcx: TyCtxt<'_>) -> bool {
    if !fn_def_id.is_local() {
        return true;
    }

    if let Some(assoc) = tcx.opt_associated_item(fn_def_id) {
        return match assoc.container {
            ty::AssocContainer::Trait => is_def_id_public(assoc.container_id(tcx), tcx),
            ty::AssocContainer::InherentImpl => {
                let impl_def_id = assoc.container_id(tcx);
                tcx.visibility(fn_def_id).is_public()
                    && is_public_self_ty(tcx.type_of(impl_def_id).instantiate_identity(), tcx)
            }
            ty::AssocContainer::TraitImpl(trait_item_id) => {
                let impl_def_id = assoc.container_id(tcx);
                is_local_public_self_ty(tcx.type_of(impl_def_id).instantiate_identity(), tcx)
                    && trait_item_id.is_ok_and(|trait_item_id| is_def_id_public(trait_item_id, tcx))
            }
        };
    }

    tcx.effective_visibilities(())
        .is_exported(fn_def_id.expect_local())
}

fn record_public_glob_paths(
    graph: &mut ApiDependencyGraph<'_>,
    tcx: TyCtxt<'_>,
    use_item_def_id: DefId,
    module_def_id: DefId,
) {
    let base_path = public_use_parent_path(tcx, use_item_def_id);
    let mut module_stack = HashSet::new();
    record_public_module_exports(graph, tcx, module_def_id, &base_path, &mut module_stack);
}

fn record_public_module_exports(
    graph: &mut ApiDependencyGraph<'_>,
    tcx: TyCtxt<'_>,
    module_def_id: DefId,
    base_path: &str,
    module_stack: &mut HashSet<DefId>,
) {
    if !module_stack.insert(module_def_id) {
        return;
    }

    if let Some(local_def_id) = module_def_id.as_local() {
        let local_mod_def_id = LocalModDefId::new_unchecked(local_def_id);
        for item_id in tcx.hir_module_free_items(local_mod_def_id) {
            let item = tcx.hir_item(item_id);
            let item_def_id = item.owner_id.to_def_id();
            if !tcx.visibility(item_def_id).is_public() {
                continue;
            }

            match &item.kind {
                rustc_hir::ItemKind::Use(path, kind) => {
                    let target_res = path.res.type_ns.or(path.res.value_ns).or(path.res.macro_ns);
                    match kind {
                        rustc_hir::UseKind::Single(alias) => {
                            let Some(Res::Def(_, target_def_id)) = target_res else {
                                continue;
                            };
                            record_public_export_path(
                                graph,
                                target_def_id,
                                base_path,
                                alias.as_str(),
                            );
                        }
                        rustc_hir::UseKind::Glob => {
                            let Some(Res::Def(DefKind::Mod, target_def_id)) = target_res else {
                                continue;
                            };
                            record_public_module_exports(
                                graph,
                                tcx,
                                target_def_id,
                                base_path,
                                module_stack,
                            );
                        }
                        _ => {}
                    }
                }
                _ => {
                    let export_name = tcx.item_name(item_def_id);
                    record_public_export_path(graph, item_def_id, base_path, export_name.as_str());
                }
            }
        }
    } else {
        for child in tcx.module_children(module_def_id).iter() {
            if !child.vis.is_public() {
                continue;
            }
            let Some(child_def_id) = child.res.opt_def_id() else {
                continue;
            };
            record_public_export_path(graph, child_def_id, base_path, child.ident.name.as_str());
        }
    }

    module_stack.remove(&module_def_id);
}

fn record_public_export_path(
    graph: &mut ApiDependencyGraph<'_>,
    export_def_id: DefId,
    base_path: &str,
    export_name: &str,
) {
    let public_path = if base_path.is_empty() {
        export_name.to_owned()
    } else {
        format!("{base_path}::{export_name}")
    };
    graph.record_public_path(export_def_id, public_path);
}

fn public_use_parent_path(tcx: TyCtxt<'_>, item_def_id: DefId) -> String {
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let Some(parent_def_id) = tcx.opt_parent(item_def_id) else {
        return String::new();
    };

    let mut parent_path = tcx.def_path_str(parent_def_id);
    if parent_path == crate_name {
        return String::new();
    }
    let crate_prefix = format!("{crate_name}::");
    if let Some(stripped) = parent_path.strip_prefix(&crate_prefix) {
        parent_path = stripped.to_owned();
    }
    parent_path
}

fn is_public_self_ty(self_ty: Ty<'_>, tcx: TyCtxt<'_>) -> bool {
    match self_ty.peel_refs().kind() {
        TyKind::Adt(def, _) => !def.did().is_local() || is_def_id_public(def.did(), tcx),
        TyKind::Slice(_)
        | TyKind::Str
        | TyKind::Bool
        | TyKind::Char
        | TyKind::Int(_)
        | TyKind::Uint(_)
        | TyKind::Float(_)
        | TyKind::Array(..) => true,
        _ => false,
    }
}

fn is_local_public_self_ty(self_ty: Ty<'_>, tcx: TyCtxt<'_>) -> bool {
    match self_ty.peel_refs().kind() {
        TyKind::Adt(def, _) => def.did().is_local() && is_def_id_public(def.did(), tcx),
        _ => false,
    }
}

fn has_const_generics(generics: &ty::Generics, tcx: TyCtxt<'_>) -> bool {
    if generics
        .own_params
        .iter()
        .any(|param| matches!(param.kind, ty::GenericParamDefKind::Const { .. }))
    {
        return true;
    }
    if let Some(parent_def_id) = generics.parent {
        has_const_generics(tcx.generics_of(parent_def_id), tcx)
    } else {
        false
    }
}

fn public_alias_path(tcx: TyCtxt<'_>, item_def_id: DefId, ident: &str) -> String {
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let Some(parent_def_id) = tcx.opt_parent(item_def_id) else {
        return ident.to_owned();
    };

    let mut parent_path = tcx.def_path_str(parent_def_id);
    if parent_path == crate_name {
        return ident.to_owned();
    }
    let crate_prefix = format!("{crate_name}::");
    if let Some(stripped) = parent_path.strip_prefix(&crate_prefix) {
        parent_path = stripped.to_owned();
    }

    if parent_path.is_empty() {
        ident.to_owned()
    } else {
        format!("{parent_path}::{ident}")
    }
}

/// Return the instantiated function signature for a definition under the
/// provided generic arguments.
pub fn fn_sig_with_generic_args<'tcx>(
    fn_did: DefId,
    args: ty::GenericArgsRef<'tcx>,
    tcx: TyCtxt<'tcx>,
) -> ty::FnSig<'tcx> {
    let binder = tcx.fn_sig(fn_did).instantiate(tcx, args);
    tcx.liberate_late_bound_regions(fn_did, binder)
}

fn eq_ty<'tcx>(lhs: Ty<'tcx>, rhs: Ty<'tcx>) -> bool {
    match (lhs.kind(), rhs.kind()) {
        (TyKind::Adt(def1, args1), TyKind::Adt(def2, args2)) => {
            if def1.did() != def2.did() {
                return false;
            }
            for (arg1, arg2) in args1.iter().zip(args2.iter()) {
                match (arg1.kind(), arg2.kind()) {
                    (ty::GenericArgKind::Lifetime(_), ty::GenericArgKind::Lifetime(_)) => {}
                    (ty::GenericArgKind::Type(ty1), ty::GenericArgKind::Type(ty2)) => {
                        if !eq_ty(ty1, ty2) {
                            return false;
                        }
                    }
                    (ty::GenericArgKind::Const(c1), ty::GenericArgKind::Const(c2)) => {
                        if c1 != c2 {
                            return false;
                        }
                    }
                    _ => return false,
                }
            }
            true
        }
        (TyKind::RawPtr(inner1, mut1), TyKind::RawPtr(inner2, mut2))
        | (TyKind::Ref(_, inner1, mut1), TyKind::Ref(_, inner2, mut2)) => {
            mut1 == mut2 && eq_ty(*inner1, *inner2)
        }
        (TyKind::Array(inner1, len1), TyKind::Array(inner2, len2)) => {
            len1 == len2 && eq_ty(*inner1, *inner2)
        }
        (TyKind::Slice(inner1), TyKind::Slice(inner2)) => eq_ty(*inner1, *inner2),
        (TyKind::Tuple(items1), TyKind::Tuple(items2)) => {
            items1.len() == items2.len()
                && items1
                    .iter()
                    .zip(items2.iter())
                    .all(|(ty1, ty2)| eq_ty(ty1, ty2))
        }
        _ => lhs == rhs,
    }
}

fn hash_ty<'tcx, H: Hasher>(ty: Ty<'tcx>, state: &mut H, no: &mut usize) {
    std::mem::discriminant(ty.kind()).hash(state);
    match ty.kind() {
        TyKind::Adt(def, args) => {
            def.did().hash(state);
            for arg in args.iter() {
                match arg.kind() {
                    ty::GenericArgKind::Lifetime(_) => {
                        *no += 1;
                        no.hash(state);
                    }
                    ty::GenericArgKind::Type(ty) => hash_ty(ty, state, no),
                    ty::GenericArgKind::Const(ct) => ct.hash(state),
                }
            }
        }
        TyKind::RawPtr(inner, mutability) | TyKind::Ref(_, inner, mutability) => {
            mutability.hash(state);
            if matches!(ty.kind(), TyKind::Ref(..)) {
                *no += 1;
                no.hash(state);
            }
            hash_ty(*inner, state, no);
        }
        TyKind::Array(inner, len) => {
            len.hash(state);
            hash_ty(*inner, state, no);
        }
        TyKind::Slice(inner) => hash_ty(*inner, state, no),
        TyKind::Tuple(items) => {
            for item in items.iter() {
                hash_ty(item, state, no);
            }
        }
        _ => ty.hash(state),
    }
}
