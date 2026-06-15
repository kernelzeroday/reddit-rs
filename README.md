# reddit-rs

Browse Reddit from the terminal — no API key, no tracking, no account needed.

Uses [Redlib](https://github.com/redlib-org/redlib) privacy proxies as the backend, so nothing touches Reddit directly.

## Install

```
cargo install --path .
```

Requires Rust 2024 edition (1.85+).

## Quick start

```bash
# discover working Redlib instances (run this first)
reddit discover

# browse a subreddit
reddit rust
reddit programming --sort new -n 50

# search
reddit search "async runtime" --sub rust --time month

# read a post with comments
reddit post /r/rust/comments/abc123/some_title/
```

## Usage

```
reddit <subreddit>                     browse r/<subreddit>/hot
reddit <subreddit> -s new              sort by new, top, rising
reddit <subreddit> -n 10               limit to 10 posts
reddit <subreddit> -j                  output JSON
reddit search <query>                  search all of Reddit
reddit search <query> --sub rust       search within a subreddit
reddit post <permalink>                view post + comment thread
reddit discover                        find working Redlib instances
reddit discover --deep                 scan all sources (GitHub, Codeberg, Farside, Shodan, Searx)
reddit list                            show known instances with health stats
reddit stats                           cache hit rate, db size, entry age
reddit cache                           list cached queries
reddit cache clear                     wipe the cache
reddit cleanup                         remove expired entries and vacuum
```

### Flags

| Flag | Default | Description |
|------|---------|-------------|
| `-s, --sort` | `hot` | Sort order: `hot`, `new`, `top`, `rising` |
| `-n, --number` | `25` | Number of posts to display |
| `-i, --instance` | auto | Pin a specific Redlib instance URL |
| `-N, --fanout` | `3` | Instances to try in parallel |
| `-j, --json` | off | Output raw JSON |
| `-v, --verbose` | off | Show request timings and decisions |
| `--no-cache` | off | Bypass the result cache |
| `--fresh-ttl` | `300` | Seconds before cache is considered stale |
| `--stale-ttl` | `3600` | Seconds before cache expires entirely |
| `--race-timeout` | `2000` | Milliseconds to wait before falling back to stale cache |

## How it works

**Instance discovery** pulls from multiple sources in parallel — the [redlib-org instance list](https://github.com/redlib-org/redlib-instances), a Codeberg mirror, and [Farside](https://github.com/benbusby/farside). With `--deep`, it also scans Shodan and Searx for unlisted instances. Every discovered URL is probed against `/r/all/hot` and scored by latency and reliability. Instances behind anti-bot walls (Cloudflare Anubis) are automatically excluded.

**Instance selection** picks the best available instance using a score based on success ratio, average latency, and recency, with random jitter to spread load.

**Caching** is three-tier:
- **Fresh** (< 5 min by default) — served immediately from SQLite, no network hit
- **Stale** (5 min – 1 hr) — races a network fetch against a 2-second timeout; if the network is slow, returns the stale result instantly
- **Expired** (> 1 hr) — fetches from network; falls back to stale data if all instances fail

Data is stored in `~/.config/reddit/reddit.db`.

## Discovery sources

| Source | Flag | What it does |
|--------|------|-------------|
| GitHub | always | `redlib-org/redlib-instances` JSON list |
| Codeberg | always | Mirror of the instance list |
| Farside | always | `benbusby/farside` service directory |
| Shodan | `--shodan` or `--deep` | Scans for Redlib HTTP fingerprints |
| Searx | `--searx` or `--deep` | Web search for instance lists |

## License

MIT
