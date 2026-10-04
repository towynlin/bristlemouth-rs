//! `bm-image`: see `bm-image/README.md`.

use std::process::ExitCode;
use std::{env, fs};

use bm_image::{Info, Key};

const USAGE: &str = "\
usage: bm-image dfu <elf> [--key <ed25519.pem>] -o <out.dfu.bin>
       bm-image unified <bootloader> <dfu.bin> -o <out.unified.bin>
       bm-image info <file>";

/// Positional arguments, and the values of `-o` and `--key`.
struct Args {
    positional: Vec<String>,
    out: Option<String>,
    key: Option<String>,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        positional: Vec::new(),
        out: None,
        key: None,
    };
    let mut args = args;
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "-o" => &mut parsed.out,
            "--key" => &mut parsed.key,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            _ => {
                parsed.positional.push(arg);
                continue;
            }
        };
        *slot = Some(args.next().ok_or(format!("{arg} needs a value"))?);
    }
    Ok(parsed)
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{path}: {e}"))
}

fn write(path: &str, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("{path}: {e}"))
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or(USAGE)?;
    let args = parse(args)?;
    let positional: Vec<&str> = args.positional.iter().map(String::as_str).collect();
    match (command.as_str(), positional.as_slice(), &args.out) {
        ("dfu", [elf], Some(out)) => {
            let key = match &args.key {
                Some(path) => {
                    let pem = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
                    Some(Key::from_pem(&pem).map_err(|e| format!("{path}: {e}"))?)
                }
                None => None,
            };
            let image =
                bm_image::dfu(&read(elf)?, key.as_ref()).map_err(|e| format!("{elf}: {e}"))?;
            write(out, &image)
        }
        ("unified", [bootloader, dfu], Some(out)) if args.key.is_none() => {
            let image =
                bm_image::unified(&read(bootloader)?, &read(dfu)?).map_err(|e| e.to_string())?;
            write(out, &image)
        }
        ("info", [file], None) if args.key.is_none() => {
            let info = Info::read(&read(file)?).map_err(|e| format!("{file}: {e}"))?;
            print!("{info}");
            Ok(())
        }
        _ => Err(USAGE.into()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
