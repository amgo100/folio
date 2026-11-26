//! The compiler pin (SPEC §2.4): running the version that `folio.toml`
//! pins from the toolchain cache, checking the pin, and `folio toolchain
//! install` (SPEC §12.1, §14.3).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use folio::diag::TextFiles;
use folio::toolchain::{self, Channel, Curl, InstallError, Toolchains};
use folio::{Diagnostic, Manifest, World};
use folio_syntax::{DiagSpan, Spanned};
use semver::Version;

use super::build::{error, print};
use super::project::{self, MANIFEST};
use super::{Color, FAILURE};

/// The environment variable that a `folio` that hands a build to the
/// pinned version sets, to its own version. The pinned version then does
/// not hand the build on again.
const DISPATCHED: &str = "FOLIO_DISPATCHED_BY";

/// The arguments of `folio toolchain`.
#[derive(Debug, Args)]
pub(crate) struct ToolchainArgs {
    #[command(subcommand)]
    command: ToolchainCommand,
}

#[derive(Debug, Subcommand)]
enum ToolchainCommand {
    /// Download a compiler version from the Folio release channel, verify
    /// its signature and install it into the toolchain cache
    Install(InstallArgs),
}

/// The arguments of `folio toolchain install`.
#[derive(Debug, Args)]
struct InstallArgs {
    /// The version, such as 1.2.0 [default: the version that the project's
    /// folio.toml pins]
    #[arg(value_name = "VERSION", value_parser = parse_version)]
    version: Option<Version>,

    /// The project directory, whose pin to install when no version is given
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// When to colour diagnostics
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = Color::Auto)]
    color: Color,
}

fn parse_version(text: &str) -> Result<Version, String> {
    Version::parse(text).map_err(|_| "expected an exact version, such as 1.2.0".into())
}

/// Runs `folio toolchain`.
pub(crate) fn toolchain(args: &ToolchainArgs) -> ExitCode {
    match &args.command {
        ToolchainCommand::Install(args) => install(args),
    }
}

/// Runs `folio toolchain install`.
fn install(args: &InstallArgs) -> ExitCode {
    let color = args.color.enabled();
    let fail = |message: String, hints: &[&str]| {
        let mut error = error(message);
        for hint in hints {
            error = error.with_hint(*hint);
        }
        print(&TextFiles::new(), &[error], color);
        ExitCode::from(FAILURE)
    };
    let version = match &args.version {
        Some(version) => version.clone(),
        None => match project_pin(args.root.as_deref()) {
            Ok(version) => version,
            Err(message) => {
                return fail(
                    message,
                    &["name the version: `folio toolchain install 1.2.0`"],
                );
            }
        },
    };
    let Some(dir) = Toolchains::default_dir() else {
        return fail(
            "there is no cache directory to install into".into(),
            &["set FOLIO_CACHE_DIR to a directory"],
        );
    };
    let toolchains = Toolchains::new(dir);
    match toolchain::install(&version, &Channel::official(), &Curl, &toolchains) {
        Ok(installed) => {
            if installed.already {
                eprintln!(
                    "Folio {version} is already installed at {}",
                    installed.path.display()
                );
            } else {
                eprintln!("installed Folio {version} at {}", installed.path.display());
            }
            let floor = toolchain::security_fixes_from(Some(&toolchains));
            if version < floor {
                let warning = Diagnostic::warning(
                    DiagSpan::detached(),
                    format!("Folio {version} no longer receives security fixes"),
                )
                .with_hint(format!("{floor} is the oldest version that does"));
                print(&TextFiles::new(), &[warning], color);
            }
            ExitCode::SUCCESS
        }
        Err(e @ InstallError::ChannelReplayed { serial, stored }) => {
            let what = if serial < stored {
                "lowered the serial"
            } else {
                "published other contents under the same serial"
            };
            let reset = format!(
                "if the Folio project says it {what}, delete `{}`",
                toolchains.stored_channel_path().display()
            );
            fail(
                e.to_string(),
                &[
                    "try again later or on another network; if it keeps happening, tell the \
                     Folio project",
                    &reset,
                ],
            )
        }
        Err(e) => fail(e.to_string(), &[]),
    }
}

