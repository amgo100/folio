//! The commands of SPEC §12.1 and their arguments. `folio build`, `folio
//! check`, `folio fmt`, `folio lsp` and `folio toolchain install` are
//! implemented; the other
//! commands, and the flags of `folio build` and `folio check` for later
//! features, are parsed and rejected as not yet supported.

mod build;
mod check;
mod fmt;
mod lsp;
mod project;
mod toolchain;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use folio::{Date, Target};

/// The exit code of a build or check that found errors.
const FAILURE: u8 = 1;
/// The exit code of a command line that cannot be run: arguments that do
/// not parse, or a command or flag that is not supported yet. clap exits
/// with it too.
const USAGE: u8 = 2;

/// Folio compiles one semantic source into PDF, DOCX, PPTX and live
/// presentations.
#[derive(Debug, Parser)]
#[command(name = "folio", version = folio::VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Build outputs: every variant and target the manifest declares, or
    /// those chosen
    Build(BuildArgs),
    /// Create a project from a template or package (not yet supported)
    New(Later),
    /// Rebuild incrementally and serve the live preview (not yet supported)
    Watch(Later),
    /// Run the live presentation with presenter view (not yet supported)
    Present(Later),
    /// Report diagnostics without writing output: every variant and target
    /// the manifest declares, or those chosen
    Check(CheckArgs),
    /// Format the project's `.fol` and `.fsty` files canonically
    Fmt(FmtArgs),
    /// Move a local override into a stylesheet (not yet supported)
    Promote(Later),
    /// Print elements as JSON (not yet supported)
    Query(Later),
    /// List the available components (not yet supported)
    Components(Later),
    /// Run the language server over standard input and output
    Lsp(lsp::LspArgs),
    /// Manage the compiler versions that projects pin
    Toolchain(toolchain::ToolchainArgs),
}

impl Command {
    /// The command's name, for a command that is not supported yet.
    fn later(&self) -> Option<&'static str> {
        Some(match self {
            Self::Build(_)
            | Self::Check(_)
            | Self::Fmt(_)
            | Self::Lsp(_)
            | Self::Toolchain(_) => return None,
            Self::New(_) => "new",
            Self::Watch(_) => "watch",
            Self::Present(_) => "present",
            Self::Promote(_) => "promote",
            Self::Query(_) => "query",
            Self::Components(_) => "components",
        })
    }
}

/// The arguments of a command that is not supported yet, whatever they
/// are.
#[derive(Debug, Args)]
struct Later {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
    _args: Vec<OsString>,
}

/// The arguments that `folio build` and `folio check` share: which builds,
/// of which project, and how to report.
#[derive(Debug, Args)]
pub(crate) struct ProjectArgs {
    /// Only this variant; repeat for several
    #[arg(long = "variant", value_name = "VARIANT")]
    pub(crate) variants: Vec<String>,

    /// Only this target; repeat for several
    #[arg(long = "target", value_name = "TARGET", value_parser = parse_target)]
    pub(crate) targets: Vec<Target>,

    /// The date that scripts see as `today`, such as 2026-11-24; it
    /// replaces `project.today` of folio.toml
    #[arg(long, value_name = "DATE", value_parser = parse_date)]
    pub(crate) today: Option<Date>,

    /// The project directory, which holds folio.toml [default: the current
    /// directory or the nearest one above it that holds folio.toml]
    #[arg(long, value_name = "DIR")]
    pub(crate) root: Option<PathBuf>,

    /// Use this compiler instead of the version folio.toml pins; `current`
    /// is the running one
    #[arg(long, value_name = "VERSION", value_parser = ["current"])]
    pub(crate) compiler: Option<String>,

    /// When to colour diagnostics
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = Color::Auto)]
    pub(crate) color: Color,

    /// How to write diagnostics to standard error: `human` with source
    /// excerpts, or `json` as one document when the command ends
    #[arg(
        long,
        value_enum,
        value_name = "FORMAT",
        default_value_t = DiagnosticsFormat::Human
    )]
    pub(crate) diagnostics: DiagnosticsFormat,
}

/// The arguments of `folio build`.
#[derive(Debug, Args)]
pub(crate) struct BuildArgs {
    #[command(flatten)]
    pub(crate) project: ProjectArgs,

    /// Force strict mode for this build, or with `report` list violations
    /// (not yet supported)
    #[arg(
        long,
        value_name = "report",
        num_args = 0..=1,
        require_equals = true,
        value_parser = ["report"],
    )]
    pub(crate) strict: Option<Option<String>>,

    /// Require alt text (not yet supported)
    #[arg(long)]
    pub(crate) strict_a11y: bool,
}

