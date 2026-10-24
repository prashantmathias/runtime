use std::fs;
use std::io::{self, Write};
use clap::Parser;

mod jsc;
mod mimalloc_shim;
mod runtime;
mod transpile;

use runtime::Runtime;
use transpile::transpile_ts;

#[derive(Parser)]
#[command(name = "runtime")]
#[command(version = "0.1.0")]
#[command(about = "A custom JavaScript/TypeScript runtime built in Rust and JavaScriptCore (Bun's engine)", long_about = None)]
struct Cli {
    /// The path to the JavaScript/TypeScript file to execute. If omitted, starts the REPL.
    file: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cli = Cli::parse();

    let result = match cli.file {
        Some(file) => run_file(&file).await,
        None => run_repl().await,
    };

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run_file(file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let code = fs::read_to_string(file)?;
    let code = transpile_ts(&code, file)?;

    let mut runtime = Runtime::new()?;
    runtime.execute_script(file, &code)?;
    runtime.run_event_loop().await?;
    Ok(())
}

async fn run_repl() -> Result<(), Box<dyn std::error::Error>> {
    let mut runtime = Runtime::new()?;

    let mut input = String::new();

    println!("Welcome to Custom JS/TS Runtime REPL!");
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

        // Transpile input in case it contains TypeScript syntax
        let js_input = match transpile_ts(trimmed, "repl.ts") {
            Ok(js) => js,
            Err(err) => {
                eprintln!("Transpile Error: {}", err);
                continue;
            }
        };

        // Evaluate input, then let any async work it started complete.
        match runtime.execute_script("repl", &js_input) {
            Ok(Some(value)) => println!("{}", value),
            Ok(None) => {}
            Err(err) => eprintln!("Error: {}", err),
        }
        if let Err(err) = runtime.run_event_loop().await {
            eprintln!("Error: {}", err);
        }
    }

    Ok(())
}