/// The compiler version that the project's `folio.toml` pins.
fn project_pin(root: Option<&Path>) -> Result<Version, String> {
    let root = project::find_root(root)?;
    let text = fs::read_to_string(root.join(MANIFEST))
        .map_err(|e| format!("cannot read {MANIFEST}: {e}"))?;
    Manifest::compiler_pin(&text, World::MANIFEST)
        .map(|pin| pin.v)
        .ok_or_else(|| format!("{MANIFEST} pins no valid compiler version"))
}

/// Hands the command line (`args`, without the program name) to the
/// compiler version that a project's manifest pins, when the command builds,
/// checks, formats or watches a project, the pin is another version, it is
/// installed, and `--compiler current` was not given. Returns the exit
/// status of the pinned version, or `None` when this version runs the
/// command. A pinned version without `folio watch` is an error for `folio
/// watch`, which suggests `--compiler current`.
///
/// It runs before the arguments are parsed and reads only `--root`,
/// `--compiler` and `--color` from them, and only `project.compiler` from
/// the manifest, so that flags, keys or an edition that only the pinned
/// version knows still reach it. A pin this version cannot run is reported
/// by [`check`] once the manifest is parsed.
pub(crate) fn dispatch(args: &[OsString]) -> Option<ExitCode> {
    if !matches!(args.first()?.to_str()?, "build" | "check" | "fmt" | "watch") {
        return None;
    }
    let mut root = None;
    let mut current = false;
    let mut color = Color::Auto;
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        let Some(arg) = arg.to_str() else { continue };
        if arg == "--" {
            break;
        }
        let Some((flag, value)) = arg
            .strip_prefix("--")
            .map(|flag| match flag.split_once('=') {
                Some((flag, value)) => (flag, Some(OsString::from(value))),
                None => (flag, None),
            })
        else {
            continue;
        };
        if !matches!(flag, "root" | "compiler" | "color") {
            continue;
        }
        let Some(value) = value.or_else(|| rest.next().cloned()) else {
            break;
        };
        match flag {
            "root" => root = Some(PathBuf::from(value)),
            "compiler" => current = value == "current",
            _ => {
                color = match value.to_str() {
                    Some("always") => Color::Always,
                    Some("never") => Color::Never,
                    _ => Color::Auto,
                }
            }
        }
    }
    let root = project::find_root(root.as_deref()).ok()?;
    let manifest = fs::read_to_string(root.join(MANIFEST)).ok()?;
    dispatch_to_pin(&manifest, args, current, color.enabled())
}

/// [`dispatch`], given the manifest's text.
fn dispatch_to_pin(
    manifest: &str,
    args: &[OsString],
    current: bool,
    color: bool,
) -> Option<ExitCode> {
    let pin = Manifest::compiler_pin(manifest, World::MANIFEST)?;
    if current || pin.v.to_string() == folio::VERSION || std::env::var_os(DISPATCHED).is_some() {
        return None;
    }
    let toolchains = Toolchains::default_dir().map(Toolchains::new)?;
    let binary = toolchains.get(&pin.v)?;
    // The pinned version may predate what is known of security fixes.
    let floor = toolchain::security_fixes_from(Some(&toolchains));
    if pin.v < floor {
        let mut files = TextFiles::new();
        files.insert(World::MANIFEST, MANIFEST, manifest.to_owned());
        print(&files, &[unfixed(pin.span, &pin.v, &floor)], color);
    }
    let watch = args.first().is_some_and(|command| command == "watch");
    if watch && let Err(why) = has_watch(&binary) {
        if let Some(why) = why {
            return Some(cannot_run(&binary, &pin.v, &why, color));
        }
        let mut files = TextFiles::new();
        files.insert(World::MANIFEST, MANIFEST, manifest.to_owned());
        let error = Diagnostic::error(
            pin.span,
            format!(
                "the project pins compiler {}, which has no `folio watch`",
                pin.v
            ),
        )
        .with_hint(format!(
            "`folio watch --compiler current` previews and writes the outputs with Folio {} \
             instead; they may differ from what Folio {} builds",
            folio::VERSION,
            pin.v
        ));
        print(&files, &[error], color);
        return Some(ExitCode::from(FAILURE));
    }
    let mut command = Command::new(&binary);
    command.args(args).env(DISPATCHED, folio::VERSION);
    Some(run(command, &binary, &pin.v, color))
}

