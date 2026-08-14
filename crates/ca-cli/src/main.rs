//! Headless batch runner.
//!
//! This is the interface for the long jobs — the overnight run across a whole
//! corpus — and it is also the seam a server deployment would drive. The
//! desktop app calls the same crates; nothing here is CLI-only logic.

use ca_asr::{WhisperCliConfig, WhisperCliTranscriber};
use ca_core::analysis::AgentKind;
use ca_core::call::CallStage;
use ca_ingest::LocalFolderSource;
use ca_llm::{ProviderKind, ProviderSpec};
use ca_pipeline::{Analyzer, Pipeline, PipelineOptions, Progress};
use ca_store::Store;
mod serve;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "ca", about = "Post-call analysis batch runner")]
struct Cli {
    /// Database file. Defaults to the same location the desktop app uses,
    /// so both share one corpus.
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Act as this agent handle for this command only, without changing the
    /// stored operator. Useful for checking what a colleague would see.
    #[arg(long, global = true)]
    r#as: Option<String>,

    /// Which tenant's corpus to work on. Each tenant is a separate database
    /// file, so this is not a filter that could be forgotten — it selects
    /// which corpus exists at all.
    #[arg(long, global = true, default_value = ca_store::DEFAULT_TENANT)]
    tenant: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scan a folder and register everything found. Safe to re-run.
    Discover {
        folder: PathBuf,
        #[arg(long, default_value_t = true)]
        recursive: bool,
    },
    /// Process registered calls that have not finished yet.
    Run {
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value_t = 2)]
        concurrency: usize,
        /// Whisper threads per file.
        #[arg(long, default_value_t = 8)]
        threads: usize,
        #[arg(long)]
        model: Option<PathBuf>,
        /// Re-process calls that already completed.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Run the agent battery (QA, sentiment, compliance, root cause) over
    /// calls that are transcribed but unscored.
    ///
    /// Separate from `run` because the two have different costs and different
    /// failure modes: transcription is free and CPU-bound, scoring is metered
    /// and depends on a third party being up.
    Score {
        #[arg(long)]
        limit: Option<usize>,
        /// Concurrent model calls. Higher than `run` because this waits on a
        /// network, not on cores.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        /// Vendor: nvidia_nim, openai, openrouter, groq, ollama, lmstudio,
        /// anthropic, gemini, or openai_compatible.
        #[arg(long, default_value = "nvidia_nim")]
        provider: String,
        #[arg(long, default_value = "meta/llama-3.1-8b-instruct")]
        model: String,
        /// Base URL, required only for `openai_compatible`.
        #[arg(long)]
        base_url: Option<String>,
        /// TOML rubric file. Uses the built-in five-criterion rubric if absent.
        #[arg(long)]
        rubric: Option<PathBuf>,
        /// Re-score calls that already have a scorecard.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Print a stored transcript.
    Show { call_id: String },
    /// Print a stored QA scorecard.
    Scorecard { call_id: String },
    /// Print everything the agent battery produced for a call.
    Report {
        call_id: String,
        /// Print the full payloads as well as the digest.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Corpus and spend summary.
    Stats,
    /// Derive who was on each call from its filename.
    ///
    /// Explicit rather than automatic: this rewrites every row in the corpus,
    /// which is not something to do behind the user's back at startup.
    Attribute {
        /// Report what would change and write nothing.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Also overwrite assignments made by hand. Off by default, because a
        /// backfill that discards corrections is worse than one that skips.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Clear the corpus so you can start again.
    ///
    /// Takes a copy of the database first unless told not to. That copy is the
    /// only thing between a mistaken reset and a corpus that took hours to
    /// build, so opting out has to be deliberate.
    Reset {
        /// analyses | transcripts | all.
        ///
        /// `analyses` drops scores and findings but keeps transcripts, so a
        /// re-score costs model calls and no transcription. `transcripts` also
        /// drops the text and returns every call to `discovered`. `all` empties
        /// the corpus; settings are kept either way.
        #[arg(long, default_value = "analyses")]
        scope: String,
        /// Skip the confirmation prompt. Required when not on a terminal.
        #[arg(long, default_value_t = false)]
        yes: bool,
        /// Do not copy the database first.
        #[arg(long, default_value_t = false)]
        no_backup: bool,
    },
    /// Serve the real corpus to the dev UI on loopback.
    ///
    /// `npm run dev` runs the UI in a browser, where there is no Tauri runtime.
    /// With this running it shows real data from this database instead of
    /// invented fixtures. Development only — the desktop app talks to the same
    /// store in-process and never over a socket.
    Serve {
        #[arg(long, default_value_t = 8479)]
        port: u16,
    },
    /// List every tenant that has a corpus on this machine.
    Tenants,
    /// Create a tenant's corpus now rather than on first use.
    NewTenant { name: String },
    /// Show who this machine is signed in as, and what that lets them see.
    Whoami,
    /// Set who is using this machine. Omit the handle to clear it.
    ///
    /// This narrows what the app *shows*. It is not a security boundary while
    /// everything runs against one local file — anyone who can read the
    /// database can read all of it.
    Operator { handle: Option<String> },
    /// Give an agent a role: agent, supervisor or admin.
    Role { handle: String, role: String },
    /// Put an agent on a team. Omit the team to remove them from one.
    Team {
        handle: String,
        team: Option<String>,
    },
    /// List teams and their membership.
    Teams,
    /// List the people on calls.
    People {
        /// `agent` or `contact`. Both if omitted.
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        search: Option<String>,
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
}

fn default_paths() -> (PathBuf, PathBuf) {
    // Repo-relative so a checkout works without configuration.
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    (
        root.join("vendor/whisper/Release/whisper-cli.exe"),
        root.join("models/ggml-small.en.bin"),
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let cli = Cli::parse();
    // An explicit --db wins: it names one file outright, and silently
    // redirecting it under a tenant directory would be a surprise.
    let db_path = match cli.db {
        Some(path) => path,
        None => {
            let base = ca_store::default_db_path();
            let base = base.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            ca_store::tenant_db_path(&base, &cli.tenant)?
        }
    };
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let store = Arc::new(Store::open(&db_path)?);

    // `--as` answers "what would this person see?" without changing whose
    // machine this is. Everything that reads calls goes through this one value.
    let scope = match cli.r#as.as_deref() {
        Some(handle) => store.scope_for_handle(handle)?,
        None => store.current_scope()?,
    };

    match cli.command {
        Command::Discover { folder, recursive } => {
            let source = LocalFolderSource::new(&folder).recursive(recursive);
            let (bin, model) = default_paths();
            let asr = Arc::new(WhisperCliTranscriber::new(WhisperCliConfig::new(
                bin, model,
            ))?);
            let pipeline = Pipeline::new(store.clone(), asr);

            let (total, new) = pipeline.discover(&source).await?;
            println!("found {total} media files in {}", folder.display());
            println!("registered {new} new, {} already known", total - new);
        }

        Command::Run {
            limit,
            concurrency,
            threads,
            model,
            force,
        } => {
            let (bin, default_model) = default_paths();
            let model = model.unwrap_or(default_model);
            let cfg = WhisperCliConfig::new(bin, &model).with_threads(threads);
            if cfg.is_english_only() {
                eprintln!(
                    "warning: {} is English-only; non-English calls will transcribe poorly",
                    model.display()
                );
            }
            let asr = Arc::new(WhisperCliTranscriber::new(cfg)?);
            let pipeline = Pipeline::new(store.clone(), asr);

            let opts = PipelineOptions {
                concurrency,
                limit,
                restart_from: force.then_some(CallStage::Discovered),
                target_stage: CallStage::Redacted,
                ..Default::default()
            };

            let mut done = 0usize;
            let report = pipeline
                .run(&opts, |p| match p {
                    Progress::Discovered { total, .. } => {
                        println!("{total} calls to process");
                    }
                    Progress::Finished { name, secs, .. } => {
                        done += 1;
                        println!("  [{done}] ok   {:.0}s  {}", secs, short(&name));
                    }
                    Progress::Failed { name, error, .. } => {
                        done += 1;
                        println!("  [{done}] FAIL      {}  — {}", short(&name), short(&error));
                    }
                    _ => {}
                })
                .await?;

            println!(
                "\n{} processed, {} failed, {:.0}s of audio in {:.0}s ({:.1}x realtime)",
                report.processed,
                report.failed,
                report.audio_secs,
                report.wall_secs,
                report.audio_secs / report.wall_secs.max(1e-9)
            );
        }

        Command::Score {
            limit,
            concurrency,
            provider,
            model,
            base_url,
            rubric,
            force,
        } => {
            let kind = ProviderKind::parse(&provider).ok_or_else(|| {
                format!(
                    "unknown provider `{provider}`; try one of: {}",
                    ProviderKind::all()
                        .iter()
                        .map(|k| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;

            let key = kind.api_key_from_env();
            if kind.requires_api_key() && key.is_none() {
                return Err(format!(
                    "{} needs a key in ${}",
                    kind.label(),
                    kind.env_var().unwrap_or("(none)")
                )
                .into());
            }

            let mut spec = ProviderSpec::new(kind, &model);
            spec.base_url = base_url;
            let llm = ca_llm::build_provider(&spec, key.as_deref())?;

            let rubric = match &rubric {
                Some(p) => ca_agents::Rubric::load(p)?,
                None => ca_agents::Rubric::default(),
            };
            println!(
                "agent battery with {} / {} (QA rubric `{}` v{}, {} criteria)",
                kind.label(),
                model,
                rubric.id,
                rubric.version,
                rubric.criteria.len()
            );

            let analyzer = Arc::new(Analyzer::new(
                llm,
                ca_agents::QaScorecardAgent::new(rubric),
                kind.is_local(),
            ));

            // Still needed to construct the pipeline, but `require_stage`
            // below guarantees nothing selected here reaches it.
            let (bin, default_model) = default_paths();
            let asr = Arc::new(WhisperCliTranscriber::new(WhisperCliConfig::new(
                bin,
                default_model,
            ))?);
            let pipeline = Pipeline::new(store.clone(), asr).with_analyzer(analyzer);

            let opts = PipelineOptions {
                concurrency,
                limit,
                // Re-scoring rewinds only the analysis stage; the transcript
                // and its redaction are kept.
                restart_from: force.then_some(CallStage::Analyzed),
                target_stage: CallStage::Analyzed,
                // Only calls that already have a transcript. Remote providers
                // additionally need it redacted, so the floor is higher there —
                // and a call that misses the floor is skipped, not silently
                // transcribed inside a metered batch.
                require_stage: Some(if kind.is_local() {
                    CallStage::Diarized
                } else {
                    CallStage::Redacted
                }),
                ..Default::default()
            };

            let mut done = 0usize;
            let report = pipeline
                .run(&opts, |p| match p {
                    Progress::Discovered { total, .. } => println!("{total} calls to score"),
                    Progress::Finished { call, .. } => {
                        done += 1;
                        let score = store
                            .load_analysis(&call, AgentKind::QaScorecard)
                            .ok()
                            .flatten()
                            .and_then(|v| v.get("overall").and_then(|o| o.as_f64()));
                        let sent = store
                            .load_analysis(&call, AgentKind::Sentiment)
                            .ok()
                            .flatten()
                            .and_then(|v| v.get("overall").and_then(|o| o.as_f64()));
                        let comp = store
                            .load_analysis(&call, AgentKind::Compliance)
                            .ok()
                            .flatten()
                            .and_then(|v| v.get("passed").and_then(|o| o.as_bool()));
                        let rca = store
                            .load_analysis(&call, AgentKind::RootCause)
                            .ok()
                            .flatten()
                            .and_then(|v| {
                                v.get("category")
                                    .and_then(|c| c.as_str())
                                    .map(|s| s.to_string())
                            });
                        let acts = store
                            .load_analysis(&call, AgentKind::ActionItems)
                            .ok()
                            .flatten()
                            .and_then(|v| {
                                v.get("items").and_then(|i| i.as_array()).map(|a| a.len())
                            });
                        let qa_s = score
                            .map(|s| format!("{s:>5.1}%"))
                            .unwrap_or_else(|| "   ?  ".into());
                        let se_s = sent
                            .map(|s| format!("sent={s:+.2}"))
                            .unwrap_or_else(|| "sent=?".into());
                        let co_s = match comp {
                            Some(true) => "comp=pass",
                            Some(false) => "comp=FAIL",
                            None => "comp=?",
                        };
                        let rc_s = rca
                            .map(|c| format!("rca={c}"))
                            .unwrap_or_else(|| "rca=?".into());
                        let ac_s = acts
                            .map(|n| format!("acts={n}"))
                            .unwrap_or_else(|| "acts=?".into());
                        println!(
                            "  [{done}] {qa_s}  {se_s}  {co_s}  {rc_s}  {ac_s}  {}",
                            short(call.as_str())
                        );
                    }
                    Progress::Failed { name, error, .. } => {
                        done += 1;
                        println!("  [{done}] FAIL     {}  — {}", short(&name), short(&error));
                    }
                    _ => {}
                })
                .await?;

            let (input, output, cost) = store.total_usage()?;
            println!(
                "\n{} scored, {} failed in {:.0}s\ntokens {input} in / {output} out, ${cost:.4} total spend",
                report.processed, report.failed, report.wall_secs
            );
        }

        Command::Scorecard { call_id } => {
            let id = ca_core::ids::CallId::from(call_id);
            match store.load_analysis(&id, AgentKind::QaScorecard)? {
                None => println!("no scorecard stored for {id}"),
                Some(v) => {
                    let card: ca_core::analysis::Scorecard = serde_json::from_value(v)?;
                    println!("{id} — {:.1}% overall", card.overall);
                    println!("rubric {} v{}\n", card.rubric_id, card.rubric_version);
                    for c in &card.criteria {
                        let score = if c.not_applicable {
                            "  n/a".to_string()
                        } else {
                            format!("{:>5.0}%", c.score * 100.0)
                        };
                        println!("{score}  {} (weight {})", c.label, c.weight);
                        println!("        {}", c.reasoning);
                        for cite in &c.citations {
                            println!(
                                "        @ {} {}",
                                ca_core::transcript::fmt_ts(cite.start),
                                cite.quote.as_deref().unwrap_or("")
                            );
                        }
                    }
                    if !card.summary.is_empty() {
                        println!("\n{}", card.summary);
                    }
                }
            }
        }

        Command::Report { call_id, json } => {
            let id = ca_core::ids::CallId::from(call_id);
            println!("report for {id}\n");

            // Every agent the build knows about, driven off `AgentKind::all()`.
            // This list was written out by hand, so the three agents added after
            // it was written were silently absent from every report.
            let mut payloads = Vec::new();
            for agent in AgentKind::all() {
                match store.load_analysis(&id, *agent)? {
                    None => println!("  {:<20} (not run)", agent.label()),
                    Some(v) => {
                        println!("  {:<20} {}", agent.label(), digest(*agent, &v));
                        payloads.push((*agent, v));
                    }
                }
            }

            // The ledger, not the compliance payload: it is the durable record,
            // and it covers any agent that files findings.
            let findings = store.findings(&id)?;
            if !findings.is_empty() {
                let blocking = findings.iter().filter(|f| f.blocking).count();
                println!("\nfindings ({} total, {blocking} blocking)", findings.len());
                for f in &findings {
                    let at = f
                        .citations
                        .first()
                        .map(|c| format!(" @ {}", ca_core::transcript::fmt_ts(c.start)))
                        .unwrap_or_default();
                    println!(
                        "  [{:>8}] {:<24} {}{at}",
                        f.severity.as_str(),
                        short(&f.code),
                        short(&f.title)
                    );
                }
            }

            if json {
                for (agent, v) in payloads {
                    println!("\n[{}]", agent.as_str());
                    println!("{}", serde_json::to_string_pretty(&v)?);
                }
            } else if !payloads.is_empty() {
                println!("\npass --json for the full payloads");
            }
        }

        Command::Show { call_id } => {
            let id = ca_core::ids::CallId::from(call_id);
            match store.load_transcript(&id)? {
                None => println!("no transcript stored for {id}"),
                Some(t) => {
                    println!(
                        "{} — {} segments, {} words, {} / {}\n",
                        id,
                        t.segments.len(),
                        t.word_count(),
                        t.engine,
                        t.model
                    );
                    for s in &t.segments {
                        println!(
                            "[{}] spk{} {}",
                            ca_core::transcript::fmt_ts(s.start),
                            s.speaker_index,
                            // Show the redacted form: this is what leaves the
                            // machine, so it is what matters to review.
                            s.outbound_text()
                        );
                    }
                }
            }
        }

        Command::Stats => {
            let s = store.stats(&scope)?;
            let (input, output, cost) = store.total_usage()?;
            if !scope.is_unrestricted() {
                println!("(scoped to {})", scope.role.as_str());
            }
            println!("calls registered : {}", s.total_calls);
            println!("calls outstanding: {}", s.outstanding);
            println!("tokens           : {input} in / {output} out");
            println!("spend            : ${cost:.4}");
        }
        Command::Attribute { dry_run, force } => {
            let report = store.attribute_calls(
                &ca_identity::builtin_patterns(),
                ca_store::AttributeOptions { dry_run, force },
            )?;
            println!(
                "{} scanned, {} attributed, {} skipped (manual), {} unmatched{}",
                report.scanned,
                report.attributed,
                report.skipped_manual,
                report.unmatched,
                if dry_run {
                    "  [dry run, nothing written]"
                } else {
                    ""
                },
            );
        }
        Command::Reset {
            scope,
            yes,
            no_backup,
        } => {
            let Some(scope) = ca_store::ResetScope::parse(&scope) else {
                eprintln!("unknown scope `{scope}` — use analyses, transcripts or all");
                std::process::exit(2);
            };

            let preview = store.reset_preview(scope)?;
            println!("This will delete {}.", scope.describe());
            println!(
                "  {} calls, {} transcripts, {} analyses, {} findings, {} people, {} runs",
                preview.calls,
                preview.transcripts,
                preview.analyses,
                preview.findings,
                preview.people,
                preview.runs,
            );

            if !yes {
                // Nothing is destroyed on a bare `ca reset`: the count above is
                // usually enough to change someone's mind about the scope.
                println!(
                    "
Nothing was deleted. Re-run with --yes to go ahead."
                );
                return Ok(());
            }

            if !no_backup {
                let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
                match store.backup(&stamp) {
                    Ok(Some(path)) => println!("backup: {}", path.display()),
                    Ok(None) => println!("in-memory corpus — nothing to back up"),
                    Err(e) => {
                        // Refuse rather than proceed: an unbacked-up reset is a
                        // different, far riskier operation than the one asked for.
                        eprintln!("could not back up the database: {e}");
                        eprintln!("nothing was deleted — pass --no-backup to reset anyway");
                        std::process::exit(1);
                    }
                }
            }

            let report = store.reset(scope)?;
            println!(
                "removed {} calls, {} transcripts, {} analyses, {} findings, {} people",
                report.calls, report.transcripts, report.analyses, report.findings, report.people,
            );
        }
        Command::Tenants => {
            let base = ca_store::default_db_path();
            let base = base.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let tenants = ca_store::TenantStores::new(&base).list()?;
            if tenants.is_empty() {
                println!("no corpora yet");
            }
            println!("{:<20} {:>10}  PATH", "TENANT", "SIZE");
            for t in &tenants {
                println!(
                    "{:<20} {:>10}  {}",
                    clip(&t.slug, 20),
                    t.bytes.map(human_bytes).unwrap_or_else(|| "-".into()),
                    t.path,
                );
            }
        }
        Command::NewTenant { name } => {
            let base = ca_store::default_db_path();
            let base = base.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            let info = ca_store::TenantStores::new(&base).create(&name)?;
            println!("created tenant `{}` at {}", info.slug, info.path);
        }
        Command::Whoami => {
            let scope = store.current_scope()?;
            let handle = store.operator_handle()?;
            println!("operator : {}", handle.as_deref().unwrap_or("(not set)"));
            println!("role     : {}", scope.role.as_str());
            println!(
                "sees     : {}",
                if scope.is_unrestricted() {
                    "every call".to_string()
                } else if let Some(t) = &scope.team_id {
                    let name = store
                        .list_teams()?
                        .into_iter()
                        .find(|s| s.id == t.as_str())
                        .map(|s| s.name)
                        .unwrap_or_else(|| t.as_str().to_string());
                    format!("calls handled by team `{name}`")
                } else {
                    "only their own calls".to_string()
                }
            );
            println!("calls    : {}", store.stats(&scope)?.total_calls);
        }
        Command::Operator { handle } => {
            store.set_operator(handle.as_deref())?;
            match handle {
                Some(h) => println!("operator set to {h}"),
                None => println!("operator cleared — the app shows every call again"),
            }
        }
        Command::Role { handle, role } => {
            let Some(parsed) = ca_core::identity::AccessRole::parse(&role) else {
                eprintln!("unknown role `{role}` — use agent, supervisor or admin");
                std::process::exit(2);
            };
            let normalised = ca_identity::normalise_handle(&handle);
            let id = store.upsert_person(
                ca_core::identity::PersonKind::Agent,
                handle.trim(),
                &normalised,
            )?;
            store.set_person_role(&id, parsed)?;
            println!("{handle} is now {}", parsed.as_str());
        }
        Command::Team { handle, team } => {
            let normalised = ca_identity::normalise_handle(&handle);
            let id = store.upsert_person(
                ca_core::identity::PersonKind::Agent,
                handle.trim(),
                &normalised,
            )?;
            store.set_person_team(&id, team.as_deref())?;
            match team {
                Some(t) => println!("{handle} joined {t}"),
                None => println!("{handle} left their team"),
            }
        }
        Command::Teams => {
            let teams = store.list_teams()?;
            if teams.is_empty() {
                println!("no teams yet — `ca team <handle> <team>` creates one");
            }
            for t in &teams {
                println!("{:<28} {:>3} members", clip(&t.name, 28), t.members);
            }
        }
        Command::Serve { port } => {
            println!("serving {}", db_path.display());
            serve::serve(store.clone(), port)?;
        }
        Command::People {
            kind,
            search,
            limit,
        } => {
            let kind = kind
                .as_deref()
                .and_then(ca_core::identity::PersonKind::parse);
            let people = store.list_people(kind, search.as_deref())?;
            println!("{:<32} {:<8} {:>6}  LAST CALL", "NAME", "KIND", "CALLS");
            for p in people.iter().take(limit) {
                println!(
                    "{:<32} {:<8} {:>6}  {}",
                    clip(&p.display_name, 32),
                    p.kind,
                    p.calls,
                    // An unattributed time shows as absent rather than as a
                    // date nobody ever observed.
                    p.last_call
                        .as_deref()
                        .map(|s| &s[..10.min(s.len())])
                        .unwrap_or("-"),
                );
            }
            println!();
            println!("{} people", people.len());
        }
    }

    Ok(())
}

/// Bytes as something a person can read at a glance.
fn human_bytes(n: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    if n as f64 >= MB {
        format!("{:.1} MB", n as f64 / MB)
    } else {
        format!("{:.0} KB", (n as f64 / 1024.0).max(1.0))
    }
}

/// Trim to a column width without splitting a multi-byte character.
///
/// Contact names in a real corpus contain emoji, so slicing by byte would
/// panic on the first one.
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "\u{2026}"
    }
}

fn short(s: &str) -> String {
    s.chars().take(70).collect()
}

/// One line summarising what an agent found.
///
/// Read field by field out of the stored JSON rather than deserialised into the
/// report type: a row written by an older build prints what it can rather than
/// taking the whole report down. The match is over `AgentKind`, so adding an
/// agent to the battery does not compile until it has a line here.
fn digest(agent: AgentKind, v: &serde_json::Value) -> String {
    let f = |key: &str| v.get(key).and_then(|x| x.as_f64());
    let s = |key: &str| v.get(key).and_then(|x| x.as_str()).unwrap_or("");
    let n = |key: &str| {
        v.get(key)
            .and_then(|x| x.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    };

    match agent {
        AgentKind::QaScorecard => format!(
            "{:.1}% over {} criteria",
            f("overall").unwrap_or(0.0),
            n("criteria")
        ),
        AgentKind::Sentiment => format!(
            "{:+.2} across {} moments",
            f("overall").unwrap_or(0.0),
            n("trajectory")
        ),
        AgentKind::Compliance => {
            let passed = v.get("passed").and_then(|x| x.as_bool()).unwrap_or(false);
            format!(
                "{} — {} finding(s)",
                if passed { "pass" } else { "FAIL" },
                n("findings")
            )
        }
        AgentKind::RootCause => format!(
            "{} ({:.0}% confidence) — {}",
            s("category"),
            f("confidence").unwrap_or(0.0) * 100.0,
            short(s("primaryCause"))
        ),
        AgentKind::ActionItems => {
            let high = v
                .get("items")
                .and_then(|x| x.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter(|i| i.get("priority").and_then(|p| p.as_str()) == Some("high"))
                        .count()
                })
                .unwrap_or(0);
            format!("{} item(s), {high} high", n("items"))
        }
        AgentKind::Coaching => format!(
            "{} strength(s), {} improvement(s)",
            n("strengths"),
            n("improvements")
        ),
        AgentKind::CustomerExperience => format!(
            "effort {:.0}%, escalation {:.0}%, churn {:.0}%",
            f("effort").unwrap_or(0.0) * 100.0,
            f("escalationRisk").unwrap_or(0.0) * 100.0,
            f("churnRisk").unwrap_or(0.0) * 100.0,
        ),
        AgentKind::ExecutiveSummary => {
            format!("{} — {}", s("customerOutcome"), short(s("primaryIssue")))
        }
        // The rules tier files findings rather than an analysis payload, so a
        // row for it only exists if something wrote one deliberately.
        AgentKind::Rules => format!("{} finding(s)", n("findings")),
        AgentKind::LiveMonitor => format!(
            "{} risk, sentiment {:+.2} — {}",
            s("risk"),
            f("sentiment").unwrap_or(0.0),
            short(s("headline"))
        ),
    }
}
