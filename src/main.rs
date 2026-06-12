use std::env;
use std::fs;
use std::io::{self, Write};
use deno_core::extension;
use deno_core::op2;
use deno_core::JsRuntime;
use deno_core::RuntimeOptions;
use tokio::fs as tokio_fs;
use clap::Parser;

#[op2(fast)]
fn op_custom_print(#[string] msg: &str, is_err: bool) {
    if is_err {
        eprintln!("{}", msg);
    } else {
        println!("{}", msg);
    }
}

#[op2]
async fn op_sleep(delay_ms: u32) {
    tokio::time::sleep(std::time::Duration::from_millis(delay_ms as u64)).await;
}

#[op2]
#[string]
fn op_fs_read_file_sync(#[string] path: String) -> Result<String, std::io::Error> {
    let contents = fs::read_to_string(path)?;
    Ok(contents)
}

#[op2(fast)]
fn op_fs_write_file_sync(#[string] path: String, #[string] contents: String) -> Result<(), std::io::Error> {
    fs::write(path, contents)?;
    Ok(())
}

#[op2]
#[string]
async fn op_fs_read_file_async(#[string] path: String) -> Result<String, std::io::Error> {
    let contents = tokio_fs::read_to_string(path).await?;
    Ok(contents)
}

#[op2]
async fn op_fs_write_file_async(#[string] path: String, #[string] contents: String) -> Result<(), std::io::Error> {
    tokio_fs::write(path, contents).await?;
    Ok(())
}

extension!(
    runtime,
    ops = [
        op_custom_print,
        op_sleep,
        op_fs_read_file_sync,
        op_fs_write_file_sync,
        op_fs_read_file_async,
        op_fs_write_file_async
    ],
    js = ["src/bootstrap.js"]
);

#[derive(Parser)]
#[command(name = "runtime")]
#[command(version = "0.1.0")]
#[command(about = "A custom JavaScript runtime built in Rust and V8", long_about = None)]
struct Cli {
    /// The path to the JavaScript file to execute. If omitted, starts the REPL.
    file: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.file {
        Some(file) => {
            let code = fs::read_to_string(&file)?;

            let mut runtime = JsRuntime::new(RuntimeOptions {
                extensions: vec![runtime::init()],
                ..Default::default()
            });

            runtime.execute_script(file, code)?;
            runtime.run_event_loop(Default::default()).await?;
        }
        None => {
            run_repl().await?;
        }
    }

    Ok(())
}

async fn run_repl() -> Result<(), Box<dyn std::error::Error>> {
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![runtime::init()],
        ..Default::default()
    });

    let mut input = String::new();

    println!("Welcome to Custom JS Runtime REPL!");
    println!("Type 'exit' or 'quit' to exit.");

    loop {
        print!("> ");
        io::stdout().flush()?;
        input.clear();
        let bytes_read = io::stdin().read_line(&mut input)?;
        if bytes_read == 0 {
            println!();
            break; // Ctrl+D
        }

        let trimmed = input.trim();
        if trimmed == "exit" || trimmed == "quit" {
            break;
        }

        if trimmed.is_empty() {
            continue;
        }

        // Evaluate input
        match runtime.execute_script("repl", input.clone()) {
            Ok(value) => {
                let context_global = runtime.main_context();
                let scope_storage = deno_core::v8::HandleScope::new(runtime.v8_isolate());
                let scope_storage = std::pin::pin!(scope_storage);
                let mut scope_without_context = scope_storage.init();
                
                let context_local = deno_core::v8::Local::new(&mut scope_without_context, context_global);
                
                let mut scope = deno_core::v8::ContextScope::new(&mut scope_without_context, context_local);
                
                let local_value = deno_core::v8::Local::new(&mut scope, value);
                let value_str = local_value.to_string(&mut scope);
                if let Some(v8_str) = value_str {
                    let rust_str = v8_str.to_rust_string_lossy(&mut scope);
                    if rust_str != "undefined" {
                        println!("{}", rust_str);
                    }
                }
            }
            Err(err) => {
                eprintln!("Error: {}", err);
            }
        }
    }

    Ok(())
}
