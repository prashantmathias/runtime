//! TypeScript -> JavaScript type stripping and ES module (`import`/`export`)
//! transformation using `oxc`, plus Node/Bun-compatible module resolution via
//! `oxc_resolver`.

use std::path::{Path, PathBuf};

use oxc::allocator::Allocator;
use oxc::ast::ast::{
    BindingPattern, Declaration, ExportDefaultDeclarationKind, ImportDeclarationSpecifier,
    ModuleExportName, Statement,
};
use oxc::codegen::Codegen;
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
use oxc::span::{GetSpan, SourceType};
use oxc::transformer::{TransformOptions, Transformer};
use oxc_resolver::{ResolveOptions, Resolver};

pub fn transpile_ts(code: &str, file_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let source_type = if file_name.ends_with(".tsx") || file_name.ends_with(".jsx") {
        SourceType::tsx()
    } else {
        SourceType::ts()
    }
    .with_unambiguous(true);

    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, source_type).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return Err(join_errors(file_name, &parsed.diagnostics));
    }
    let mut program = parsed.program;

    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();
    let options = TransformOptions::default();
    let transformed =
        Transformer::new(&allocator, Path::new(file_name), &options).build_with_scoping(scoping, &mut program);
    if !transformed.diagnostics.is_empty() {
        return Err(join_errors(file_name, &transformed.diagnostics));
    }

    let js_code = Codegen::new().build(&program).code;
    rewrite_esm_to_cjs(&js_code)
}

/// Resolves `specifier` from `referrer` using `oxc_resolver`, reads the target
/// file, and transpiles it if it is JS/TS. Returns `(resolved_path, code, is_json)`.
pub fn resolve_and_load_module(specifier: &str, referrer: &str) -> Result<(String, String, bool), String> {
    let base_dir = referrer_dir(referrer);
    let resolver = Resolver::new(ResolveOptions {
        extensions: vec![
            ".ts".into(),
            ".tsx".into(),
            ".mjs".into(),
            ".js".into(),
            ".cjs".into(),
            ".json".into(),
        ],
        condition_names: vec!["require".into(), "import".into(), "node".into(), "default".into()],
        main_fields: vec!["main".into(), "module".into()],
        ..ResolveOptions::default()
    });

    let resolution = resolver
        .resolve(&base_dir, specifier)
        .map_err(|e| format!("Cannot find module '{specifier}' from '{}': {e}", base_dir.display()))?;

    let full_path = resolution.full_path();
    let path_str = full_path.to_string_lossy().into_owned();
    let contents = std::fs::read_to_string(&full_path)
        .map_err(|e| format!("Failed to read module '{}': {e}", path_str))?;

    if path_str.ends_with(".json") {
        return Ok((path_str, contents, true));
    }

    let transpiled = transpile_ts(&contents, &path_str).map_err(|e| e.to_string())?;
    Ok((path_str, transpiled, false))
}

