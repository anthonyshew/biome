mod diagnostics;
mod migrate;
mod process_file;
mod std_in;
pub(crate) mod traverse;

use crate::cli_options::{CliOptions, CliReporter};
use crate::commands::MigrateSubCommand;
use crate::diagnostics::ReportDiagnostic;
use crate::execute::migrate::MigratePayload;
use crate::execute::traverse::{traverse, TraverseResult};
use crate::reporter::github::{GithubReporter, GithubReporterVisitor};
use crate::reporter::gitlab::{GitLabReporter, GitLabReporterVisitor};
use crate::reporter::json::{JsonReporter, JsonReporterVisitor};
use crate::reporter::junit::{JunitReporter, JunitReporterVisitor};
use crate::reporter::summary::{SummaryReporter, SummaryReporterVisitor};
use crate::reporter::terminal::{ConsoleReporter, ConsoleReporterVisitor};
use crate::{CliDiagnostic, CliSession, DiagnosticsPayload, Reporter};
use biome_configuration::analyzer::RuleSelector;
use biome_console::{markup, ConsoleExt};
use biome_diagnostics::adapters::SerdeJsonError;
use biome_diagnostics::{category, Category};
use biome_fs::BiomePath;
use biome_service::workspace::{
    FeatureName, FeaturesBuilder, FixFileMode, FormatFileParams, OpenFileParams, PatternId,
};
use std::borrow::Borrow;
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use tracing::info;

/// Useful information during the traversal of files and virtual content
#[derive(Debug, Clone)]
pub struct Execution {
    /// How the information should be collected and reported
    report_mode: ReportMode,

    /// The modality of execution of the traversal
    traversal_mode: TraversalMode,

    /// The maximum number of diagnostics that can be printed in console
    max_diagnostics: u32,
}

impl Execution {
    pub fn new_format(vcs_targeted: VcsTargeted) -> Self {
        Self {
            traversal_mode: TraversalMode::Format {
                ignore_errors: false,
                write: false,
                stdin: None,
                vcs_targeted,
            },
            report_mode: ReportMode::default(),
            max_diagnostics: 0,
        }
    }

    pub fn report_mode(&self) -> &ReportMode {
        &self.report_mode
    }
}

