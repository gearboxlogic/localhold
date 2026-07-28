use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use serde::Serialize;
use syn::visit::{self, Visit};
use syn::{
    Arm, Attribute, BareFnArg, BareVariadic, Block, Expr, Field, FieldPat, FieldValue, File, FnArg, ForeignItem, ForeignItemFn, ForeignItemStatic, GenericParam, Generics,
    ImplItem, ImplItemConst, ImplItemFn, ImplItemType, Item, ItemConst, ItemEnum, ItemExternCrate, ItemFn, ItemImpl, ItemMacro, ItemMod, ItemStatic, ItemStruct, ItemTrait,
    ItemType, ItemUnion, ItemUse, Local, Pat, Path as SynPath, Stmt, StmtMacro, TraitItem, TraitItemConst, TraitItemFn, TraitItemType, Variadic, Variant, Visibility,
};

use crate::scan::{RESERVED_LOCAL_MACROS, reviewed_attribute_expansion, reviewed_macro_expansion, syntax_fingerprint};

use super::{
    ProductionCfgContext, expr_attributes, fn_arg_attributes, foreign_item_attributes, generic_param_attributes, impl_item_attributes, item_attributes, normalized_ident,
    pat_attributes, production_cfg_context, trait_item_attributes, visibility_is_exposed,
};

mod concrete;
mod declarations;
mod exposures;
mod macro_definitions;
mod production;
mod reexports;
mod resolution;
mod stringify;
mod tokens;
mod visibility;
use concrete::{BindingSiteContext, ConcreteStoreInventory, SignatureSiteContext, context_fingerprint, is_concrete_store_name, production_generics, without_documentation};
pub use concrete::{ConcreteStoreCounts, ConcreteStoreSignatureSite, ConcreteStoreSignatureSites, ConcreteStoreSites};
pub use declarations::TypeDeclarationEvidence;
pub(in crate::structure) use declarations::TypeDeclarationKind;
use declarations::{TypeDeclarationContext, type_declaration_evidence};
use macro_definitions::{contains_production_concrete_store, reviewed_macro_transcribers};
#[cfg(test)]
use production::production_impl_tokens;
use production::{production_foreign_item_tokens, production_impl_item_tokens, production_item_tokens, production_stmt_tokens, production_trait_item_tokens};
use reexports::{
    PendingPublicReexport, PublicTypeAliasContext, UseResolution, public_type_alias, resolve_binding_aliases, resolve_impl_signature_aliases, resolve_public_reexport_aliases,
    type_alias_resolution,
};
pub(in crate::structure) use resolution::source_module;
use resolution::{StringScan, UsePath, flatten_use_tree, resolve_path, restricted_attribute_identifier, restricted_token_identifier};
use stringify::{
    BlockBuiltinStringifyAlias, MacroShadow, ModuleStringifyImports, binding_is_fully_builtin, collect_module_stringify_imports, is_explicit_builtin_stringify,
    stringify_imports_in_block,
};
use tokens::resolving_tokens;
pub use visibility::VisibilityCounts;
use visibility::VisibilityMacroAudit;

#[derive(Clone, Copy)]
enum FieldExposure {
    Struct(bool),
    Enum(bool),
    Union(bool),
}

