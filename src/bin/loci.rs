//! Ordinary-user command line client for the independent NTFS index service.
use loci_experiment::service::{self, Filter, Request};
use std::io;

const HELP: &str = "Loci — Windows file search\n\n  loci status\n  loci query [--type all|images|documents|videos|audio|archives|folders] [--limit 1..100] QUERY\n\nThe LociIndex Windows service owns the NTFS indexes. This client does not require elevation.\nEnable the service using the installer, or run scripts/install-service.ps1 from an administrator PowerShell.\n";

fn parse(args: &[String]) -> Result<Option<Request>, String> {
    match args.first().map(String::as_str) {
        None | Some("--help" | "-h" | "help") => Ok(None),
        Some("status") if args.len() == 1 => Ok(Some(Request::Status {})),
        Some("query") => {
            let mut filter = Filter::All;
            let mut limit = 20;
            let mut terms = Vec::new();
            let mut rest = args[1..].iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--type" => {
                        let value = rest.next().ok_or("--type requires a value")?;
                        filter = serde_json::from_value(serde_json::Value::String(value.clone()))
                            .map_err(|_| format!("Unknown file type: {value}"))?;
                    }
                    "--limit" => {
                        limit = rest
                            .next()
                            .ok_or("--limit requires a value")?
                            .parse()
                            .map_err(|_| "Invalid result limit")?;
                        if !(1..=service::MAX_RESULTS).contains(&limit) {
                            return Err("Result limit must be between 1 and 100".into());
                        }
                    }
                    "--" => {
                        terms.extend(rest.cloned());
                        break;
                    }
                    value if value.starts_with("--") => {
                        return Err(format!("Unknown option: {value}"))
                    }
                    _ => terms.push(arg.clone()),
                }
            }
            if terms.is_empty() {
                return Err("query requires search text".into());
            }
            Ok(Some(Request::Query {
                query: terms.join(" "),
                filter,
                limit,
            }))
        }
        _ => Err("Use loci status or loci query; see loci --help".into()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let request = match parse(&args) {
        Ok(Some(request)) => request,
        Ok(None) => {
            print!("{HELP}");
            return;
        }
        Err(error) => {
            eprintln!("Loci: {error}");
            std::process::exit(2);
        }
    };
    match service::request(&request) {
        Ok(response) => {
            if let Err(error) = serde_json::to_writer_pretty(io::stdout().lock(), &response) {
                eprintln!("Loci output: {error}");
                std::process::exit(1);
            }
            println!();
            if response.error.is_some() {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("Loci service is unavailable: {error}. Enable the LociIndex service first.");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }
    #[test]
    fn queries_keep_reserved_words_and_validate_options() {
        let request = parse(&args(&[
            "query",
            "--type",
            "documents",
            "--limit",
            "3",
            "status",
            "ext:md",
        ]))
        .unwrap()
        .unwrap();
        assert!(
            matches!(request, Request::Query { query, filter: Filter::Documents, limit: 3 } if query == "status ext:md")
        );
        assert!(parse(&args(&["query", "--limit", "101", "a"])).is_err());
        assert!(parse(&args(&["query", "--type", "bad", "a"])).is_err());
        assert!(parse(&args(&["status", "extra"])).is_err());
        assert!(parse(&args(&["query", "--", "--filename"])).is_ok());
    }
}
