use anyhow::Result;
use benchmarkoor_replay::{
    baseline, cli::*, default_cache_dir, fixtures, index::TestQuery, replay, schelk, snapshot,
    status, suite,
};
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cache_dir = cli.cache_dir.clone().unwrap_or_else(default_cache_dir);
    let suite = suite::Suite::resolve(
        &cli.suite,
        &cli.context,
        &cli.fork,
        &cli.test_type,
        &cli.metadata_root,
    )?;

    match cli.command.clone() {
        Command::Status(args) => {
            status::print_status(&cache_dir, &suite, &cli.schelk_bin, args.datadir.as_deref())?;
        }
        Command::Fixtures(FixturesCommand::Download(args)) => {
            let source = args
                .url
                .or_else(|| args.release.map(fixtures::FixtureSource::Release))
                .unwrap_or_else(|| fixtures::FixtureSource::Url(suite.fixture_url.clone()));
            let cache =
                fixtures::download_and_index(&cache_dir, &suite, source, args.force).await?;
            println!("fixture_root={}", cache.fixture_root.display());
            println!("index={}", cache.index_path.display());
            println!("tests={}", cache.index.tests.len());
        }
        Command::Fixtures(FixturesCommand::ListTests(args)) => {
            let index = fixtures::load_index(&cache_dir, &suite)?;
            let query = TestQuery::from(args.query);
            let matches = index.search(&query)?;
            for test in matches.iter().take(args.limit) {
                if args.command {
                    println!(
                        "{}",
                        replay::run_command(&cli, &suite, &test.name, args.mode, &cache_dir)
                    );
                } else {
                    println!("{}", test.summary_line());
                }
            }
            if matches.len() > args.limit {
                eprintln!("truncated: {} of {} tests", args.limit, matches.len());
            }
        }
        Command::Fixtures(FixturesCommand::ShowTest(args)) => {
            let index = fixtures::load_index(&cache_dir, &suite)?;
            let test = index
                .find_one(&args.name)?
                .ok_or_else(|| anyhow::anyhow!("test not found: {}", args.name))?;
            println!("{}", serde_json::to_string_pretty(test)?);
        }
        Command::Snapshot(SnapshotCommand::Import(args)) => {
            snapshot::import_snapshot(&suite, &cli.reth_bin, args).await?;
        }
        Command::Baseline(BaselineCommand::Prepare(args)) => {
            baseline::prepare(&cache_dir, &suite, &cli, args).await?;
        }
        Command::Baseline(BaselineCommand::PromotePrerun(args)) => {
            baseline::promote_prerun(&cache_dir, &suite, &cli, args).await?;
        }
        Command::Baseline(BaselineCommand::Verify(args)) => {
            baseline::verify(&cache_dir, &suite, &cli, args).await?;
        }
        Command::Replay(args) => {
            replay::replay_files(&cli, args).await?;
        }
        Command::Run(args) => {
            let index = fixtures::load_index(&cache_dir, &suite)?;
            replay::run_one(&cli, &suite, &index, args).await?;
        }
        Command::RunMany(args) => {
            let index = fixtures::load_index(&cache_dir, &suite)?;
            replay::run_many(&cli, &suite, &index, args).await?;
        }
        Command::RunUrl(args) => {
            let cache = fixtures::download_and_index(
                &cache_dir,
                &suite,
                fixtures::FixtureSource::Url(args.url),
                args.force,
            )
            .await?;
            let run = RunArgs {
                test: args.test,
                mode: args.mode,
                dry_run: args.dry_run,
                no_schelk: args.no_schelk,
                recover_before: false,
            };
            replay::run_one(&cli, &suite, &cache.index, run).await?;
        }
        Command::Schelk(SchelkCommand::Mount) => schelk::run(&cli.schelk_bin, ["mount"])?,
        Command::Schelk(SchelkCommand::Recover(args)) => {
            if args.kill {
                schelk::run(&cli.schelk_bin, ["recover", "--kill"])?;
            } else {
                schelk::run(&cli.schelk_bin, ["recover"])?;
            }
            if args.drop_caches {
                let report = schelk::drop_caches()?;
                println!("drop_caches=succeeded path={}", report.path);
            }
        }
        Command::Schelk(SchelkCommand::FullRecover(args)) => {
            if !args.yes {
                anyhow::bail!(
                    "schelk full-recover overwrites scratch from virgin; rerun with --yes"
                );
            }
            schelk::run(&cli.schelk_bin, ["full-recover"])?;
        }
    }

    Ok(())
}