impl Execution {
    pub(crate) fn to_feature(&self) -> FeatureName {
        match self.traversal_mode {
            TraversalMode::Format { .. } => FeaturesBuilder::new().with_formatter().build(),
            TraversalMode::Lint { .. } => FeaturesBuilder::new().with_linter().build(),
            TraversalMode::Check { .. } | TraversalMode::CI { .. } => FeaturesBuilder::new()
                .with_organize_imports()
                .with_formatter()
                .with_linter()
                .with_assists()
                .build(),
            TraversalMode::Migrate { .. } => FeatureName::empty(),
            TraversalMode::Search { .. } => FeaturesBuilder::new().with_search().build(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ExecutionEnvironment {
    GitHub,
}

/// A type that holds the information to execute the CLI via `stdin
#[derive(Debug, Clone)]
pub struct Stdin(
    /// The virtual path to the file
    PathBuf,
    /// The content of the file
    String,
);

impl Stdin {
    fn as_path(&self) -> &Path {
        self.0.as_path()
    }

    fn as_content(&self) -> &str {
        self.1.as_str()
    }
}

impl From<(PathBuf, String)> for Stdin {
    fn from((path, content): (PathBuf, String)) -> Self {
        Self(path, content)
    }
}

#[derive(Debug, Clone)]
pub struct VcsTargeted {
    pub staged: bool,
    pub changed: bool,
}

#[derive(Debug, Clone)]
pub enum TraversalMode {
    /// This mode is enabled when running the command `biome check`
    Check {
        /// The type of fixes that should be applied when analyzing a file.
        ///
        /// It's [None] if the `check` command is called without `--apply` or `--apply-suggested`
        /// arguments.
        fix_file_mode: Option<FixFileMode>,
        /// An optional tuple.
        /// 1. The virtual path to the file
        /// 2. The content of the file
        stdin: Option<Stdin>,
        /// A flag to know vcs integrated options such as `--staged` or `--changed` are enabled
        vcs_targeted: VcsTargeted,
    },
    /// This mode is enabled when running the command `biome lint`
    Lint {
        /// The type of fixes that should be applied when analyzing a file.
        ///
        /// It's [None] if the `check` command is called without `--apply` or `--apply-suggested`
        /// arguments.
        fix_file_mode: Option<FixFileMode>,
        /// An optional tuple.
        /// 1. The virtual path to the file
        /// 2. The content of the file
        stdin: Option<Stdin>,
        /// Run only the given rule or group of rules.
        /// If the severity level of a rule is `off`,
        /// then the severity level of the rule is set to `error` if it is a recommended rule or `warn` otherwise.
        only: Vec<RuleSelector>,
        /// Skip the given rule or group of rules by setting the severity level of the rules to `off`.
        /// This option takes precedence over `--only`.
        skip: Vec<RuleSelector>,
        /// A flag to know vcs integrated options such as `--staged` or `--changed` are enabled
        vcs_targeted: VcsTargeted,
        /// Supress existing diagnostics with a `// biome-ignore` comment
        suppress: bool,
    },
    /// This mode is enabled when running the command `biome ci`
    CI {
        /// Whether the CI is running in a specific environment, e.g. GitHub, GitLab, etc.
        environment: Option<ExecutionEnvironment>,
        /// A flag to know vcs integrated options such as `--staged` or `--changed` are enabled
        vcs_targeted: VcsTargeted,
    },
    /// This mode is enabled when running the command `biome format`
    Format {
        /// It ignores parse errors
        ignore_errors: bool,
        /// It writes the new content on file
        write: bool,
        /// An optional tuple.
        /// 1. The virtual path to the file
        /// 2. The content of the file
        stdin: Option<Stdin>,
        /// A flag to know vcs integrated options such as `--staged` or `--changed` are enabled
        vcs_targeted: VcsTargeted,
    },
    /// This mode is enabled when running the command `biome migrate`
    Migrate {
        /// Write result to disk
        write: bool,
        /// The path to `biome.json`
        configuration_file_path: PathBuf,
        /// The path directory where `biome.json` is placed
        configuration_directory_path: PathBuf,
        sub_command: Option<MigrateSubCommand>,
    },
    /// This mode is enabled when running the command `biome search`
    Search {
        /// The GritQL pattern to search for.
        ///
        /// Note that the search command does not support rewrites.
        pattern: PatternId,

        /// An optional tuple.
        /// 1. The virtual path to the file
        /// 2. The content of the file
        stdin: Option<Stdin>,
    },
}

impl Display for TraversalMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TraversalMode::Check { .. } => write!(f, "check"),
            TraversalMode::CI { .. } => write!(f, "ci"),
            TraversalMode::Format { .. } => write!(f, "format"),
            TraversalMode::Migrate { .. } => write!(f, "migrate"),
            TraversalMode::Lint { .. } => write!(f, "lint"),
            TraversalMode::Search { .. } => write!(f, "search"),
        }
    }
}

/// Tells to the execution of the traversal how the information should be reported
#[derive(Copy, Clone, Debug)]
pub enum ReportMode {
    /// Reports information straight to the console, it's the default mode
    Terminal { with_summary: bool },
    /// Reports information in JSON format
    Json { pretty: bool },
    /// Reports information for GitHub
    GitHub,
    /// JUnit output
    /// Ref: https://github.com/testmoapp/junitxml?tab=readme-ov-file#basic-junit-xml-structure
    Junit,
    /// Reports information in the [GitLab Code Quality](https://docs.gitlab.com/ee/ci/testing/code_quality.html#implement-a-custom-tool) format.
    GitLab,
}

impl Default for ReportMode {
    fn default() -> Self {
        Self::Terminal {
            with_summary: false,
        }
    }
}

impl From<CliReporter> for ReportMode {
    fn from(value: CliReporter) -> Self {
        match value {
            CliReporter::Default => Self::Terminal {
                with_summary: false,
            },
            CliReporter::Summary => Self::Terminal { with_summary: true },
            CliReporter::Json => Self::Json { pretty: false },
            CliReporter::JsonPretty => Self::Json { pretty: true },
            CliReporter::GitHub => Self::GitHub,
            CliReporter::Junit => Self::Junit,
            CliReporter::GitLab => Self::GitLab {},
        }
    }
}

impl Execution {
    pub(crate) fn new(mode: TraversalMode) -> Self {
        Self {
            report_mode: ReportMode::default(),
            traversal_mode: mode,
            max_diagnostics: 20,
        }
    }