impl BuildArgs {
    /// The first flag given that is not supported yet.
    fn later(&self) -> Option<&'static str> {
        if self.strict.is_some() {
            Some("--strict")
        } else if self.strict_a11y {
            Some("--strict-a11y")
        } else {
            None
        }
    }
}

/// The arguments of `folio check`.
#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    #[command(flatten)]
    pub(crate) project: ProjectArgs,

    /// Also report the softer layout problems: underfull lines, widows
    /// and orphans, keeps and unbreakable blocks that a page break split
    /// (overfull lines and content too tall for a page are always
    /// reported)
    #[arg(long)]
    pub(crate) layout: bool,

    /// Also check that content hidden from a variant does not reach its
    /// outputs (not yet supported)
    #[arg(long)]
    pub(crate) leaks: bool,

    /// List the local overrides (not yet supported)
    #[arg(long, value_name = "WHAT", value_parser = ["overrides"])]
    pub(crate) report: Option<String>,
}

impl CheckArgs {
    /// The first flag given that is not supported yet.
    fn later(&self) -> Option<&'static str> {
        if self.leaks {
            Some("--leaks")
        } else if self.report.is_some() {
            Some("--report")
        } else {
            None
        }
    }
}

/// The arguments of `folio fmt`.
#[derive(Debug, Args)]
pub(crate) struct FmtArgs {
    /// Change no file: report each file that is not formatted, with the
    /// edits that would format it, and exit with status 1 if there is one
    #[arg(long)]
    pub(crate) check: bool,

    /// The project directory, which holds folio.toml [default: the current
    /// directory or the nearest one above it that holds folio.toml]
    #[arg(long, value_name = "DIR")]
    pub(crate) root: Option<PathBuf>,

    /// Use this compiler instead of the version folio.toml pins; `current`
    /// is the running one
    #[arg(long, value_name = "VERSION", value_parser = ["current"])]
    pub(crate) compiler: Option<String>,

    /// When to colour diagnostics
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = Color::Auto)]
    pub(crate) color: Color,

    /// How to write diagnostics to standard error: `human` with source
    /// excerpts, or `json` as one document when the command ends
    #[arg(
        long,
        value_enum,
        value_name = "FORMAT",
        default_value_t = DiagnosticsFormat::Human
    )]
    pub(crate) diagnostics: DiagnosticsFormat,
}

/// When to colour diagnostics.
#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
pub(crate) enum Color {
    /// When standard error is a terminal and `NO_COLOR` is not set
    Auto,
    /// Always
    Always,
    /// Never
    Never,
}

impl Color {
    /// Whether to colour what is written to standard error.
    pub(crate) fn enabled(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                std::io::stderr().is_terminal()
                    && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
            }
        }
    }
}

/// How diagnostics are written.
#[derive(Debug, Copy, Clone, Eq, PartialEq, ValueEnum)]
pub(crate) enum DiagnosticsFormat {
    /// For people: source excerpts, as they are found
    Human,
    /// For programs: one JSON document, described in
    /// docs/diagnostics-json.md, when the command ends
    Json,
}

/// A target as the manifest writes it.
fn parse_target(text: &str) -> Result<Target, String> {
    Target::from_name(text).ok_or_else(|| {
        let names: Vec<_> = Target::ALL.iter().map(|t| t.as_str()).collect();
        format!("the targets are {}", names.join(", "))
    })
}

/// A date as TOML writes it.
fn parse_date(text: &str) -> Result<Date, String> {
    Date::parse(text).ok_or_else(|| "expected a date such as 2026-11-24".into())
}

