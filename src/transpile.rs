//! TypeScript -> JavaScript type stripping using oxc.

use std::path::Path;

use oxc::allocator::Allocator;
use oxc::codegen::Codegen;
use oxc::parser::Parser;
use oxc::semantic::SemanticBuilder;
use oxc::span::SourceType;
use oxc::transformer::{TransformOptions, Transformer};

pub fn transpile_ts(code: &str, file_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let source_type = if file_name.ends_with(".tsx") {
        SourceType::tsx()
    } else {
        SourceType::ts()
    }
    // The runtime evaluates code as a classic script (not an ES module), so
    // parse it the same way to avoid module-only output such as `export {}`.
    .with_script(true);

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

    Ok(Codegen::new().build(&program).code)
}

fn join_errors(file_name: &str, errors: &[oxc::diagnostics::OxcDiagnostic]) -> Box<dyn std::error::Error> {
    let msg = errors.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n");
    format!("{file_name}: {msg}").into()
}
