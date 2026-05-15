use std::io::{self, BufRead, Write};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // Top-level help
    if args.len() < 2 || is_help_flag(&args[1]) {
        print_general_usage(&args[0]);
        std::process::exit(if args.len() < 2 { 1 } else { 0 });
    }

    let cmd = args[1].as_str();

    // Check for command-level help anywhere in remaining args
    let help_requested = args[2..].iter().any(|a| is_help_flag(a));

    match cmd {
        "export" => {
            if help_requested || args.len() < 3 {
                print_export_usage();
                std::process::exit(if help_requested { 0 } else { 1 });
            }
            let output_flag = get_flag_value(&args[3..], &["-o", "--output"]);
            match output_flag {
                Some(Some(path)) => export(Path::new(&args[2]), Path::new(path))?,
                _ => {
                    eprintln!("Error: export requires -o or --output flag");
                    print_export_usage();
                    std::process::exit(1);
                }
            }
        }
        "build" => {
            if help_requested || args.len() < 3 {
                print_build_usage();
                std::process::exit(if help_requested { 0 } else { 1 });
            }
            let input_flag = get_flag_value(&args[3..], &["-i", "--input"]);
            let input = match input_flag {
                Some(Some(path)) => Some(path),
                Some(None) => {
                    eprintln!("Error: -i / --input requires a value");
                    std::process::exit(1);
                }
                None => None,
            };
            build(Path::new(&args[2]), input)?;
        }
        "query" => {
            if help_requested || args.len() < 3 {
                print_query_usage();
                std::process::exit(if help_requested { 0 } else { 1 });
            }
            query(Path::new(&args[2]), &args[3..])?;
        }
        _ => {
            if is_help_flag(cmd) {
                print_general_usage(&args[0]);
                std::process::exit(0);
            }
            eprintln!("Unknown command: {}", cmd);
            print_general_usage(&args[0]);
            std::process::exit(1);
        }
    }

    Ok(())
}

fn is_help_flag(arg: &str) -> bool {
    arg == "--help" || arg == "-h"
}

/// Parse manual flags from remaining args.
/// Returns Some(Some(val)) if flag found with value,
/// Some(None) if flag found but no value follows,
/// None if flag not found.
fn get_flag_value<'a>(args: &'a [String], flags: &[&str]) -> Option<Option<&'a str>> {
    for (i, arg) in args.iter().enumerate() {
        if flags.contains(&arg.as_str()) {
            return Some(args.get(i + 1).map(|s| s.as_str()));
        }
    }
    None
}

fn print_general_usage(program: &str) {
    println!("Usage: {} <command> [args...]", program);
    println!();
    println!("Commands:");
    println!("  export    Export all entries from a .gram file");
    println!("  build     Build a .gram file from TSV input");
    println!("  query     Query a .gram file");
    println!();
    println!("Options:");
    println!("  -h, --help    Print help information");
    println!();
    println!("Use '{} <command> --help' for more information on a command.", program);
}

fn print_export_usage() {
    println!("Usage: export <gram-file> -o <output-file>");
    println!();
    println!("Export all entries from a .gram file as TSV.");
    println!();
    println!("Options:");
    println!("  -o, --output <file>    Output file path (required)");
    println!("  -h, --help             Print help information");
}

fn print_build_usage() {
    println!("Usage: build <output-file> [-i <input-file>]");
    println!();
    println!("Build a .gram file from TSV input.");
    println!("If -i is omitted or '-', reads from stdin.");
    println!();
    println!("Input format: <key>\\t<value> per line, where value is a float.");
    println!();
    println!("Options:");
    println!("  -i, --input <file>    Input file path (default: stdin)");
    println!("  -h, --help            Print help information");
}

fn print_query_usage() {
    println!("Usage: query <gram-file> [keys...]");
    println!();
    println!("Query a .gram file.");
    println!("If no keys are given, reads from stdin.");
    println!();
    println!("Stdin format: one <word> or <context>\\t<word> per line.");
    println!("Command-line: one key = word lookup; two keys = context + word lookup.");
    println!();
    println!("Options:");
    println!("  -h, --help    Print help information");
}

fn export(gram_path: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let db = gram_db::GramDb::open(gram_path)?;
    let keys = db.trie.list_all_keys();
    let file = std::fs::File::create(output)?;
    let mut handle = io::BufWriter::new(file);
    for (key_bytes, scaled_value) in keys {
        let key = gram_db::GramDbKey::decode(&key_bytes);
        let value = gram_db::GramDbValue::from_scaled(scaled_value);
        writeln!(handle, "{}\t{}", key, value)?;
    }
    Ok(())
}

fn build(output: &Path, input: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let reader: Box<dyn BufRead> = match input {
        Some("-") | None => Box::new(io::BufReader::new(io::stdin())),
        Some(path) => Box::new(io::BufReader::new(std::fs::File::open(path)?)),
    };

    let mut data = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            eprintln!("Warning: skipping invalid line: {}", line);
            continue;
        }
        let key = parts[0].to_string();
        let value_str = parts[parts.len() - 1];
        let value: f64 = match value_str.parse() {
            Ok(v) => v,
            Err(_) => {
                eprintln!("Warning: invalid value in line: {}", line);
                continue;
            }
        };
        data.push((key, value));
    }

    let mut builder = gram_db::GramDbBuilder::new();
    builder.extend_data(data);
    builder.build(output)?;
    Ok(())
}

fn query(path: &Path, keys: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let db = gram_db::GramDb::open(path)?;

    if keys.is_empty() {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            let line = line?;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (context, word) = if let Some(pos) = line.find('\t') {
                (&line[..pos], &line[pos + 1..])
            } else {
                ("", line.as_str())
            };
            print_results(&db, context, word);
        }
    } else if keys.len() == 1 {
        print_results(&db, "", &keys[0]);
    } else {
        // First arg is context, second is word.
        print_results(&db, &keys[0], &keys[1]);
        // Any remaining args are treated as word-only queries.
        for key in &keys[2..] {
            print_results(&db, "", key);
        }
    }

    Ok(())
}

fn print_results(db: &gram_db::GramDb, context: &str, word: &str) {
    let mut results = [darts::Match::default(); gram_db::K_MAX_RESULTS];
    let num = db.lookup(context, word, &mut results);
    let context_bytes = gram_db::GramDbKey::encode(context);
    let word_bytes = gram_db::GramDbKey::encode(word);
    for i in 0..num {
        let m = &results[i];
        let matched_word_prefix = &word_bytes[..m.length];
        let full_key_bytes = [context_bytes.as_slice(), matched_word_prefix].concat();
        let full_key = gram_db::GramDbKey::decode(&full_key_bytes);
        let value = gram_db::GramDbValue::from_scaled(m.value);
        println!("{}\t{}", full_key, value);
    }
}