    pub(crate) fn new_ci(vcs_targeted: VcsTargeted) -> Self {
        // Ref: https://docs.github.com/actions/learn-github-actions/variables#default-environment-variables
        let is_github = std::env::var("GITHUB_ACTIONS")
            .ok()
            .map_or(false, |value| value == "true");

        Self {
            report_mode: ReportMode::default(),
            traversal_mode: TraversalMode::CI {
                environment: if is_github {
                    Some(ExecutionEnvironment::GitHub)
                } else {
                    None
                },
                vcs_targeted,
            },
            max_diagnostics: 20,
        }
    }

    /// It sets the reporting mode by reading the [CliOptions]
    pub(crate) fn set_report(mut self, cli_options: &CliOptions) -> Self {
        self.report_mode = cli_options.reporter.clone().into();
        self
    }

    pub(crate) fn traversal_mode(&self) -> &TraversalMode {
        &self.traversal_mode
    }

    pub(crate) fn get_max_diagnostics(&self) -> u32 {
        self.max_diagnostics
    }

    /// `true` only when running the traversal in [TraversalMode::Check] and `should_fix` is `true`
    pub(crate) fn as_fix_file_mode(&self) -> Option<&FixFileMode> {
        match &self.traversal_mode {
            TraversalMode::Check { fix_file_mode, .. }
            | TraversalMode::Lint { fix_file_mode, .. } => fix_file_mode.as_ref(),
            TraversalMode::Format { .. }
            | TraversalMode::CI { .. }
            | TraversalMode::Migrate { .. }
            | TraversalMode::Search { .. } => None,
        }
    }

    pub(crate) fn as_write_suppressions_mode(&self) -> bool {
        match &self.traversal_mode {
            TraversalMode::Lint { suppress, .. } => *suppress,
            TraversalMode::Check { .. }
            | TraversalMode::Format { .. }
            | TraversalMode::CI { .. }
            | TraversalMode::Migrate { .. }
            | TraversalMode::Search { .. } => false,
        }
    }

    pub(crate) fn as_diagnostic_category(&self) -> &'static Category {
        match self.traversal_mode {
            TraversalMode::Check { .. } => category!("check"),
            TraversalMode::Lint { .. } => category!("lint"),
            TraversalMode::CI { .. } => category!("ci"),
            TraversalMode::Format { .. } => category!("format"),
            TraversalMode::Migrate { .. } => category!("migrate"),
            TraversalMode::Search { .. } => category!("search"),
        }
    }

    pub(crate) const fn is_ci(&self) -> bool {
        matches!(self.traversal_mode, TraversalMode::CI { .. })
    }

    pub(crate) const fn is_search(&self) -> bool {
        matches!(self.traversal_mode, TraversalMode::Search { .. })
    }

    pub(crate) const fn is_check(&self) -> bool {
        matches!(self.traversal_mode, TraversalMode::Check { .. })
    }

    pub(crate) const fn is_lint(&self) -> bool {
        matches!(self.traversal_mode, TraversalMode::Lint { .. })
    }

    pub(crate) const fn is_check_apply(&self) -> bool {
        matches!(
            self.traversal_mode,
            TraversalMode::Check {
                fix_file_mode: Some(FixFileMode::SafeFixes),
                ..
            }
        )
    }

    pub(crate) const fn is_check_apply_unsafe(&self) -> bool {
        matches!(
            self.traversal_mode,
            TraversalMode::Check {
                fix_file_mode: Some(FixFileMode::SafeAndUnsafeFixes),
                ..
            }
        )
    }