#[derive(Default)]
pub struct ProductionSyntaxFacts {
    pub module: Vec<String>,
    pub internal_imports: Vec<String>,
    pub public_reexports: Vec<PublicReexportEvidence>,
    pub type_declarations: Vec<TypeDeclarationEvidence>,
    pub concrete_stores: ConcreteStoreCounts,
    pub public_concrete_store_structs: ConcreteStoreSites,
    pub concrete_store_sites: ConcreteStoreSites,
    pub generic_default_concrete_store_sites: ConcreteStoreSites,
    pub signature_concrete_store_sites: ConcreteStoreSignatureSites,
    pub binding_concrete_store_sites: ConcreteStoreSites,
    pub visibilities: VisibilityCounts,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PublicReexportEvidence {
    pub exported_path: Vec<String>,
    pub target_path: Vec<String>,
    pub fingerprint: String,
    #[serde(skip)]
    pub(in crate::structure) cfg: ProductionCfgContext,
    #[serde(skip)]
    pub(in crate::structure) direct_exposure_cfg: Option<ProductionCfgContext>,
    #[serde(skip)]
    pub(in crate::structure) required_trait_path: Option<Vec<String>>,
}

#[derive(Clone, Copy)]
pub struct ProductionSyntaxOptions {
    pub collect_internal_imports: bool,
    pub rust_2015_absolute_paths: bool,
    pub require_reviewed_expansions: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(in crate::structure) enum ProductionSourceRevision {
    Current,
    Historical,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::structure) struct ProductionAncestorPath {
    pub(in crate::structure) cfg: ProductionCfgContext,
    pub(in crate::structure) ancestors: Vec<String>,
}

pub(in crate::structure) struct ProductionSyntaxContext {
    pub(in crate::structure) cfg: ProductionCfgContext,
    pub(in crate::structure) declaration_ancestors: Vec<ProductionAncestorPath>,
    pub(in crate::structure) module_exposure_cfg: Option<ProductionCfgContext>,
    pub(in crate::structure) source_revision: ProductionSourceRevision,
}

impl Default for ProductionSyntaxContext {
    fn default() -> Self {
        Self {
            cfg: ProductionCfgContext::default(),
            declaration_ancestors: Vec::new(),
            module_exposure_cfg: Some(ProductionCfgContext::default()),
            source_revision: ProductionSourceRevision::Current,
        }
    }
}

#[cfg(test)]
pub fn production_syntax_facts(file: &syn::File, source_path: &str, crate_root: Option<&str>, options: ProductionSyntaxOptions) -> Result<ProductionSyntaxFacts> {
    production_syntax_facts_with_context(file, source_path, crate_root, options, ProductionSyntaxContext::default())
}

pub(in crate::structure) fn production_syntax_facts_with_context(
    file: &syn::File,
    source_path: &str,
    crate_root: Option<&str>,
    options: ProductionSyntaxOptions,
    initial_context: ProductionSyntaxContext,
) -> Result<ProductionSyntaxFacts> {
    let module = if source_path.starts_with("src/") || crate_root.is_some() {
        source_module(source_path, crate_root)?
    } else {
        Vec::new()
    };
    let mut collector = collect_production_syntax(file, module, options, initial_context, Vec::new())?;
    collector.imports.sort();
    collector.imports.dedup();
    collector.type_declarations.sort();
    collector.type_declarations.dedup();
    let mut public_reexports = resolve_public_reexport_aliases(collector.public_reexports, &collector.use_resolutions, &collector.type_declarations);
    public_reexports.sort();
    public_reexports.dedup();
    resolve_impl_signature_aliases(&mut collector.concrete_stores.signature_sites, &collector.use_resolutions, &collector.type_declarations);
    resolve_binding_aliases(&mut collector.concrete_stores.binding_sites, &collector.use_resolutions, &collector.type_declarations);
    collector.concrete_stores.finish();
    let binding_concrete_store_sites = collector.concrete_stores.binding_fingerprints();
    collector.visibility_macros.finish()?;
    Ok(ProductionSyntaxFacts {
        module: collector.module,
        internal_imports: collector.imports,
        public_reexports,
        type_declarations: collector.type_declarations,
        concrete_stores: collector.concrete_stores.counts,
        public_concrete_store_structs: collector.concrete_stores.public_struct_declarations,
        concrete_store_sites: collector.concrete_stores.sites,
        generic_default_concrete_store_sites: collector.concrete_stores.generic_default_sites,
        signature_concrete_store_sites: collector.concrete_stores.signature_sites,
        binding_concrete_store_sites,
        visibilities: collector.visibilities,
    })
}

fn collect_production_syntax(
    file: &syn::File,
    module: Vec<String>,
    options: ProductionSyntaxOptions,
    initial_context: ProductionSyntaxContext,
    declaration_ancestors: Vec<String>,
) -> Result<ProductionSyntaxCollector> {
    let module_stringify_imports = collect_module_stringify_imports(file, &module, &initial_context.cfg)?;
    let mut collector = ProductionSyntaxCollector {
        module,
        module_stringify_imports,
        builtin_stringify_block_aliases: Vec::new(),
        macro_import_shadow_scopes: Vec::new(),
        imports: Vec::new(),
        use_resolutions: Vec::new(),
        public_reexports: Vec::new(),
        type_declarations: Vec::new(),
        concrete_stores: ConcreteStoreInventory::default(),
        visibilities: VisibilityCounts::default(),
        visibility_macros: VisibilityMacroAudit::default(),
        site_context: None,
        block_depth: 0,
        block_type_scopes: Vec::new(),
        macro_shadow_scopes: vec![BTreeSet::new()],
        generic_default_depth: 0,
        impl_signature_headers: Vec::new(),
        impl_trait_exposures: Vec::new(),
        impl_trait_paths: Vec::new(),
        trait_exposures: Vec::new(),
        field_exposures: Vec::new(),
        generic_type_scopes: Vec::new(),
        impl_item_paths: Vec::new(),
        inherited_declaration_ancestors: initial_context.declaration_ancestors,
        declaration_ancestors,
        macro_context: MacroContext::Invocation,
        cfg_context: initial_context.cfg,
        module_exposure_cfg: initial_context.module_exposure_cfg,
        error: None,
        rust_2015_absolute_paths: options.rust_2015_absolute_paths,
        collect_internal_imports: options.collect_internal_imports,
        require_reviewed_expansions: options.require_reviewed_expansions,
        source_revision: initial_context.source_revision,
    };
    collector.visit_file(file);
    if let Some(error) = collector.error {
        return Err(error);
    }
    Ok(collector)
}

struct ProductionSyntaxCollector {
    module: Vec<String>,
    module_stringify_imports: ModuleStringifyImports,
    builtin_stringify_block_aliases: Vec<BTreeSet<BlockBuiltinStringifyAlias>>,
    macro_import_shadow_scopes: Vec<BTreeSet<MacroShadow>>,
    imports: Vec<String>,
    use_resolutions: Vec<UseResolution>,
    public_reexports: Vec<PendingPublicReexport>,
    type_declarations: Vec<TypeDeclarationEvidence>,
    concrete_stores: ConcreteStoreInventory,
    visibilities: VisibilityCounts,
    visibility_macros: VisibilityMacroAudit,
    site_context: Option<String>,
    block_depth: usize,
    block_type_scopes: Vec<BlockTypeBindings>,
    macro_shadow_scopes: Vec<BTreeSet<MacroShadow>>,
    generic_default_depth: usize,
    impl_signature_headers: Vec<TokenStream>,
    impl_trait_exposures: Vec<bool>,
    impl_trait_paths: Vec<Option<Vec<String>>>,
    trait_exposures: Vec<bool>,
    field_exposures: Vec<FieldExposure>,
    generic_type_scopes: Vec<BTreeSet<String>>,
    impl_item_paths: Vec<Vec<String>>,
    inherited_declaration_ancestors: Vec<ProductionAncestorPath>,
    declaration_ancestors: Vec<String>,
    macro_context: MacroContext,
    cfg_context: ProductionCfgContext,
    module_exposure_cfg: Option<ProductionCfgContext>,
    error: Option<anyhow::Error>,
    rust_2015_absolute_paths: bool,
    collect_internal_imports: bool,
    require_reviewed_expansions: bool,
    source_revision: ProductionSourceRevision,
}

enum SiteContextEntry {
    Entered(Option<String>),
    Failed,
}

#[derive(Default)]
struct BlockTypeBindings {
    nominal_types: BTreeSet<BlockTypeBinding>,
    ambiguous_roots: BTreeSet<BlockTypeBinding>,
    glob_imports: BTreeSet<ProductionCfgContext>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BlockTypeBinding {
    name: String,
    cfg: ProductionCfgContext,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum MacroContext {
    #[default]
    Invocation,
    Definition,
}

impl ProductionSyntaxCollector {
    fn collect_reviewed_macro_transcribers(&mut self, name: &str, tokens: &TokenStream) -> Result<()> {
        for transcriber in reviewed_macro_transcribers(tokens)? {
            let mut declaration_ancestors = self.declaration_ancestors.clone();
            declaration_ancestors.push(format!("macro-transcriber:{name}:matcher:{}", transcriber.matcher_fingerprint));
            let generated = collect_production_syntax(
                &transcriber.syntax,
                self.module.clone(),
                ProductionSyntaxOptions {
                    collect_internal_imports: false,
                    rust_2015_absolute_paths: self.rust_2015_absolute_paths,
                    require_reviewed_expansions: false,
                },
                ProductionSyntaxContext {
                    cfg: self.cfg_context.clone(),
                    declaration_ancestors: self.inherited_declaration_ancestors.clone(),
                    module_exposure_cfg: self.module_exposure_cfg.clone(),
                    source_revision: self.source_revision,
                },
                declaration_ancestors,
            )?;
            if generated.concrete_stores.counts.sqlite_store != 0 || generated.concrete_stores.counts.postgres_store != 0 {
                bail!("production macro definitions cannot inject concrete stores into call sites");
            }
            self.use_resolutions.extend(generated.use_resolutions);
            self.public_reexports.extend(generated.public_reexports);
            self.type_declarations.extend(generated.type_declarations);
        }
        Ok(())
    }

    fn collect_use(&mut self, item: &ItemUse) -> Result<()> {
        if item.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(());
        }
        let import_count = self.imports.len();
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for mut path in paths {
            if path.renamed && path.segments.iter().any(|segment| is_concrete_store_name(segment)) {
                bail!("production concrete stores cannot be hidden behind renamed imports");
            }
            if item.leading_colon.is_some() {
                path.segments.insert(0, "crate".to_owned());
            }
            self.collect_path(&path)?;
        }
        if self.imports.len() != import_count && !matches!(item.vis, Visibility::Inherited) {
            bail!("production restricted imports cannot be re-exported");
        }
        Ok(())
    }

    fn record_public_reexport(&mut self, item: &ItemUse) -> Result<()> {
        if !visibility_is_exposed(&item.vis) || item.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(());
        }
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for mut path in paths {
            if path.alias.as_deref() == Some("_") {
                continue;
            }
            if item.leading_colon.is_some() {
                path.segments.insert(0, "crate".to_owned());
            }
            let Some(target_path) = resolve_path(&self.module, &path.segments, self.rust_2015_absolute_paths)? else {
                continue;
            };
            let mut exported_path = self.module.clone();
            exported_path.push(
                path.alias
                    .clone()
                    .or_else(|| target_path.last().cloned())
                    .context("production public re-export has no exported name")?,
            );
            let identity = format!(
                "public-reexport:{}\0alias:{}\0visibility:{}\0cfg:{}\0ancestors:{}",
                target_path.join("::"),
                path.alias.as_deref().unwrap_or_default(),
                syntax_fingerprint(&item.vis),
                self.cfg_context.identity(),
                self.declaration_ancestor_identity()
            );
            self.public_reexports.push(PendingPublicReexport {
                evidence: PublicReexportEvidence {
                    exported_path,
                    target_path,
                    fingerprint: syntax_fingerprint(&identity),
                    cfg: self.cfg_context.clone(),
                    direct_exposure_cfg: self.direct_exposure_cfg(),
                    required_trait_path: None,
                },
                cfg: self.cfg_context.clone(),
                source_path: None,
            });
        }
        Ok(())
    }

