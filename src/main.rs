mod client;
mod db;
mod discover;
mod display;
mod fetch;

use clap::{Parser, Subcommand};
use colored::Colorize;

#[derive(Parser)]
#[command(name = "reddit", about = "Browse Reddit via Redlib proxies from the terminal")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Subreddit to browse (e.g. "rust", "r/programming")
    subreddit: Vec<String>,

    #[arg(short, long, default_value = "hot", help = "Sort: hot, new, top, rising")]
    sort: String,

    #[arg(short, long, default_value_t = 25)]
    number: usize,

    #[arg(short, long, help = "Pin a specific instance URL")]
    instance: Option<String>,

    #[arg(short, long, help = "Verbose debug output")]
    verbose: bool,

    #[arg(long, help = "Skip result cache")]
    no_cache: bool,

    #[arg(short, long, help = "Output as JSON")]
    json: bool,

    #[arg(short = 'N', long, default_value_t = 3, help = "Number of instances to try")]
    fanout: usize,

    #[arg(long, default_value_t = 300, help = "Seconds before cached results are stale")]
    fresh_ttl: i64,

    #[arg(long, default_value_t = 3600, help = "Seconds before cached results expire")]
    stale_ttl: i64,

    #[arg(long, default_value_t = 2000, help = "Milliseconds to wait before returning stale cache")]
    race_timeout: u64,
}

#[derive(Subcommand)]
enum Command {
    /// Discover and probe Redlib instances
    Discover,
    /// List known instances with health data
    List,
    /// Show aggregate statistics
    Stats,
    /// View a post with comments
    Post {
        /// Post permalink (e.g. /r/rust/comments/abc123/title/)
        permalink: String,
        #[arg(short, long, help = "Output as JSON")]
        json: bool,
    },
    /// Search Reddit
    Search {
        /// Search query
        query: Vec<String>,
        #[arg(short, long, help = "Restrict to a subreddit")]
        sub: Option<String>,
        #[arg(short, long, default_value = "relevance", help = "Sort: relevance, hot, top, new, comments")]
        sort: String,
        #[arg(short, long, default_value = "all", help = "Time: hour, day, week, month, year, all")]
        time: String,
        #[arg(short, long, default_value_t = 25)]
        number: usize,
        #[arg(short, long, help = "Output as JSON")]
        json: bool,
    },
    /// Clean up expired cache
    Cleanup,
    /// View and manage the result cache
    Cache {
        #[command(subcommand)]
        action: Option<CacheAction>,
    },
}

