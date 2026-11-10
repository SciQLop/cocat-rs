//! The rolling window of providers that revise recent events: a list's first run reads its
//! whole history, request by request; later runs read the last 90 days only, and what the
//! list holds from before stays as it is.

use cocat_core::time::Timestamp;
use time::{Date, Duration, Month};

const WINDOW: Duration = Duration::days(90);

/// Where this run starts reading a list: its whole history when its catalogue was never
/// refreshed, else the window, or further back when the last refresh is older than that.
fn first_day(
    refreshed: Option<Timestamp>,
    now: Timestamp,
    history_start: Date,
) -> Result<Date, String> {
    let Some(refreshed) = refreshed else {
        return Ok(history_start);
    };
    Ok(date_of(refreshed)?.min(date_of(window_start(now)?)?))
}

/// The requests of a run, as first and last day, each reading `span` days at most, ends
/// included.
pub fn windows(
    refreshed: Option<Timestamp>,
    now: Timestamp,
    history_start: Date,
    span: Duration,
) -> Result<Vec<(Date, Date)>, String> {
    let last_day = date_of(now)?;
    let first = first_day(refreshed, now, history_start)?;
    let spans = std::iter::successors(Some(first), |start| Some(*start + span + Duration::DAY))
        .take_while(|start| *start <= last_day);
    Ok(spans
        .map(|start| (start, (start + span).min(last_day)))
        .collect())
}

pub fn window_start(now: Timestamp) -> Result<Timestamp, String> {
    let micros = now.unix_micros() - WINDOW.whole_microseconds();
    Timestamp::from_unix_micros(micros).ok_or_else(|| "the window starts out of range".to_owned())
}

pub fn date_of(at: Timestamp) -> Result<Date, String> {
    let parts = at.parts();
    let month = Month::try_from(parts.month).map_err(|error| error.to_string())?;
    Date::from_calendar_date(parts.year, month, parts.day).map_err(|error| error.to_string())
}
