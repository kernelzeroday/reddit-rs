use crate::client::{self, Post};
use crate::db::{CachePolicy, Db};
use crate::display;
use std::time::Duration;

pub struct FetchOutput {
    pub posts: Vec<Post>,
    pub source: FetchSource,
}

pub enum FetchSource {
    CacheFresh,
    CacheStaleTimeout,
    CacheStaleNetFail,
    Network,
}

pub async fn fetch(
    http: &reqwest::Client,
    db: &Db,
    subreddit: &str,
    sort: &str,
    fanout: usize,
    verbose: bool,
    no_cache: bool,
    policy: &CachePolicy,
) -> Option<FetchOutput> {
    let cache_key = format!("{}/{}", subreddit, sort);
    let cached = if !no_cache {
        db.cache_load(&cache_key)
    } else {
        None
    };

    match &cached {
        Some(entry) if entry.age_secs < policy.fresh_secs => {
            db.bump_stats(1, 0, 1, 0);
            if verbose {
                display::verbose(&format!("fresh cache ({}s old)", entry.age_secs));
            }
            return Some(FetchOutput {
                posts: entry.posts.clone(),
                source: FetchSource::CacheFresh,
            });
        }

        Some(entry) if entry.age_secs < policy.stale_secs => {
            if verbose {
                display::verbose(&format!(
                    "stale cache ({}s old) — racing network vs {}ms timeout",
                    entry.age_secs, policy.race_ms
                ));
            }

            let net_fut = fetch_from_instances(http, db, subreddit, sort, fanout, verbose);

            match tokio::time::timeout(Duration::from_millis(policy.race_ms), net_fut).await {
                Ok(Some(posts)) => {
                    db.cache_put(&cache_key, &posts);
                    db.bump_stats(1, 1, 0, 0);
                    Some(FetchOutput {
                        posts,
                        source: FetchSource::Network,
                    })
                }
                _ => {
                    if verbose {
                        display::verbose("network too slow — returning stale cache");
                    }
                    db.bump_stats(1, 0, 1, 1);
                    Some(FetchOutput {
                        posts: entry.posts.clone(),
                        source: FetchSource::CacheStaleTimeout,
                    })
                }
            }
        }

        _ => {
            if verbose {
                let reason = if cached.is_some() {
                    "expired cache"
                } else {
                    "cache miss"
                };
                display::verbose(&format!("{reason} — fetching from network"));
            }

            match fetch_from_instances(http, db, subreddit, sort, fanout, verbose).await {
                Some(posts) => {
                    db.cache_put(&cache_key, &posts);
                    db.bump_stats(1, 1, 0, 0);
                    Some(FetchOutput {
                        posts,
                        source: FetchSource::Network,
                    })
                }
                None => {
                    if let Some(entry) = cached {
                        db.bump_stats(1, 0, 1, 1);
                        Some(FetchOutput {
                            posts: entry.posts.clone(),
                            source: FetchSource::CacheStaleNetFail,
                        })
                    } else {
                        None
                    }
                }
            }
        }
    }
}

async fn fetch_from_instances(
    http: &reqwest::Client,
    db: &Db,
    subreddit: &str,
    sort: &str,
    fanout: usize,
    verbose: bool,
) -> Option<Vec<Post>> {
    let instances = db.pick_instances(fanout);
    if instances.is_empty() {
        if verbose {
            display::verbose("no instances available");
        }
        return None;
    }

    if verbose {
        display::verbose(&format!(
            "trying {}: {}",
            instances.len(),
            instances.join(", ")
        ));
    }

    let mut progress = display::Progress::new(instances.len());

    for inst in &instances {
        if verbose {
            display::verbose(&format!("querying {inst}..."));
        }

        match client::fetch_subreddit(http, inst, subreddit, sort).await {
            Ok(fr) if !fr.posts.is_empty() => {
                db.record_success(inst, fr.latency_ms);
                db.bump_stats(0, 1, 0, 0);
                progress.tick(&format!("{} posts from {inst}", fr.posts.len()));
                progress.finish();
                if verbose {
                    display::verbose(&format!(
                        "{inst}: {} posts in {}ms",
                        fr.posts.len(),
                        fr.latency_ms
                    ));
                }
                return Some(fr.posts);
            }
            Ok(fr) => {
                db.record_success(inst, fr.latency_ms);
                progress.tick("empty response, trying next...");
                if verbose {
                    display::verbose(&format!("{inst}: empty response"));
                }
            }
            Err(e) => {
                let err = e.to_string();
                db.record_failure(inst, &err);
                db.bump_stats(0, 1, 0, 0);
                if matches!(e, client::FetchError::Blocked(_)) {
                    db.mark_unreachable(inst);
                }
                progress.tick("failed, trying next...");
                if verbose {
                    display::verbose(&err);
                }
            }
        }
    }

    progress.finish();
    None
}