/// Whether a compiler binary has `folio watch`: `Ok` when it has, `Err(None)`
/// when it has not, and `Err(Some(_))` when it cannot be run or does not
/// answer within ten seconds. Versions before the command either lack it or
/// parse it with any arguments and reject it as not yet supported, so only
/// one that has it shows the help of `watch --port 0`.
fn has_watch(binary: &Path) -> Result<(), Option<std::io::Error>> {
    let mut child = Command::new(binary)
        .args(["watch", "--port", "0", "--help"])
        .env(DISPATCHED, folio::VERSION)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Some)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait().map_err(Some)? {
            Some(status) if status.success() => return Ok(()),
            Some(_) => return Err(None),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Some(std::io::Error::other(
                    "it did not answer `folio watch --help` within ten seconds",
                )));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Runs the pinned version in place of this process, where the platform
/// allows it, so that it receives signals as this process would.
#[cfg(unix)]
fn run(mut command: Command, binary: &Path, version: &Version, color: bool) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let error = command.exec();
    cannot_run(binary, version, &error, color)
}

/// Runs the pinned version and exits with its status.
#[cfg(not(unix))]
fn run(mut command: Command, binary: &Path, version: &Version, color: bool) -> ExitCode {
    match command.status() {
        Ok(status) => match status.code() {
            Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(FAILURE)),
            None => ExitCode::from(FAILURE),
        },
        Err(error) => cannot_run(binary, version, &error, color),
    }
}

fn cannot_run(binary: &Path, version: &Version, why: &std::io::Error, color: bool) -> ExitCode {
    let error = error(format!(
        "cannot run Folio {version} at `{}`: {why}",
        binary.display()
    ))
    .with_hint(format!(
        "`folio toolchain install {version}` installs it, after removing `{}`",
        binary.parent().unwrap_or(binary).display()
    ));
    print(&TextFiles::new(), &[error], color);
    ExitCode::from(FAILURE)
}

/// Checks the compiler version that the manifest pins, for a build that
/// this version runs (SPEC §2.4, §14.3): with another pin, the build fails,
/// as when the pinned version is not installed, unless `--compiler current`
/// asks for this one, which is a warning. A pin below the oldest version
/// that still receives security fixes is a warning.
pub(crate) fn check(manifest: &Manifest, current: bool) -> Option<Diagnostic> {
    check_version(&manifest.project.compiler, current)
}

/// [`check`], given the pin alone, as [`Manifest::compiler_pin`] reads it.
pub(crate) fn check_version(
    pinned: &Spanned<Version, DiagSpan>,
    current: bool,
) -> Option<Diagnostic> {
    let dispatched = std::env::var_os(DISPATCHED);
    let toolchains = Toolchains::default_dir().map(Toolchains::new);
    let floor = toolchain::security_fixes_from(toolchains.as_ref());
    check_pin(
        pinned,
        current,
        dispatched.as_ref(),
        toolchains.as_ref(),
        &floor,
    )
}