fn referrer_dir(referrer: &str) -> PathBuf {
    if !referrer.is_empty() && referrer != "repl" && referrer != "repl.ts" {
        let p = Path::new(referrer);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(p)
        };
        if let Some(parent) = abs.parent() {
            return parent.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// If `js_code` contains top-level ES module `import`/`export` statements,
/// rewrites them into CommonJS `require(...)` and `exports` mutations so they
/// can execute inside `JSEvaluateScript` or a CommonJS module wrapper.
fn rewrite_esm_to_cjs(js_code: &str) -> Result<String, Box<dyn std::error::Error>> {
    let allocator = Allocator::default();
    let source_type = SourceType::mjs().with_unambiguous(true);
    let parsed = Parser::new(&allocator, js_code, source_type).parse();
    if parsed.panicked || !parsed.diagnostics.is_empty() {
        return Ok(js_code.to_string());
    }

    let has_esm = parsed.program.body.iter().any(|stmt| {
        matches!(
            stmt,
            Statement::ImportDeclaration(_)
                | Statement::ExportDeclaration(_)
                | Statement::ExportNamedDeclaration(_)
                | Statement::ExportFromDeclaration(_)
                | Statement::ExportDefaultDeclaration(_)
                | Statement::ExportAllDeclaration(_)
        )
    });

    if !has_esm {
        return Ok(js_code.to_string());
    }

    let mut hoisted_exports: Vec<(String, String)> = Vec::new();
    let mut body_out = String::with_capacity(js_code.len() + 256);
    let mut counter: usize = 0;

    for stmt in &parsed.program.body {
        match stmt {
            Statement::ImportDeclaration(import_decl) => {
                let spec_json = serde_json::to_string(import_decl.source.value.as_str())?;
                match &import_decl.specifiers {
                    None => {
                        body_out.push_str(&format!("require({spec_json});\n"));
                    }
                    Some(specs) if specs.is_empty() => {
                        body_out.push_str(&format!("require({spec_json});\n"));
                    }
                    Some(specs) => {
                        let tmp = format!("__import_{counter}");
                        counter += 1;
                        body_out.push_str(&format!("const {tmp} = __toESM(require({spec_json}));\n"));
                        for spec in specs {
                            match spec {
                                ImportDeclarationSpecifier::ImportDefaultSpecifier(d) => {
                                    let local = d.local.name.as_str();
                                    body_out.push_str(&format!("const {local} = {tmp}.default;\n"));
                                }
                                ImportDeclarationSpecifier::ImportNamespaceSpecifier(ns) => {
                                    let local = ns.local.name.as_str();
                                    body_out.push_str(&format!("const {local} = {tmp};\n"));
                                }
                                ImportDeclarationSpecifier::ImportSpecifier(s) => {
                                    let local = s.local.name.as_str();
                                    let imported = module_export_name(&s.imported);
                                    let imported_json = serde_json::to_string(imported)?;
                                    body_out.push_str(&format!("const {local} = {tmp}[{imported_json}];\n"));
                                }
                            }
                        }
                    }
                }
            }
            Statement::ExportAllDeclaration(export_all) => {
                let spec_json = serde_json::to_string(export_all.source.value.as_str())?;
                if let Some(exported) = &export_all.exported {
                    let exported_json = serde_json::to_string(module_export_name(exported))?;
                    body_out.push_str(&format!("exports[{exported_json}] = __toESM(require({spec_json}));\n"));
                } else {
                    body_out.push_str(&format!("__exportStar(require({spec_json}), exports);\n"));
                }
            }
            Statement::ExportDefaultDeclaration(export_def) => match &export_def.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    let slice = &js_code[f.span.start as usize..f.span.end as usize];
                    if let Some(id) = &f.id {
                        let name = id.name.as_str();
                        body_out.push_str(slice);
                        body_out.push_str(&format!("\nexports.default = {name};\n"));
                    } else {
                        body_out.push_str(&format!("exports.default = ({slice});\n"));
                    }
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    let slice = &js_code[c.span.start as usize..c.span.end as usize];
                    if let Some(id) = &c.id {
                        let name = id.name.as_str();
                        body_out.push_str(slice);
                        body_out.push_str(&format!("\nexports.default = {name};\n"));
                    } else {
                        body_out.push_str(&format!("exports.default = ({slice});\n"));
                    }
                }
                decl => {
                    let span = decl.span();
                    let slice = &js_code[span.start as usize..span.end as usize];
                    body_out.push_str(&format!("exports.default = {slice};\n"));
                }
            },
            Statement::ExportDeclaration(export_decl) => {
                let decl = &export_decl.declaration;
                let span = decl.span();
                let slice = &js_code[span.start as usize..span.end as usize];
                body_out.push_str(slice);
                body_out.push('\n');
                let mut names = Vec::new();
                collect_decl_names(decl, &mut names);
                for name in names {
                    hoisted_exports.push((name.clone(), name));
                }
            }
            Statement::ExportFromDeclaration(export_from) => {
                let spec_json = serde_json::to_string(export_from.source.value.as_str())?;
                let tmp = format!("__reexport_{counter}");
                counter += 1;
                body_out.push_str(&format!("const {tmp} = __toESM(require({spec_json}));\n"));
                for s in &export_from.specifiers {
                    let imported = module_export_name(&s.local);
                    let exported = module_export_name(&s.exported);
                    let imported_json = serde_json::to_string(imported)?;
                    let exported_json = serde_json::to_string(exported)?;
                    body_out.push_str(&format!(
                        "Object.defineProperty(exports, {exported_json}, {{ enumerable: true, configurable: true, get: () => {tmp}[{imported_json}] }});\n"
                    ));
                }
            }
            Statement::ExportNamedDeclaration(export_named) => {
                for s in &export_named.specifiers {
                    let local = module_export_name(&s.local).to_string();
                    let exported = module_export_name(&s.exported).to_string();
                    hoisted_exports.push((exported, local));
                }
            }
            other => {
                let span = other.span();
                body_out.push_str(&js_code[span.start as usize..span.end as usize]);
                body_out.push('\n');
            }
        }
    }

    let mut header = String::from("Object.defineProperty(exports, \"__esModule\", { value: true });\n");
    for (exported, local) in hoisted_exports {
        let exported_json = serde_json::to_string(&exported)?;
        header.push_str(&format!(
            "Object.defineProperty(exports, {exported_json}, {{ enumerable: true, configurable: true, get: () => {local} }});\n"
        ));
    }

    header.push_str(&body_out);
    Ok(header)
}

fn module_export_name<'a>(name: &'a ModuleExportName<'a>) -> &'a str {
    match name {
        ModuleExportName::IdentifierName(id) => id.name.as_str(),
        ModuleExportName::IdentifierReference(id) => id.name.as_str(),
        ModuleExportName::StringLiteral(s) => s.value.as_str(),
    }
}

fn collect_decl_names(decl: &Declaration<'_>, out: &mut Vec<String>) {
    match decl {
        Declaration::VariableDeclaration(var_decl) => {
            for declarator in &var_decl.declarations {
                collect_binding_names(&declarator.id, out);
            }
        }
        Declaration::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                out.push(id.name.as_str().to_string());
            }
        }
        Declaration::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                out.push(id.name.as_str().to_string());
            }
        }
        _ => {}
    }
}

fn collect_binding_names(pat: &BindingPattern<'_>, out: &mut Vec<String>) {
    match pat {
        BindingPattern::BindingIdentifier(id) => {
            out.push(id.name.as_str().to_string());
        }
        BindingPattern::ObjectPattern(obj) => {
            for prop in &obj.properties {
                collect_binding_names(&prop.value, out);
            }
            if let Some(rest) = &obj.rest {
                collect_binding_names(&rest.argument, out);
            }
        }
        BindingPattern::ArrayPattern(arr) => {
            for elem in arr.elements.iter().flatten() {
                collect_binding_names(elem, out);
            }
            if let Some(rest) = &arr.rest {
                collect_binding_names(&rest.argument, out);
            }
        }
        BindingPattern::AssignmentPattern(assign) => {
            collect_binding_names(&assign.left, out);
        }
    }
}

fn join_errors(file_name: &str, errors: &[oxc::diagnostics::OxcDiagnostic]) -> Box<dyn std::error::Error> {
    let msg = errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
    format!("{file_name}: {msg}").into()
}
