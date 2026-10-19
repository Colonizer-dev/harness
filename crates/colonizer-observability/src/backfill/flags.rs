//! The `backfill` subcommand's flags (#858): `--since`, `--until`, `--repo`, `--rate`,
//! `--dry-run`, `--include-archive`. A module of the engine's own ([`super`]); the mothership's
//! CLI mirrors these spellings when it invokes the one-shot, and the offline `export` shares the
//! window's grammar.

/// Records a second when `--rate` is absent or 0.
pub(crate) const DEFAULT_RATE: u64 = 500;

/// What the operator asked for, parsed by [`Options::from_args`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Unix nanos: replay lines with `since <= ts` (inclusive).
    pub since: Option<u64>,
    /// Unix nanos: replay lines with `ts < until` (exclusive).
    pub until: Option<u64>,
    /// Only colonies of this `owner/name` repository (case-insensitive).
    pub repo: Option<String>,
    /// Records a second; 0 means the default.
    pub rate: u64,
    /// Count what would be sent, sending and writing nothing.
    pub dry_run: bool,
    /// Also replay the archive bundles of colonies the disk no longer holds (#859).
    pub include_archive: bool,
}

impl Options {
    /// Parses `--since <when> --until <when> --repo <repo> --rate <n> --dry-run --include-archive`,
    /// skipping the `--contract <path>` pair the caller has already taken. An unknown flag or a
    /// missing value is an error.
    pub fn from_args(args: &[String]) -> Result<Options, String> {
        let mut opts = Options {
            rate: DEFAULT_RATE,
            ..Options::default()
        };
        let mut i = 0;
        while i < args.len() {
            let flag = args[i].as_str();
            if flag == "--dry-run" {
                opts.dry_run = true;
                i += 1;
                continue;
            }
            if flag == "--include-archive" {
                opts.include_archive = true;
                i += 1;
                continue;
            }
            let value = args.get(i + 1).ok_or_else(|| format!("{flag} needs a value"))?.clone();
            i += 2;
            match flag {
                // The contract path is the caller's business (every subcommand takes it).
                "--contract" => {}
                "--since" => opts.since = Some(parse_when(&value, false)?),
                "--until" => opts.until = Some(parse_when(&value, true)?),
                "--repo" => opts.repo = Some(value),
                "--rate" => {
                    opts.rate = value
                        .parse()
                        .map_err(|_| format!("--rate needs a number of records a second, not {value}"))?;
                }
                other => return Err(format!("unknown flag {other}")),
            }
        }
        Ok(opts)
    }
}

/// An RFC 3339 instant, a naive `YYYY-MM-DDTHH:MM:SS` (read as UTC), or a bare `YYYY-MM-DD` day —
/// its start, or with `end_of_day` its end. The grammar is the mothership's own
/// (`crates/colonizer/src/history.rs`); the answer is Unix nanoseconds. The offline `export`
/// shares it for its own `--since`/`--until`.
pub(crate) fn parse_when(raw: &str, end_of_day: bool) -> Result<u64, String> {
    let at = if let Ok(at) = chrono::DateTime::parse_from_rfc3339(raw) {
        at.with_timezone(&chrono::Utc)
    } else if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S") {
        naive.and_utc()
    } else {
        let day = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
            .map_err(|_| format!("{raw} is not a date the backfill understands: RFC 3339, YYYY-MM-DDTHH:MM:SS or YYYY-MM-DD"))?;
        let time = if end_of_day {
            day.and_hms_milli_opt(23, 59, 59, 999)
        } else {
            day.and_hms_opt(0, 0, 0)
        }
        .ok_or("that day does not exist")?;
        time.and_utc()
    };
    let nanos = at.timestamp_nanos_opt().ok_or("the date is out of range")?;
    u64::try_from(nanos).map_err(|_| format!("{raw} is before 1970"))
}