    fn record_type_alias_evidence(&mut self, item: &ItemType) -> Result<()> {
        let ancestors = self.declaration_ancestor_identity();
        let context = PublicTypeAliasContext {
            module: &self.module,
            cfg: &self.cfg_context,
            direct_exposure_cfg: self.direct_exposure_cfg(),
            ancestors: &ancestors,
            rust_2015_absolute_paths: self.rust_2015_absolute_paths,
        };
        if let Some(resolution) = type_alias_resolution(item, &context)? {
            self.use_resolutions.push(resolution);
        }
        let alias = public_type_alias(item, context)?;
        self.record_exposed_type_alias_exposures(item, alias.as_ref().map(|alias| alias.evidence.target_path.as_slice()))?;
        if let Some(alias) = alias {
            self.public_reexports.push(alias);
        }
        Ok(())
    }

    fn record_use_resolutions(&mut self, item: &ItemUse) -> Result<()> {
        if item.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(());
        }
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for mut path in paths {
            if path.alias.as_deref() == Some("_") {
                continue;
            }
            if item.leading_colon.is_some() {
                path.segments.insert(0, "crate".to_owned());
            }
            let Some(target_path) = resolve_path(&self.module, &path.segments, self.rust_2015_absolute_paths)? else {
                continue;
            };
            let mut exported_path = self.module.clone();
            exported_path.push(path.alias.clone().or_else(|| target_path.last().cloned()).context("production use has no imported name")?);
            let identity = format!(
                "use-resolution:{}\0alias:{}\0visibility:{}\0cfg:{}\0ancestors:{}",
                target_path.join("::"),
                path.alias.as_deref().unwrap_or_default(),
                syntax_fingerprint(&item.vis),
                self.cfg_context.identity(),
                self.declaration_ancestor_identity()
            );
            self.use_resolutions.push(UseResolution {
                exported_path,
                target_path,
                fingerprint: syntax_fingerprint(&identity),
                cfg: self.cfg_context.clone(),
            });
        }
        Ok(())
    }

    fn collect_path(&mut self, path: &UsePath) -> Result<()> {
        self.collect_segments(&path.segments, path.renamed, self.rust_2015_absolute_paths)
    }

    fn collect_segments(&mut self, segments: &[String], renamed: bool, rust_2015_use_path: bool) -> Result<()> {
        if !self.collect_internal_imports {
            return Ok(());
        }
        let Some(resolved) = resolve_path(&self.module, segments, rust_2015_use_path)? else {
            return Ok(());
        };
        if resolved.is_empty() && renamed {
            bail!("production crate-root import aliases cannot be classified safely for dependency boundaries");
        }
        if resolved.as_slice() == ["*"] {
            bail!("production crate-root glob imports cannot be classified safely for dependency boundaries");
        }
        if matches!(resolved.first().map(String::as_str), Some("server" | "ui")) {
            self.imports.push(format!("crate::{}", resolved.join("::")));
        }
        Ok(())
    }

    fn enter_production_node(&mut self, attributes: Result<&[Attribute]>) -> Option<ProductionCfgContext> {
        if self.error.is_some() {
            return None;
        }
        let active = match attributes.and_then(|attributes| production_cfg_context(attributes, &self.cfg_context)) {
            Ok(Some(active)) => active,
            Ok(None) => return None,
            Err(error) => {
                self.error = Some(error);
                return None;
            }
        };
        Some(std::mem::replace(&mut self.cfg_context, active))
    }

    fn leave_production_node(&mut self, previous: ProductionCfgContext) {
        self.cfg_context = previous;
    }

    fn record_concrete_store(&mut self, ident: &proc_macro2::Ident) {
        let context = self.site_context.as_deref().unwrap_or("unscoped-production-syntax");
        if let Err(error) = self.concrete_stores.record_ident(ident, context) {
            self.error = Some(error);
        }
        if self.generic_default_depth > 0 {
            self.concrete_stores.record_generic_default_ident(ident, context);
        }
    }

    fn reject_concrete_store_alias(&mut self, before: ConcreteStoreCounts) {
        if self.error.is_none() && self.concrete_stores.counts != before {
            self.error = Some(anyhow::anyhow!("production concrete stores cannot be hidden behind type aliases"));
        }
    }

    fn record_concrete_stores_in_tokens(&mut self, tokens: &TokenStream) {
        let context = self.site_context.as_deref().unwrap_or("unscoped-production-syntax");
        if let Err(error) = self.concrete_stores.record_tokens(tokens, context) {
            self.error = Some(error);
        }
        if self.generic_default_depth > 0 {
            self.concrete_stores.record_generic_default_tokens(tokens, context);
        }
    }

    fn record_concrete_stores_in_signature(&mut self, kind: &str, syntax: &impl ToTokens) {
        let tokens = without_documentation(&syntax.to_token_stream());
        self.record_concrete_stores_in_signature_with_identity(kind, &tokens, &tokens);
    }

    fn record_concrete_stores_in_visible_signature(&mut self, kind: &str, visibility: &Visibility, syntax: &impl ToTokens) {
        let tokens = without_documentation(&syntax.to_token_stream());
        let mut identity = visibility.to_token_stream();
        identity.extend(tokens.clone());
        self.record_concrete_stores_in_signature_with_identity(kind, &tokens, &identity);
    }

