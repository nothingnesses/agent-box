use agent_box_common::portal_client::PortalClient;
use eyre::Result;
use std::io::Write;

#[derive(Debug, Default, Clone)]
struct Args {
    list_types: bool,
    mime: Option<String>,
    no_newline: bool,
}

fn parse_args(raw: &[String]) -> Result<Args> {
    let mut out = Args::default();
    let mut i = 0usize;
    while i < raw.len() {
        match raw[i].as_str() {
            "--list-types" => out.list_types = true,
            "--no-newline" => out.no_newline = true,
            "--type" => {
                let value = raw
                    .get(i + 1)
                    .ok_or_else(|| eyre::eyre!("--type expects a value"))?;
                out.mime = Some(value.clone());
                i += 1;
            }
            "-t" => {
                let value = raw
                    .get(i + 1)
                    .ok_or_else(|| eyre::eyre!("-t expects a value"))?;
                out.mime = Some(value.clone());
                i += 1;
            }
            "-l" => out.list_types = true,
            "-n" => out.no_newline = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(eyre::eyre!("unsupported argument: {other}")),
        }
        i += 1;
    }
    Ok(out)
}

fn print_help() {
    println!("Usage: wl-paste [--list-types] [--type <mime>] [--no-newline]");
}

fn main() {
    if let Err(e) = run() {
        eprintln!("wl-paste wrapper error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&raw)?;

    let client = PortalClient::from_env_or_config();

    if args.list_types {
        let image = client.clipboard_read_image(Some("wl-paste --list-types".to_string()))?;
        println!("{}", image.mime);
        return Ok(());
    }

    let image = client.clipboard_read_image(Some("wl-paste --type".to_string()))?;

    if let Some(requested) = args.mime
        && requested != image.mime
    {
        return Err(eyre::eyre!(
            "requested mime {} not currently available (got {})",
            requested,
            image.mime
        ));
    }

    std::io::stdout().write_all(&image.bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_list_types() {
        let args = parse_args(&["--list-types".to_string()]).unwrap();
        assert!(args.list_types);
    }

    #[test]
    fn parses_type_and_no_newline() {
        let args = parse_args(&[
            "--type".to_string(),
            "image/png".to_string(),
            "--no-newline".to_string(),
        ])
        .unwrap();
        assert_eq!(args.mime.as_deref(), Some("image/png"));
        assert!(args.no_newline);
    }
}