#[derive(Subcommand)]
enum CacheAction {
    /// Clear cached results
    Clear {
        /// Clear only this cache key (e.g. "rust/hot")
        key: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let db = db::Db::open();
    let http = client::build_client();

    match cli.command {
        Some(Command::Discover) => {
            discover::discover(&http, &db, cli.verbose).await;
        }

        Some(Command::List) => {
            let instances = db.all_instances();
            display::print_instance_list(&instances);
        }

        Some(Command::Stats) => {
            let s = db.stats();
            let cs = db.cache_stats();
            println!("{}", "Statistics".bold());
            println!("  Fetches:          {}", s.total_fetches);
            println!("  Instance queries: {}", s.total_instance_queries);
            println!(
                "  Cache hits:       {} (fresh: {}, stale: {})",
                s.total_cache_hits,
                s.total_cache_hits.saturating_sub(s.total_stale_hits),
                s.total_stale_hits,
            );
            if s.total_fetches > 0 {
                let hit_rate = (s.total_cache_hits as f64 / s.total_fetches as f64) * 100.0;
                println!("  Cache hit rate:   {:.1}%", hit_rate);
            }
            println!();
            println!("{}", "Cache".bold());
            println!("  Entries:          {}", cs.entry_count);
            println!("  Cached posts:     {}", cs.total_results);
            println!(
                "  Database size:    {}",
                display::format_bytes(cs.db_size_bytes)
            );
            if let (Some(oldest), Some(newest)) = (cs.oldest_entry_secs, cs.newest_entry_secs) {
                println!(
                    "  Oldest entry:     {} ago",
                    display::format_duration(oldest)
                );
                println!(
                    "  Newest entry:     {} ago",
                    display::format_duration(newest)
                );
            }
        }

        Some(Command::Post { permalink, json }) => {
            let path = if permalink.starts_with("/r/") {
                permalink.clone()
            } else {
                format!("/r/{permalink}")
            };

            let instances = db.pick_instances(cli.fanout);
            let mut found = false;
            for inst in &instances {
                match client::fetch_post(&http, inst, &path).await {
                    Ok(detail) => {
                        db.record_success(inst, detail.latency_ms);
                        if json {
                            display::print_post_json(&detail);
                        } else {
                            display::print_post_detail(&detail);
                        }
                        found = true;
                        break;
                    }
                    Err(e) => {
                        db.record_failure(inst, &e.to_string());
                        if cli.verbose {
                            display::verbose(&e.to_string());
                        }
                    }
                }
            }
            if !found {
                eprintln!("{}: failed to fetch post", "error".red().bold());
                std::process::exit(1);
            }
        }

        Some(Command::Search { query, sub, sort, time, number, json }) => {
            if query.is_empty() {
                eprintln!("{}: provide a search query", "error".red().bold());
                std::process::exit(1);
            }
            let q = query.join(" ");
            let sub_ref = sub.as_deref();

            let instances = db.pick_instances(cli.fanout);
            let mut found = false;
            for inst in &instances {
                match client::search(&http, inst, &q, sub_ref, &sort, &time).await {
                    Ok(fr) if !fr.posts.is_empty() => {
                        db.record_success(inst, fr.latency_ms);
                        if json {
                            display::print_json(&fr.posts);
                        } else {
                            display::print_posts(&fr.posts, number);
                        }
                        found = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        db.record_failure(inst, &e.to_string());
                        if cli.verbose {
                            display::verbose(&e.to_string());
                        }
                    }
                }
            }
            if !found {
                eprintln!("{}: no results found", "error".red().bold());
                std::process::exit(1);
            }
        }

        Some(Command::Cleanup) => {
            db.cleanup();
            db.vacuum();
            println!(
                "{} cache expired, database vacuumed",
                "Done.".green().bold()
            );
        }

        Some(Command::Cache { action }) => match action {
            None => {
                let entries = db.cache_list();
                let stats = db.cache_stats();
                display::print_cache_list(&entries, &stats);
            }
            Some(CacheAction::Clear { key }) => {
                let deleted = match key {
                    Some(k) => db.cache_clear_query(&k),
                    None => db.cache_clear_all(),
                };
                println!(
                    "{} cleared {} cache entries",
                    "Done.".green().bold(),
                    deleted
                );
            }
        },

        None => {
            if cli.subreddit.is_empty() {
                eprintln!("{}: provide a subreddit", "error".red().bold());
                eprintln!("  reddit <subreddit>     browse a subreddit");
                eprintln!("  reddit search <query>  search Reddit");
                eprintln!("  reddit discover        find Redlib instances");
                eprintln!("  reddit list            show known instances");
                eprintln!("  reddit stats           show statistics");
                eprintln!("  reddit cache           view cached results");
                eprintln!("  reddit --help          full usage");
                std::process::exit(1);
            }

            let raw = cli.subreddit.join(" ");
            let subreddit = raw.strip_prefix("r/").unwrap_or(&raw);

            if let Some(ref inst) = cli.instance {
                match client::fetch_subreddit(&http, inst, subreddit, &cli.sort).await {
                    Ok(fr) => {
                        db.record_success(inst, fr.latency_ms);
                        if cli.json {
                            display::print_json(&fr.posts);
                        } else {
                            eprintln!(
                                "{}",
                                format!("via {} ({}ms)", inst, fr.latency_ms).dimmed()
                            );
                            display::print_posts(&fr.posts, cli.number);
                        }
                    }
                    Err(e) => {
                        eprintln!("{}: {e}", "error".red().bold());
                        std::process::exit(1);
                    }
                }
                return;
            }

            let policy = db::CachePolicy {
                fresh_secs: cli.fresh_ttl,
                stale_secs: cli.stale_ttl,
                race_ms: cli.race_timeout,
            };

            match fetch::fetch(
                &http,
                &db,
                subreddit,
                &cli.sort,
                cli.fanout,
                cli.verbose,
                cli.no_cache,
                &policy,
            )
            .await
            {
                Some(output) => {
                    if cli.json {
                        display::print_json(&output.posts);
                    } else {
                        match output.source {
                            fetch::FetchSource::CacheFresh => {
                                display::print_posts_with_label(
                                    &output.posts,
                                    cli.number,
                                    "cached",
                                );
                            }
                            fetch::FetchSource::CacheStaleTimeout => {
                                display::print_posts_with_label(
                                    &output.posts,
                                    cli.number,
                                    "stale cache — network slow",
                                );
                            }
                            fetch::FetchSource::CacheStaleNetFail => {
                                display::print_posts_with_label(
                                    &output.posts,
                                    cli.number,
                                    "stale cache — network failed",
                                );
                            }
                            fetch::FetchSource::Network => {
                                display::print_posts(&output.posts, cli.number);
                            }
                        }
                    }
                }
                None => {
                    eprintln!(
                        "{}: all instances failed — run `reddit discover` to find working ones",
                        "error".red().bold()
                    );
                    std::process::exit(1);
                }
            }
        }
    }
}