    fn record_declaration_generics(&mut self, kind: &str, visibility: &Visibility, generics: &Generics) {
        let generics = match production_generics(generics, &self.cfg_context) {
            Ok(generics) => generics,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let mut tokens = generics.to_token_stream();
        if let Some(where_clause) = &generics.where_clause {
            tokens.extend(where_clause.to_token_stream());
        }
        self.record_concrete_stores_in_visible_signature(kind, visibility, &tokens);
    }

    fn production_signature(&self, signature: &syn::Signature) -> Result<syn::Signature> {
        let mut production = signature.clone();
        let mut inputs = Vec::new();
        for input in &signature.inputs {
            if production_cfg_context(fn_arg_attributes(input), &self.cfg_context)?.is_some() {
                inputs.push(input.clone());
            }
        }
        production.inputs = inputs.into_iter().collect();

        production.generics = production_generics(&signature.generics, &self.cfg_context)?;

        if let Some(variadic) = &signature.variadic
            && production_cfg_context(&variadic.attrs, &self.cfg_context)?.is_none()
        {
            production.variadic = None;
        }
        Ok(production)
    }

    fn record_concrete_stores_in_signature_with_identity(&mut self, kind: &str, tokens: &TokenStream, identity: &TokenStream) {
        let context = format!(
            "{kind}:{}\0cfg:{}\0ancestors:{}",
            syntax_fingerprint(identity),
            self.cfg_context.identity(),
            self.declaration_ancestor_identity()
        );
        let item_path = self.signature_item_path();
        let direct_exposure_cfg = self.direct_exposure_cfg();
        let signature = SignatureSiteContext {
            item_path: &item_path,
            cfg: &self.cfg_context,
            impl_self_type: !self.impl_item_paths.is_empty(),
            direct_exposure_cfg: direct_exposure_cfg.as_ref(),
            required_trait_path: self.impl_trait_paths.last().and_then(Option::as_deref),
        };
        self.concrete_stores.record_signature_tokens(tokens, &context, &signature);
    }

    fn record_concrete_stores_in_exposure_signature_with_identity(&mut self, kind: &str, tokens: &TokenStream, identity: &TokenStream) {
        let context = format!(
            "{kind}:{}\0cfg:{}\0ancestors:{}",
            syntax_fingerprint(identity),
            self.cfg_context.identity(),
            self.declaration_ancestor_identity()
        );
        let item_path = self.signature_item_path();
        let direct_exposure_cfg = self.direct_exposure_cfg();
        let signature = SignatureSiteContext {
            item_path: &item_path,
            cfg: &self.cfg_context,
            impl_self_type: !self.impl_item_paths.is_empty(),
            direct_exposure_cfg: direct_exposure_cfg.as_ref(),
            required_trait_path: self.impl_trait_paths.last().and_then(Option::as_deref),
        };
        self.concrete_stores.record_exposure_signature_tokens(tokens, &context, &signature);
    }

    fn record_type_declaration(&mut self, kind: TypeDeclarationKind, syntax_kind: &str, ident: &proc_macro2::Ident, visibility: &Visibility) {
        if self.block_depth > 0 {
            return;
        }
        let ancestors = self.declaration_ancestor_identity();
        self.type_declarations.push(type_declaration_evidence(
            kind,
            syntax_kind,
            ident,
            visibility,
            &TypeDeclarationContext {
                module: &self.module,
                cfg: &self.cfg_context,
                ancestors: &ancestors,
                direct_exposure_cfg: visibility_is_exposed(visibility).then(|| self.direct_exposure_cfg()).flatten(),
            },
        ));
    }

    fn direct_exposure_cfg(&self) -> Option<ProductionCfgContext> {
        self.module_exposure_cfg.as_ref()?.conjoin(&self.cfg_context)
    }

    fn record_impl_header_for_visible_member(&mut self, kind: &str, visibility: &Visibility, member: &impl ToTokens) {
        if !visibility_is_exposed(visibility) {
            return;
        }
        let Some(header) = self.impl_signature_headers.last().cloned() else {
            return;
        };
        let mut identity = header.clone();
        identity.extend(visibility.to_token_stream());
        identity.extend(member.to_token_stream());
        self.record_concrete_stores_in_exposure_signature_with_identity(kind, &header, &identity);
    }

    fn impl_member_is_exposed(&self, visibility: &Visibility) -> bool {
        visibility_is_exposed(visibility) || self.impl_trait_exposures.last().copied().unwrap_or(false)
    }

    fn trait_member_is_exposed(&self) -> bool {
        self.trait_exposures.last().copied().unwrap_or(false)
    }

    fn field_is_exposed(&self, visibility: &Visibility) -> bool {
        match self.field_exposures.last() {
            Some(FieldExposure::Enum(container_exposed)) => *container_exposed,
            Some(FieldExposure::Struct(container_exposed) | FieldExposure::Union(container_exposed)) => *container_exposed && visibility_is_exposed(visibility),
            None => visibility_is_exposed(visibility),
        }
    }

    fn record_visibility(&mut self, visibility: &Visibility) {
        if let Err(error) = self.visibilities.record_visibility(visibility) {
            self.error = Some(error);
        }
    }

    fn record_visibilities_in_tokens(&mut self, tokens: &TokenStream) {
        if let Err(error) = self.visibilities.record_tokens(tokens) {
            self.error = Some(error);
        }
    }

    fn enter_site_context(&mut self, kind: &str, syntax: &impl ToTokens) -> Option<String> {
        let context = context_fingerprint(self.site_context.as_deref(), kind, syntax);
        self.site_context.replace(context)
    }

    fn enter_normalized_site_context(&mut self, kind: &str, tokens: Result<TokenStream>) -> SiteContextEntry {
        match tokens {
            Ok(tokens) => SiteContextEntry::Entered(self.enter_site_context(kind, &tokens)),
            Err(error) => {
                self.error = Some(error);
                SiteContextEntry::Failed
            }
        }
    }

    fn leave_site_context(&mut self, previous: Option<String>) {
        self.site_context = previous;
    }

    fn visit_generic_default(&mut self, visit: impl FnOnce(&mut Self)) {
        self.generic_default_depth += 1;
        visit(self);
        self.generic_default_depth -= 1;
    }

    fn signature_item_path(&self) -> Vec<String> {
        if let Some(path) = self.impl_item_paths.last().filter(|path| !path.is_empty()) {
            return path.clone();
        }
        if self.block_depth > 0 {
            return Vec::new();
        }
        let Some(name) = self.declaration_ancestors.iter().rev().find_map(|ancestor| {
            ["const:", "enum:", "fn:", "static:", "struct:", "trait:", "union:"]
                .into_iter()
                .find_map(|prefix| ancestor.strip_prefix(prefix))
                .and_then(|suffix| suffix.split_once(':').map(|(name, _)| name))
        }) else {
            return Vec::new();
        };
        let mut path = self.module.clone();
        path.push(name.to_owned());
        path
    }

    fn implemented_type_path(&self, item: &ItemImpl) -> Result<Vec<String>> {
        let syn::Type::Path(path) = item.self_ty.as_ref() else {
            let mut path = self.module.clone();
            path.push(format!("{{impl-self:{}}}", syntax_fingerprint(&without_documentation(&item.self_ty.to_token_stream()))));
            return Ok(path);
        };
        if path.qself.is_some() {
            let mut synthetic_path = self.module.clone();
            synthetic_path.push(format!("{{impl-self:{}}}", syntax_fingerprint(&without_documentation(&item.self_ty.to_token_stream()))));
            return Ok(synthetic_path);
        }
        if path.path.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(Vec::new());
        }
        if self.block_depth > 0
            && path.path.leading_colon.is_none()
            && path
                .path
                .segments
                .first()
                .is_none_or(|segment| !matches!(normalized_ident(&segment.ident).as_str(), "crate" | "self" | "super"))
        {
            let root = path.path.segments.first().map(|segment| normalized_ident(&segment.ident));
            if path.path.segments.len() == 1
                && root.as_ref().is_some_and(|name| {
                    self.block_type_scopes
                        .iter()
                        .rev()
                        .any(|scope| block_binding_applies(&scope.nominal_types, name, &self.cfg_context))
                })
            {
                return Ok(Vec::new());
            }
            if root.as_ref().is_some_and(|name| {
                self.block_type_scopes
                    .iter()
                    .rev()
                    .any(|scope| block_scope_has_ambiguous_binding(scope, name, &self.cfg_context))
            }) {
                bail!(
                    "block-contained impl self type `{}` uses a block-local alias, module, or glob import and cannot be resolved safely",
                    item.self_ty.to_token_stream()
                );
            }
        }
        let mut segments = path.path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
        if path.path.leading_colon.is_some() {
            segments.insert(0, "crate".to_owned());
        }
        Ok(resolve_path(&self.module, &segments, false)?.unwrap_or_default())
    }

    fn implemented_trait_path(&self, item: &ItemImpl) -> Result<Option<Vec<String>>> {
        let Some((_, path, _)) = &item.trait_ else {
            return Ok(None);
        };
        if path.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return Ok(None);
        }
        let mut segments = path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
        if path.leading_colon.is_some() {
            segments.insert(0, "crate".to_owned());
        }
        Ok(resolve_path(&self.module, &segments, false)?.filter(|path| !path.is_empty()))
    }

    fn declaration_ancestor_identity(&self) -> String {
        self.inherited_declaration_ancestors
            .iter()
            .filter(|path| path.cfg.conjoin(&self.cfg_context).is_some())
            .map(|path| format!("out-of-line-module-path:cfg:{}\0ancestors:{}", path.cfg.identity(), path.ancestors.join("\0")))
            .chain(self.declaration_ancestors.iter().cloned())
            .collect::<Vec<_>>()
            .join("\0")
    }
}

macro_rules! visit_production_node {
    ($method:ident, $walk:ident, $node:ty, $binding:ident => $attributes:expr) => {
        fn $method(&mut self, $binding: &'ast $node) {
            let attributes: Result<&[Attribute]> = $attributes;
            let Some(previous) = self.enter_production_node(attributes) else {
                return;
            };
            visit::$walk(self, $binding);
            self.leave_production_node(previous);
        }
    };
}

