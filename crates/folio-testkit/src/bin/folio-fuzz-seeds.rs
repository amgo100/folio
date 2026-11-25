//! Writes the seed corpus of the fuzz targets
//! ([`folio_testkit::fuzz::seeds`]) into a directory, one subdirectory per
//! target: `folio-fuzz-seeds <dir>`. `scripts/fuzz.sh` runs it.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::{env, fs};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let (Some(dir), None) = (args.next(), args.next()) else {
        eprintln!("usage: folio-fuzz-seeds <dir>");
        return ExitCode::from(2);
    };
    let dir = PathBuf::from(dir);
    let seeds = folio_testkit::fuzz::seeds();
    for (target, name, bytes) in &seeds {
        let path = dir.join(target).join(name);
        let written = fs::create_dir_all(dir.join(target)).and_then(|()| fs::write(&path, bytes));
        if let Err(e) = written {
            eprintln!("cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    }
    println!("wrote {} seeds to {}", seeds.len(), dir.display());
    ExitCode::SUCCESS
}