    pub(crate) const fn is_format(&self) -> bool {
        matches!(self.traversal_mode, TraversalMode::Format { .. })
    }

    pub(crate) const fn is_format_write(&self) -> bool {
        if let TraversalMode::Format { write, .. } = self.traversal_mode {
            write
        } else {
            false
        }
    }

    /// Whether the traversal mode requires write access to files
    pub(crate) const fn requires_write_access(&self) -> bool {
        match self.traversal_mode {
            TraversalMode::Check { fix_file_mode, .. }
            | TraversalMode::Lint { fix_file_mode, .. } => fix_file_mode.is_some(),
            TraversalMode::CI { .. } | TraversalMode::Search { .. } => false,
            TraversalMode::Format { write, .. } | TraversalMode::Migrate { write, .. } => write,
        }
    }

    pub(crate) fn as_stdin_file(&self) -> Option<&Stdin> {
        match &self.traversal_mode {
            TraversalMode::Format { stdin, .. }
            | TraversalMode::Lint { stdin, .. }
            | TraversalMode::Check { stdin, .. }
            | TraversalMode::Search { stdin, .. } => stdin.as_ref(),
            TraversalMode::CI { .. } | TraversalMode::Migrate { .. } => None,
        }
    }

    pub(crate) fn is_vcs_targeted(&self) -> bool {
        match &self.traversal_mode {
            TraversalMode::Check { vcs_targeted, .. }
            | TraversalMode::Lint { vcs_targeted, .. }
            | TraversalMode::Format { vcs_targeted, .. }
            | TraversalMode::CI { vcs_targeted, .. } => vcs_targeted.staged || vcs_targeted.changed,
            TraversalMode::Migrate { .. } | TraversalMode::Search { .. } => false,
        }
    }

    /// Returns [true] if the user used the `--write`/`--fix` option
    pub(crate) fn is_write(&self) -> bool {
        match self.traversal_mode {
            TraversalMode::Check { fix_file_mode, .. } => fix_file_mode.is_some(),
            TraversalMode::Lint { fix_file_mode, .. } => fix_file_mode.is_some(),
            TraversalMode::CI { .. } => false,
            TraversalMode::Format { write, .. } => write,
            TraversalMode::Migrate { write, .. } => write,
            TraversalMode::Search { .. } => false,
        }
    }
}