impl<'ast> Visit<'ast> for ProductionSyntaxCollector {
    visit_production_node!(visit_file, visit_file, File, node => Ok(node.attrs.as_slice()));
    visit_production_node!(visit_expr, visit_expr, Expr, node => expr_attributes(node));
    visit_production_node!(visit_arm, visit_arm, Arm, node => Ok(node.attrs.as_slice()));
    visit_production_node!(visit_local, visit_local, Local, node => Ok(node.attrs.as_slice()));
    visit_production_node!(visit_stmt_macro, visit_stmt_macro, StmtMacro, node => Ok(node.attrs.as_slice()));
    visit_production_node!(
        visit_fn_arg,
        visit_fn_arg,
        FnArg,
        node => Ok(fn_arg_attributes(node))
    );
    visit_production_node!(visit_pat, visit_pat, Pat, node => pat_attributes(node));
    visit_production_node!(
        visit_bare_fn_arg,
        visit_bare_fn_arg,
        BareFnArg,
        node => Ok(node.attrs.as_slice())
    );
    visit_production_node!(
        visit_bare_variadic,
        visit_bare_variadic,
        BareVariadic,
        node => Ok(node.attrs.as_slice())
    );
    visit_production_node!(
        visit_variadic,
        visit_variadic,
        Variadic,
        node => Ok(node.attrs.as_slice())
    );
    visit_production_node!(
        visit_field_pat,
        visit_field_pat,
        FieldPat,
        node => Ok(node.attrs.as_slice())
    );
    visit_production_node!(
        visit_field_value,
        visit_field_value,
        FieldValue,
        node => Ok(node.attrs.as_slice())
    );