/// [`check`], given what it reads from the environment: the version that
/// handed the build to this one, the toolchain cache and the oldest version
/// that still receives security fixes.
fn check_pin(
    pinned: &Spanned<Version, DiagSpan>,
    current: bool,
    dispatched: Option<&OsString>,
    toolchains: Option<&Toolchains>,
    floor: &Version,
) -> Option<Diagnostic> {
    let version = folio::VERSION;
    if pinned.v.to_string() == version {
        // The version that handed the build on warned already.
        return (dispatched.is_none() && pinned.v < *floor)
            .then(|| unfixed(pinned.span, &pinned.v, floor));
    }
    if current {
        return Some(Diagnostic::warning(
            pinned.span,
            format!(
                "building with Folio {version}, not the pinned {}: the output may differ",
                pinned.v
            ),
        ));
    }
    let error = Diagnostic::error(
        pinned.span,
        format!(
            "the project pins compiler {}, but this is Folio {version}",
            pinned.v
        ),
    );
    Some(match (dispatched, toolchains) {
        (Some(by), Some(toolchains)) => error
            .with_hint(format!(
                "Folio {} ran this binary, which `{}` holds for {}",
                by.to_string_lossy(),
                toolchains.binary(&pinned.v).display(),
                pinned.v
            ))
            .with_hint(format!(
                "remove that directory and run `folio toolchain install {}`, or unset \
                 {DISPATCHED} if it was not set by Folio",
                pinned.v
            )),
        (_, None) => error
            .with_hint("there is no toolchain cache to find it in")
            .with_hint("set FOLIO_CACHE_DIR to the directory that holds `toolchains`"),
        (None, Some(_)) => error
            .with_hint(format!(
                "Folio {} is not installed: `folio toolchain install {}` installs it",
                pinned.v, pinned.v
            ))
            .with_hint("`--compiler current` builds with this version instead"),
    })
}

/// The warning that a pinned version no longer receives security fixes.
fn unfixed(span: DiagSpan, pinned: &Version, floor: &Version) -> Diagnostic {
    Diagnostic::warning(
        span,
        format!("compiler {pinned} no longer receives security fixes"),
    )
    .with_hint(format!(
        "{floor} is the oldest version that does; pinning a newer version may change the output"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
        compiler = \"0.0.0\"\n";

    fn checked(pin: &str, current: bool, dispatched: bool, floor: &str) -> Option<Diagnostic> {
        let world = World::new(&MANIFEST.replace("0.0.0", pin)).unwrap();
        let toolchains = Toolchains::new("/cache/toolchains");
        let by = OsString::from("9.9.9");
        check_pin(
            &world.manifest().project.compiler,
            current,
            dispatched.then_some(&by),
            Some(&toolchains),
            &Version::parse(floor).unwrap(),
        )
    }

    #[test]
    fn checks_the_pin() {
        assert_eq!(checked(folio::VERSION, false, false, "0.0.0"), None);

        let missing = checked("99.0.0", false, false, "0.0.0").unwrap();
        assert!(missing.is_error());
        assert!(
            missing
                .hints
                .iter()
                .any(|h| h.contains("`folio toolchain install 99.0.0`")),
            "{missing:?}"
        );

        let current = checked("99.0.0", true, false, "0.0.0").unwrap();
        assert!(!current.is_error());

        // An installed binary that is not the version it is installed as.
        let damaged = checked("99.0.0", false, true, "0.0.0").unwrap();
        assert!(damaged.is_error());
        assert!(damaged.hints[0].contains("Folio 9.9.9 ran this binary"));
    }

    #[test]
    fn warns_of_pins_without_security_fixes() {
        let old = checked(folio::VERSION, false, false, "100.0.0").unwrap();
        assert!(!old.is_error());
        assert!(old.message.contains("no longer receives security fixes"));
        assert!(old.hints[0].contains("100.0.0"));
        // The version that handed the build on warned already.
        assert_eq!(checked(folio::VERSION, false, true, "100.0.0"), None);
    }
}