/// Runs the command line.
pub(crate) fn run() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    // Before the arguments are parsed, so that flags only the pinned
    // version knows reach it.
    if let Some(status) = toolchain::dispatch(args.get(1..).unwrap_or_default()) {
        return status;
    }
    let cli = Cli::parse_from(args);
    let later = match &cli.command {
        Command::Build(args) => args.later().map(|flag| format!("`folio build {flag}`")),
        Command::Check(args) => args.later().map(|flag| format!("`folio check {flag}`")),
        command => command.later().map(|name| format!("`folio {name}`")),
    };
    if let Some(what) = later {
        eprintln!("error: {what} is not yet supported");
        return ExitCode::from(USAGE);
    }
    match cli.command {
        Command::Build(args) => build::build(&args.project),
        Command::Check(args) => check::check(&args.project, args.layout),
        Command::Fmt(args) => fmt::fmt(&args),
        Command::Lsp(args) => lsp::lsp(&args),
        Command::Toolchain(args) => toolchain::toolchain(&args),
        _ => unreachable!("every other command is rejected above"),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn arguments_are_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn build_flags() {
        let parse = |args: &[&str]| {
            let mut all = vec!["folio", "build"];
            all.extend(args);
            match Cli::try_parse_from(all).map(|cli| cli.command) {
                Ok(Command::Build(args)) => Ok(args),
                Ok(other) => panic!("{other:?}"),
                Err(error) => Err(error.kind()),
            }
        };
        let args = parse(&[
            "--variant",
            "exam",
            "--variant",
            "markscheme",
            "--target",
            "pdf",
            "--today",
            "2026-11-24",
        ])
        .unwrap();
        assert_eq!(args.project.variants, ["exam", "markscheme"]);
        assert_eq!(args.project.targets, [Target::Pdf]);
        assert_eq!(args.project.today.unwrap().to_string(), "2026-11-24");
        assert_eq!(args.later(), None);

        assert!(parse(&["--target", "html"]).is_err());
        assert!(parse(&["--today", "2026-02-30"]).is_err());
        assert!(parse(&["--compiler", "1.2.0"]).is_err());
        assert_eq!(parse(&["--compiler", "current"]).unwrap().later(), None);

        assert_eq!(parse(&["--strict"]).unwrap().later(), Some("--strict"));
        assert_eq!(
            parse(&["--strict=report"]).unwrap().later(),
            Some("--strict")
        );
        assert!(parse(&["--strict=lax"]).is_err());
        assert_eq!(
            parse(&["--strict-a11y"]).unwrap().later(),
            Some("--strict-a11y")
        );
    }

    #[test]
    fn check_flags() {
        let parse = |args: &[&str]| {
            let mut all = vec!["folio", "check"];
            all.extend(args);
            match Cli::try_parse_from(all).map(|cli| cli.command) {
                Ok(Command::Check(args)) => Ok(args),
                Ok(other) => panic!("{other:?}"),
                Err(error) => Err(error.kind()),
            }
        };
        let args = parse(&[
            "--variant",
            "exam",
            "--target",
            "pdf",
            "--diagnostics",
            "json",
        ])
        .unwrap();
        assert_eq!(args.project.variants, ["exam"]);
        assert_eq!(args.project.targets, [Target::Pdf]);
        assert_eq!(args.project.diagnostics, DiagnosticsFormat::Json);
        assert_eq!(args.later(), None);
        assert_eq!(parse(&["--layout"]).unwrap().later(), None);
        assert_eq!(parse(&["--leaks"]).unwrap().later(), Some("--leaks"));
        assert_eq!(
            parse(&["--report", "overrides"]).unwrap().later(),
            Some("--report")
        );
        assert!(parse(&["--report", "fonts"]).is_err());
        assert!(parse(&["--strict"]).is_err());
    }

    #[test]
    fn fmt_flags() {
        let parse = |args: &[&str]| {
            let mut all = vec!["folio", "fmt"];
            all.extend(args);
            match Cli::try_parse_from(all).map(|cli| cli.command) {
                Ok(Command::Fmt(args)) => Ok(args),
                Ok(other) => panic!("{other:?}"),
                Err(error) => Err(error.kind()),
            }
        };
        let args = parse(&[]).unwrap();
        assert!(!args.check);
        assert_eq!(args.diagnostics, DiagnosticsFormat::Human);
        let args = parse(&[
            "--check",
            "--root",
            "dir",
            "--diagnostics",
            "json",
            "--compiler",
            "current",
        ])
        .unwrap();
        assert_eq!(args.compiler.as_deref(), Some("current"));
        assert!(args.check);
        assert_eq!(args.root.as_deref(), Some(std::path::Path::new("dir")));
        assert_eq!(args.diagnostics, DiagnosticsFormat::Json);
        assert!(parse(&["--variant", "exam"]).is_err());
        assert_eq!(
            Cli::try_parse_from(["folio", "fmt"])
                .unwrap()
                .command
                .later(),
            None
        );
    }

    #[test]
    fn later_commands_take_any_arguments() {
        let cli = Cli::try_parse_from(["folio", "watch"]);
        assert_eq!(cli.unwrap().command.later(), Some("watch"));
    }
}