    fn visit_block(&mut self, node: &'ast Block) {
        let imports = match stringify_imports_in_block(node, &self.cfg_context) {
            Ok(imports) => imports,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let type_bindings = match block_local_type_bindings(node, &self.cfg_context) {
            Ok(bindings) => bindings,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.block_depth += 1;
        self.block_type_scopes.push(type_bindings);
        self.macro_shadow_scopes.push(BTreeSet::new());
        self.builtin_stringify_block_aliases.push(imports.aliases);
        self.macro_import_shadow_scopes.push(imports.shadows);
        visit::visit_block(self, node);
        self.macro_import_shadow_scopes.pop();
        self.builtin_stringify_block_aliases.pop();
        self.macro_shadow_scopes.pop();
        self.block_type_scopes.pop();
        self.block_depth -= 1;
    }

    fn visit_item(&mut self, node: &'ast Item) {
        let Some(cfg) = self.enter_production_node(item_attributes(node)) else {
            return;
        };
        let SiteContextEntry::Entered(previous) = self.enter_normalized_site_context("item", production_item_tokens(node, &self.cfg_context)) else {
            self.leave_production_node(cfg);
            return;
        };
        let ancestor = declaration_ancestor(node);
        if let Some(ancestor) = &ancestor {
            self.declaration_ancestors.push(ancestor.clone());
        }
        visit::visit_item(self, node);
        if ancestor.is_some() {
            self.declaration_ancestors.pop();
        }
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_impl_item(&mut self, node: &'ast ImplItem) {
        let Some(cfg) = self.enter_production_node(impl_item_attributes(node)) else {
            return;
        };
        let SiteContextEntry::Entered(previous) = self.enter_normalized_site_context("impl-item", production_impl_item_tokens(node, &self.cfg_context)) else {
            self.leave_production_node(cfg);
            return;
        };
        visit::visit_impl_item(self, node);
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_trait_item(&mut self, node: &'ast TraitItem) {
        let Some(cfg) = self.enter_production_node(trait_item_attributes(node)) else {
            return;
        };
        let SiteContextEntry::Entered(previous) = self.enter_normalized_site_context("trait-item", production_trait_item_tokens(node, &self.cfg_context)) else {
            self.leave_production_node(cfg);
            return;
        };
        visit::visit_trait_item(self, node);
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_foreign_item(&mut self, node: &'ast ForeignItem) {
        let Some(cfg) = self.enter_production_node(foreign_item_attributes(node)) else {
            return;
        };
        let SiteContextEntry::Entered(previous) = self.enter_normalized_site_context("foreign-item", production_foreign_item_tokens(node, &self.cfg_context)) else {
            self.leave_production_node(cfg);
            return;
        };
        visit::visit_foreign_item(self, node);
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_stmt(&mut self, node: &'ast Stmt) {
        let SiteContextEntry::Entered(previous) = self.enter_normalized_site_context("statement", production_stmt_tokens(node, &self.cfg_context)) else {
            return;
        };
        visit::visit_stmt(self, node);
        self.leave_site_context(previous);
    }

    fn visit_field(&mut self, node: &'ast Field) {
        let Some(cfg) = self.enter_production_node(Ok(node.attrs.as_slice())) else {
            return;
        };
        let previous = self.enter_site_context("field", node);
        if self.field_is_exposed(&node.vis)
            && let Err(error) = self.record_field_type_exposure(node)
        {
            self.error = Some(error);
            self.leave_site_context(previous);
            self.leave_production_node(cfg);
            return;
        }
        visit::visit_field(self, node);
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_variant(&mut self, node: &'ast Variant) {
        let Some(cfg) = self.enter_production_node(Ok(node.attrs.as_slice())) else {
            return;
        };
        self.declaration_ancestors.push(format!("variant:{}", normalized_ident(&node.ident)));
        visit::visit_variant(self, node);
        self.declaration_ancestors.pop();
        self.leave_production_node(cfg);
    }

    fn visit_generic_param(&mut self, node: &'ast GenericParam) {
        let Some(cfg) = self.enter_production_node(Ok(generic_param_attributes(node))) else {
            return;
        };
        let previous = self.enter_site_context("generic-parameter", node);
        match node {
            GenericParam::Lifetime(parameter) => visit::visit_lifetime_param(self, parameter),
            GenericParam::Type(parameter) => {
                for attribute in &parameter.attrs {
                    self.visit_attribute(attribute);
                }
                self.visit_ident(&parameter.ident);
                for bound in &parameter.bounds {
                    self.visit_type_param_bound(bound);
                }
                if let Some(default) = &parameter.default {
                    self.visit_generic_default(|collector| collector.visit_type(default));
                }
            }
            GenericParam::Const(parameter) => {
                for attribute in &parameter.attrs {
                    self.visit_attribute(attribute);
                }
                self.visit_ident(&parameter.ident);
                self.visit_type(&parameter.ty);
                if let Some(default) = &parameter.default {
                    self.visit_generic_default(|collector| collector.visit_expr(default));
                }
            }
        }
        self.leave_site_context(previous);
        self.leave_production_node(cfg);
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        let previous = self.enter_site_context("use", item);
        let result = self.collect_use(item).and_then(|()| {
            if self.block_depth == 0 {
                self.record_use_resolutions(item)?;
                self.record_public_reexport(item)?;
            }
            Ok(())
        });
        if self.error.is_none()
            && let Err(error) = result
        {
            self.error = Some(error);
            self.leave_site_context(previous);
            return;
        }
        visit::visit_item_use(self, item);
        self.leave_site_context(previous);
    }

    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        self.record_type_declaration(TypeDeclarationKind::Type, "struct", &item.ident, &item.vis);
        let exposed = visibility_is_exposed(&item.vis);
        if exposed {
            self.record_declaration_generics("struct-generics", &item.vis, &item.generics);
            if let Err(error) = self.record_exposed_generic_default_types("struct-generics", &item.ident, &item.vis, &item.generics) {
                self.error = Some(error);
                return;
            }
        }
        if matches!(item.vis, Visibility::Public(_)) {
            let mut item_path = self.module.clone();
            item_path.push(normalized_ident(&item.ident));
            let ancestors = self.declaration_ancestor_identity();
            let direct_exposure_cfg = self.direct_exposure_cfg();
            let signature = SignatureSiteContext {
                item_path: &item_path,
                cfg: &self.cfg_context,
                impl_self_type: false,
                direct_exposure_cfg: direct_exposure_cfg.as_ref(),
                required_trait_path: None,
            };
            if let Err(error) = self.concrete_stores.record_public_struct_declaration(item, &signature, &ancestors) {
                self.error = Some(error);
                return;
            }
        }
        self.enter_generic_scope(&item.generics);
        self.field_exposures.push(FieldExposure::Struct(exposed));
        visit::visit_item_struct(self, item);
        self.field_exposures.pop();
        self.leave_generic_scope();
    }

    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        self.record_type_declaration(TypeDeclarationKind::Type, "enum", &item.ident, &item.vis);
        let exposed = visibility_is_exposed(&item.vis);
        if exposed {
            self.record_declaration_generics("enum-generics", &item.vis, &item.generics);
            if let Err(error) = self.record_exposed_generic_default_types("enum-generics", &item.ident, &item.vis, &item.generics) {
                self.error = Some(error);
                return;
            }
        }
        self.enter_generic_scope(&item.generics);
        self.field_exposures.push(FieldExposure::Enum(exposed));
        visit::visit_item_enum(self, item);
        self.field_exposures.pop();
        self.leave_generic_scope();
    }

    fn visit_item_union(&mut self, item: &'ast ItemUnion) {
        self.record_type_declaration(TypeDeclarationKind::Type, "union", &item.ident, &item.vis);
        let exposed = visibility_is_exposed(&item.vis);
        if exposed {
            self.record_declaration_generics("union-generics", &item.vis, &item.generics);
            if let Err(error) = self.record_exposed_generic_default_types("union-generics", &item.ident, &item.vis, &item.generics) {
                self.error = Some(error);
                return;
            }
        }
        self.enter_generic_scope(&item.generics);
        self.field_exposures.push(FieldExposure::Union(exposed));
        visit::visit_item_union(self, item);
        self.field_exposures.pop();
        self.leave_generic_scope();
    }

    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        self.record_type_declaration(TypeDeclarationKind::Trait, "trait", &item.ident, &item.vis);
        let exposed = visibility_is_exposed(&item.vis);
        if exposed {
            let mut header = item.clone();
            header.attrs.clear();
            header.items.clear();
            self.record_concrete_stores_in_signature("trait-header", &header);
            if let Err(error) = self.record_exposed_generic_default_types("trait-generics", &item.ident, &item.vis, &item.generics) {
                self.error = Some(error);
                return;
            }
        }
        self.enter_generic_scope(&item.generics);
        if exposed && let Err(error) = self.record_exposed_supertraits(item) {
            self.error = Some(error);
            self.leave_generic_scope();
            return;
        }
        self.trait_exposures.push(exposed);
        visit::visit_item_trait(self, item);
        self.trait_exposures.pop();
        self.leave_generic_scope();
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        let item_path = match self.implemented_type_path(item) {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        self.impl_item_paths.push(item_path.clone());
        let trait_path = match self.implemented_trait_path(item) {
            Ok(path) => path,
            Err(error) => {
                self.error = Some(error);
                self.impl_item_paths.pop();
                return;
            }
        };
        self.impl_trait_paths.push(trait_path.clone());
        let mut header = item.clone();
        header.items.clear();
        let header_tokens = without_documentation(&header.to_token_stream());
        if let Err(error) = self.record_impl_self_type_exposures(item, &item_path, trait_path.as_deref()) {
            self.error = Some(error);
            self.impl_trait_paths.pop();
            self.impl_item_paths.pop();
            return;
        }
        if let Some(trait_path) = trait_path.as_deref()
            && let Err(error) = self.record_trait_implementation_exposures(item, &item_path, trait_path, &header_tokens)
        {
            self.error = Some(error);
            self.impl_trait_paths.pop();
            self.impl_item_paths.pop();
            return;
        }
        let binding = if item.trait_.is_some() { "trait-implementation" } else { "impl-header" };
        let binding = format!("{binding}:{}", syntax_fingerprint(&header_tokens));
        let context = format!("{binding}\0cfg:{}\0ancestors:{}", self.cfg_context.identity(), self.declaration_ancestor_identity());
        self.concrete_stores.record_binding_tokens(
            &header_tokens,
            &BindingSiteContext {
                fingerprint: &context,
                item_path: self.impl_item_paths.last().map_or(&[], Vec::as_slice),
                cfg: &self.cfg_context,
            },
        );
        let trait_exposure = item.trait_.is_some();
        if trait_exposure {
            self.record_concrete_stores_in_exposure_signature_with_identity("trait-impl-header", &header_tokens, &header_tokens);
        }
        self.enter_generic_scope(&item.generics);
        self.impl_signature_headers.push(header_tokens);
        self.impl_trait_exposures.push(trait_exposure);
        visit::visit_item_impl(self, item);
        self.impl_trait_exposures.pop();
        self.impl_signature_headers.pop();
        self.leave_generic_scope();
        self.impl_trait_paths.pop();
        self.impl_item_paths.pop();
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if visibility_is_exposed(&item.vis)
            && let Err(error) = self.record_exposed_function_signature("function-signature", &item.vis, &item.sig)
        {
            self.error = Some(error);
            return;
        }
        visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if let Err(error) = self.record_impl_method_exposure(item) {
            self.error = Some(error);
            return;
        }
        visit::visit_impl_item_fn(self, item);
    }

    fn visit_trait_item_fn(&mut self, item: &'ast TraitItemFn) {
        if self.trait_member_is_exposed()
            && let Err(error) = self.record_exposed_trait_signature(&item.sig)
        {
            self.error = Some(error);
            return;
        }
        visit::visit_trait_item_fn(self, item);
    }

    fn visit_foreign_item_fn(&mut self, item: &'ast ForeignItemFn) {
        if visibility_is_exposed(&item.vis)
            && let Err(error) = self.record_exposed_function_signature("foreign-function-signature", &item.vis, &item.sig)
        {
            self.error = Some(error);
            return;
        }
        visit::visit_foreign_item_fn(self, item);
    }

    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        if visibility_is_exposed(&item.vis)
            && let Err(error) = self.record_item_const_type_exposure(item)
        {
            self.error = Some(error);
            return;
        }
        visit::visit_item_const(self, item);
    }

    fn visit_item_static(&mut self, item: &'ast ItemStatic) {
        if visibility_is_exposed(&item.vis)
            && let Err(error) = self.record_item_static_type_exposure(item)
        {
            self.error = Some(error);
            return;
        }
        visit::visit_item_static(self, item);
    }

    fn visit_impl_item_const(&mut self, item: &'ast ImplItemConst) {
        if let Err(error) = self.record_impl_const_type_exposure(item) {
            self.error = Some(error);
            return;
        }
        visit::visit_impl_item_const(self, item);
    }

    fn visit_trait_item_const(&mut self, item: &'ast TraitItemConst) {
        if self.trait_member_is_exposed()
            && let Err(error) = self.record_trait_const_type_exposure(item)
        {
            self.error = Some(error);
            return;
        }
        visit::visit_trait_item_const(self, item);
    }

    fn visit_foreign_item_static(&mut self, item: &'ast ForeignItemStatic) {
        if visibility_is_exposed(&item.vis)
            && let Err(error) = self.record_foreign_static_type_exposure(item)
        {
            self.error = Some(error);
            return;
        }
        visit::visit_foreign_item_static(self, item);
    }

    fn visit_item_type(&mut self, item: &'ast ItemType) {
        if visibility_is_exposed(&item.vis)
            && let Err(error) = self.record_exposed_generic_default_types("type-alias-generics", &item.ident, &item.vis, &item.generics)
        {
            self.error = Some(error);
            return;
        }
        if self.block_depth == 0
            && let Err(error) = self.record_type_alias_evidence(item)
        {
            self.error = Some(error);
            return;
        }
        let import_count = self.imports.len();
        let concrete_before = self.concrete_stores.counts;
        visit::visit_item_type(self, item);
        if self.error.is_none() && self.imports.len() != import_count && visibility_is_exposed(&item.vis) {
            self.error = Some(anyhow::anyhow!("production restricted imports cannot be exposed through public type aliases"));
        }
        self.reject_concrete_store_alias(concrete_before);
    }

    fn visit_impl_item_type(&mut self, item: &'ast ImplItemType) {
        let concrete_before = self.concrete_stores.counts;
        self.record_impl_header_for_visible_member("inherent-impl-type", &item.vis, &item.ty);
        if let Err(error) = self.record_impl_associated_type_exposure(item) {
            self.error = Some(error);
            return;
        }
        visit::visit_impl_item_type(self, item);
        self.reject_concrete_store_alias(concrete_before);
    }

    fn visit_trait_item_type(&mut self, item: &'ast TraitItemType) {
        let concrete_before = self.concrete_stores.counts;
        if self.trait_member_is_exposed()
            && let Err(error) = self.record_trait_associated_type_bound_exposures(item)
        {
            self.error = Some(error);
            return;
        }
        visit::visit_trait_item_type(self, item);
        self.reject_concrete_store_alias(concrete_before);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast ItemExternCrate) {
        if self.error.is_none() && item.ident == "self" && item.rename.is_some() {
            self.error = Some(anyhow::anyhow!(
                "production crate-root extern aliases cannot be classified safely for dependency boundaries"
            ));
            return;
        }
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        self.record_concrete_store(ident);
    }

    fn visit_token_stream(&mut self, tokens: &'ast TokenStream) {
        self.record_concrete_stores_in_tokens(tokens);
        self.record_visibilities_in_tokens(tokens);
    }

    fn visit_visibility(&mut self, visibility: &'ast Visibility) {
        self.record_visibility(visibility);
        visit::visit_visibility(self, visibility);
    }

    fn visit_path(&mut self, path: &'ast SynPath) {
        if self.error.is_none() && (path.leading_colon.is_none() || self.rust_2015_absolute_paths) {
            let mut segments = path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
            if path.leading_colon.is_some() {
                segments.insert(0, "crate".to_owned());
            }
            let is_qualified = segments.len() > 1 || matches!(segments.first().map(String::as_str), Some("crate" | "self" | "super"));
            if is_qualified && let Err(error) = self.collect_segments(&segments, false, false) {
                self.error = Some(error);
                return;
            }
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if self.error.is_some() {
            return;
        }
        let stringifies = is_explicit_builtin_stringify(node) || self.is_imported_builtin_stringify(node);
        if self.macro_context == MacroContext::Invocation
            && !stringifies
            && let Err(error) = self.visibility_macros.record_invocation(&self.module, &node.path, &node.tokens)
        {
            self.error = Some(error);
            return;
        }
        if !stringifies && tokens_may_hide_concrete_store(&node.tokens) {
            self.error = Some(anyhow::anyhow!(
                "production concrete stores cannot be hidden behind macro-generated aliases or renamed imports"
            ));
            return;
        }
        let previous = self.enter_site_context("macro-invocation", node);
        if self.collect_internal_imports && !stringifies {
            match restricted_token_identifier(&node.tokens, &self.module, self.rust_2015_absolute_paths, StringScan::RustFragment) {
                Ok(Some(restricted)) => {
                    self.error = Some(anyhow::anyhow!(
                        "production macro token stream names restricted crate module {restricted:?} and cannot be classified safely"
                    ));
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.error = Some(error);
                    self.leave_site_context(previous);
                    return;
                }
            }
        }
        if self.require_reviewed_expansions && !stringifies && !reviewed_macro_expansion(node) {
            self.error = Some(anyhow::anyhow!("production code invokes unreviewed macro expansion path {}", node.path.to_token_stream()));
            self.leave_site_context(previous);
            return;
        }
        self.visit_path(&node.path);
        if !stringifies {
            self.record_concrete_stores_in_tokens(&node.tokens);
            if self.macro_context == MacroContext::Definition {
                self.record_visibilities_in_tokens(&node.tokens);
            }
        }
        self.leave_site_context(previous);
    }

    fn visit_attribute(&mut self, attribute: &'ast Attribute) {
        if self.error.is_some() {
            return;
        }
        if self.collect_internal_imports {
            match restricted_attribute_identifier(attribute, &self.module, self.rust_2015_absolute_paths, &self.cfg_context) {
                Ok(Some(restricted)) => {
                    self.error = Some(anyhow::anyhow!(
                        "production attribute token stream names restricted crate module {restricted:?} and cannot be classified safely"
                    ));
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        if self.require_reviewed_expansions && !reviewed_attribute_expansion(attribute) {
            self.error = Some(anyhow::anyhow!("production code uses unreviewed attribute expansion {}", attribute.meta.to_token_stream()));
            return;
        }
        let previous = self.enter_site_context("attribute", attribute);
        let site_context = self.site_context.as_deref().expect("attribute site context");
        if let Err(error) = self.concrete_stores.record_attribute(attribute, site_context, &self.cfg_context) {
            self.error = Some(error);
            self.leave_site_context(previous);
            return;
        }
        if self.generic_default_depth > 0
            && let Err(error) = self.concrete_stores.record_generic_default_attribute(attribute, site_context, &self.cfg_context)
        {
            self.error = Some(error);
            self.leave_site_context(previous);
            return;
        }
        if let Err(error) = self.visibilities.record_attribute(attribute, &self.cfg_context) {
            self.error = Some(error);
            self.leave_site_context(previous);
            return;
        }
        self.leave_site_context(previous);
    }

    fn visit_item_macro(&mut self, item: &'ast ItemMacro) {
        let Some(name) = &item.ident else {
            visit::visit_item_macro(self, item);
            return;
        };
        for attribute in &item.attrs {
            self.visit_attribute(attribute);
        }
        if self.error.is_some() {
            return;
        }
        if contains_production_concrete_store(&item.mac.tokens, &self.cfg_context) {
            self.error = Some(anyhow::anyhow!("production macro definitions cannot inject concrete stores into call sites"));
            return;
        }
        if let Err(error) = self.visibility_macros.record_definition(&self.module, name, &item.mac.tokens) {
            self.error = Some(error);
            return;
        }
        let name = normalized_ident(name);
        if RESERVED_LOCAL_MACROS.contains(&name.as_str())
            && let Err(error) = self.collect_reviewed_macro_transcribers(&name, &item.mac.tokens)
        {
            self.error = Some(error.context(format!("reviewed production macro definition `{name}` cannot be analyzed")));
            return;
        }
        if self.collect_internal_imports {
            match restricted_token_identifier(&item.mac.tokens, &self.module, self.rust_2015_absolute_paths, StringScan::RustFragment) {
                Ok(Some(restricted)) => {
                    self.error = Some(anyhow::anyhow!(
                        "production macro token stream names restricted crate module {restricted:?} and cannot be classified safely"
                    ));
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        if let Some(scope) = self.macro_shadow_scopes.last_mut() {
            scope.insert(MacroShadow {
                name,
                cfg: self.cfg_context.clone(),
            });
        }
        self.macro_context = MacroContext::Definition;
        self.record_visibilities_in_tokens(&item.mac.tokens);
        self.macro_context = MacroContext::Invocation;
    }

    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        for attribute in &item.attrs {
            self.visit_attribute(attribute);
        }
        if self.error.is_some() {
            return;
        }
        self.visit_ident(&item.ident);
        let Some((_, items)) = &item.content else {
            return;
        };
        let previous_exposure = self.module_exposure_cfg.clone();
        self.module_exposure_cfg = if visibility_is_exposed(&item.vis) {
            previous_exposure.as_ref().and_then(|cfg| cfg.conjoin(&self.cfg_context))
        } else {
            None
        };
        self.module.push(normalized_ident(&item.ident));
        self.macro_shadow_scopes.push(BTreeSet::new());
        for nested in items {
            self.visit_item(nested);
        }
        self.macro_shadow_scopes.pop();
        self.module.pop();
        self.module_exposure_cfg = previous_exposure;
    }
}

impl ProductionSyntaxCollector {
    fn is_imported_builtin_stringify(&self, node: &syn::Macro) -> bool {
        if node.path.leading_colon.is_some() || node.path.segments.len() != 1 {
            return false;
        }
        let alias = normalized_ident(&node.path.segments[0].ident);
        let block_binding = self
            .builtin_stringify_block_aliases
            .iter()
            .zip(&self.macro_import_shadow_scopes)
            .rev()
            .find_map(|(builtin_scope, shadow_scope)| {
                let builtins = builtin_scope
                    .iter()
                    .filter(|candidate| candidate.name == alias)
                    .map(|candidate| &candidate.cfg)
                    .collect::<Vec<_>>();
                let shadows = shadow_scope
                    .iter()
                    .filter(|candidate| candidate.name == alias)
                    .map(|candidate| &candidate.cfg)
                    .collect::<Vec<_>>();
                binding_is_fully_builtin(&self.cfg_context, &builtins, &shadows)
            });
        let imported = block_binding.unwrap_or_else(|| {
            let builtins = self
                .module_stringify_imports
                .aliases
                .iter()
                .filter(|candidate| candidate.module == self.module && candidate.name == alias)
                .map(|candidate| &candidate.cfg)
                .collect::<Vec<_>>();
            let shadows = self
                .module_stringify_imports
                .shadows
                .iter()
                .filter(|candidate| candidate.module == self.module && candidate.name == alias)
                .map(|candidate| &candidate.cfg)
                .collect::<Vec<_>>();
            binding_is_fully_builtin(&self.cfg_context, &builtins, &shadows).unwrap_or(false)
        });
        imported
            && !self
                .macro_shadow_scopes
                .iter()
                .rev()
                .any(|scope| scope.iter().any(|shadow| shadow.name == alias && shadow.cfg.conjoin(&self.cfg_context).is_some()))
    }
}

fn declaration_ancestor(item: &Item) -> Option<String> {
    match item {
        Item::Const(item) => Some(named_ancestor("const", &item.ident, &item.vis)),
        Item::Enum(item) => Some(named_ancestor("enum", &item.ident, &item.vis)),
        Item::Fn(item) => Some(named_ancestor("fn", &item.sig.ident, &item.vis)),
        Item::Impl(item) => {
            let mut header = item.clone();
            header.items.clear();
            Some(format!("impl:{}", syntax_fingerprint(&without_documentation(&header.to_token_stream()))))
        }
        Item::Mod(item) => Some(named_ancestor("mod", &item.ident, &item.vis)),
        Item::Static(item) => Some(named_ancestor("static", &item.ident, &item.vis)),
        Item::Struct(item) => Some(named_ancestor("struct", &item.ident, &item.vis)),
        Item::Trait(item) => Some(named_ancestor("trait", &item.ident, &item.vis)),
        Item::Union(item) => Some(named_ancestor("union", &item.ident, &item.vis)),
        _ => None,
    }
}

fn block_local_type_bindings(block: &Block, inherited_cfg: &ProductionCfgContext) -> Result<BlockTypeBindings> {
    let mut bindings = BlockTypeBindings::default();
    for item in block.stmts.iter().filter_map(|statement| match statement {
        Stmt::Item(item) => Some(item),
        Stmt::Local(_) | Stmt::Expr(_, _) | Stmt::Macro(_) => None,
    }) {
        let Some(cfg) = production_cfg_context(item_attributes(item)?, inherited_cfg)? else {
            continue;
        };
        let nominal = match item {
            Item::Enum(item) => Some(&item.ident),
            Item::Struct(item) => Some(&item.ident),
            Item::Union(item) => Some(&item.ident),
            _ => None,
        };
        if let Some(ident) = nominal {
            bindings.nominal_types.insert(BlockTypeBinding {
                name: normalized_ident(ident),
                cfg,
            });
            continue;
        }
        match item {
            Item::ExternCrate(item) => {
                let ident = item.rename.as_ref().map_or(&item.ident, |(_, rename)| rename);
                bindings.ambiguous_roots.insert(BlockTypeBinding {
                    name: normalized_ident(ident),
                    cfg,
                });
            }
            Item::Mod(item) => {
                bindings.ambiguous_roots.insert(BlockTypeBinding {
                    name: normalized_ident(&item.ident),
                    cfg,
                });
            }
            Item::Type(item) => {
                bindings.ambiguous_roots.insert(BlockTypeBinding {
                    name: normalized_ident(&item.ident),
                    cfg,
                });
            }
            Item::Use(item) => record_block_use_bindings(item, &cfg, &mut bindings),
            _ => {}
        }
    }
    Ok(bindings)
}

fn record_block_use_bindings(item: &ItemUse, cfg: &ProductionCfgContext, bindings: &mut BlockTypeBindings) {
    let mut paths = Vec::new();
    flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
    for path in paths {
        if path.segments.last().is_some_and(|segment| segment == "*") {
            bindings.glob_imports.insert(cfg.clone());
            continue;
        }
        if let Some(name) = path.alias.or_else(|| path.segments.last().cloned()) {
            bindings.ambiguous_roots.insert(BlockTypeBinding { name, cfg: cfg.clone() });
        }
    }
}

fn block_binding_applies(bindings: &BTreeSet<BlockTypeBinding>, name: &str, cfg: &ProductionCfgContext) -> bool {
    bindings.iter().any(|binding| binding.name == name && binding.cfg.conjoin(cfg).is_some())
}

fn block_scope_has_ambiguous_binding(scope: &BlockTypeBindings, name: &str, cfg: &ProductionCfgContext) -> bool {
    block_binding_applies(&scope.ambiguous_roots, name, cfg) || scope.glob_imports.iter().any(|glob_cfg| glob_cfg.conjoin(cfg).is_some())
}

fn named_ancestor(kind: &str, ident: &proc_macro2::Ident, visibility: &Visibility) -> String {
    format!("{kind}:{}:{}", normalized_ident(ident), syntax_fingerprint(visibility))
}

fn tokens_may_hide_concrete_store(tokens: &TokenStream) -> bool {
    let mut identifiers = Vec::new();
    collect_token_identifiers(tokens, &mut identifiers);
    identifiers.iter().any(|identifier| is_concrete_store_name(identifier))
        && (identifiers.iter().any(|identifier| identifier == "type")
            || identifiers.iter().any(|identifier| identifier == "use") && identifiers.iter().any(|identifier| identifier == "as"))
}

fn collect_token_identifiers(tokens: &TokenStream, identifiers: &mut Vec<String>) {
    for token in resolving_tokens(tokens) {
        match token {
            TokenTree::Group(group) => collect_token_identifiers(&group.stream(), identifiers),
            TokenTree::Ident(ident) => identifiers.push(normalized_ident(&ident)),
            TokenTree::Literal(_) | TokenTree::Punct(_) => {}
        }
    }
}

#[cfg(test)]
mod tests;