/// Based on the [mode](TraversalMode), the function might launch a traversal of the file system
/// or handles the stdin file.
pub fn execute_mode(
    mut execution: Execution,
    mut session: CliSession,
    cli_options: &CliOptions,
    paths: Vec<OsString>,
) -> Result<(), CliDiagnostic> {
    // If a custom reporter was provided, let's lift the limit so users can see all of them
    execution.max_diagnostics = if cli_options.reporter.is_default() {
        cli_options.max_diagnostics.into()
    } else {
        info!("Removing the limit of --max-diagnostics, because of a reporter different from the default one: {}", cli_options.reporter);
        u32::MAX
    };

    // don't do any traversal if there's some content coming from stdin
    if let Some(stdin) = execution.as_stdin_file() {
        let biome_path = BiomePath::new(stdin.as_path());
        std_in::run(
            session,
            &execution,
            biome_path,
            stdin.as_content(),
            cli_options.verbose,
        )
    } else if let TraversalMode::Migrate {
        write,
        configuration_file_path,
        configuration_directory_path,
        sub_command,
    } = execution.traversal_mode
    {
        let payload = MigratePayload {
            session,
            write,
            configuration_file_path,
            configuration_directory_path,
            verbose: cli_options.verbose,
            sub_command,
        };
        migrate::run(payload)
    } else {
        let TraverseResult {
            summary,
            evaluated_paths,
            diagnostics,
        } = traverse(&execution, &mut session, cli_options, paths)?;
        let console = session.app.console;
        let errors = summary.errors;
        let skipped = summary.skipped;
        let processed = summary.changed + summary.unchanged;
        let should_exit_on_warnings = summary.warnings > 0 && cli_options.error_on_warnings;

        match execution.report_mode {
            ReportMode::Terminal { with_summary } => {
                if with_summary {
                    let reporter = SummaryReporter {
                        summary,
                        diagnostics_payload: DiagnosticsPayload {
                            verbose: cli_options.verbose,
                            diagnostic_level: cli_options.diagnostic_level,
                            diagnostics,
                        },
                        execution: execution.clone(),
                    };
                    reporter.write(&mut SummaryReporterVisitor(console))?;
                } else {
                    let reporter = ConsoleReporter {
                        summary,
                        diagnostics_payload: DiagnosticsPayload {
                            verbose: cli_options.verbose,
                            diagnostic_level: cli_options.diagnostic_level,
                            diagnostics,
                        },
                        execution: execution.clone(),
                        evaluated_paths,
                    };
                    reporter.write(&mut ConsoleReporterVisitor(console))?;
                }
            }
            ReportMode::Json { pretty } => {
                console.error(markup!{
                    <Warn>"The "<Emphasis>"--json"</Emphasis>" option is "<Underline>"unstable/experimental"</Underline>" and its output might change between patches/minor releases."</Warn>
                });
                let reporter = JsonReporter {
                    summary,
                    diagnostics: DiagnosticsPayload {
                        verbose: cli_options.verbose,
                        diagnostic_level: cli_options.diagnostic_level,
                        diagnostics,
                    },
                    execution: execution.clone(),
                };
                let mut buffer = JsonReporterVisitor::new(summary);
                reporter.write(&mut buffer)?;
                if pretty {
                    let content = serde_json::to_string(&buffer).map_err(|error| {
                        CliDiagnostic::Report(ReportDiagnostic::Serialization(
                            SerdeJsonError::from(error),
                        ))
                    })?;
                    let report_file = BiomePath::new("_report_output.json");
                    session.app.workspace.open_file(OpenFileParams {
                        content,
                        path: report_file.clone(),
                        version: 0,
                        document_file_source: None,
                    })?;
                    let code = session.app.workspace.format_file(FormatFileParams {
                        path: report_file.clone(),
                    })?;
                    console.log(markup! {
                        {code.as_code()}
                    });
                } else {
                    console.log(markup! {
                        {buffer}
                    });
                }
            }
            ReportMode::GitHub => {
                let reporter = GithubReporter {
                    diagnostics_payload: DiagnosticsPayload {
                        verbose: cli_options.verbose,
                        diagnostic_level: cli_options.diagnostic_level,
                        diagnostics,
                    },
                    execution: execution.clone(),
                };
                reporter.write(&mut GithubReporterVisitor(console))?;
            }
            ReportMode::GitLab => {
                let reporter = GitLabReporter {
                    diagnostics: DiagnosticsPayload {
                        verbose: cli_options.verbose,
                        diagnostic_level: cli_options.diagnostic_level,
                        diagnostics,
                    },
                    execution: execution.clone(),
                };
                reporter.write(&mut GitLabReporterVisitor::new(
                    console,
                    session.app.fs.borrow().working_directory(),
                ))?;
            }
            ReportMode::Junit => {
                let reporter = JunitReporter {
                    summary,
                    diagnostics_payload: DiagnosticsPayload {
                        verbose: cli_options.verbose,
                        diagnostic_level: cli_options.diagnostic_level,
                        diagnostics,
                    },
                    execution: execution.clone(),
                };
                reporter.write(&mut JunitReporterVisitor::new(console))?;
            }
        }

        // Processing emitted error diagnostics, exit with a non-zero code
        if processed.saturating_sub(skipped) == 0 && !cli_options.no_errors_on_unmatched {
            Err(CliDiagnostic::no_files_processed())
        } else if errors > 0 || should_exit_on_warnings {
            let category = execution.as_diagnostic_category();
            if should_exit_on_warnings {
                if execution.is_check_apply() {
                    Err(CliDiagnostic::apply_warnings(category))
                } else {
                    Err(CliDiagnostic::check_warnings(category))
                }
            } else if execution.is_check_apply() {
                Err(CliDiagnostic::apply_error(category))
            } else {
                Err(CliDiagnostic::check_error(category))
            }
        } else {
            Ok(())
        }
    }
}
