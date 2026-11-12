//! Event lists published with papers and kept by VizieR (CDS), which serves every table the
//! same way: `#` comment lines, then a header, a units row, a row of dashes and the rows, all
//! tab-separated. A list says in `part` which columns hold its times (see [`Spec`]); every
//! other column becomes an attribute.

use cocat_core::time::Timestamp;
use serde_json::{Map, Value};

use super::{Files, Item, List, slug, unix_seconds, utc, value};

/// A table as VizieR's tab-separated answer, every row and column, from CDS or its mirror at
/// Harvard. The mirror in South Africa is left out: it holds a request until it times out.
macro_rules! table {
    ($table:literal) => {
        Files::Mirrored {
            urls: &[
                concat!(
                    "https://vizier.cds.unistra.fr/viz-bin/asu-tsv?-out.all&-out.max=unlimited&-source=",
                    $table
                ),
                concat!(
                    "https://vizier.cfa.harvard.edu/viz-bin/asu-tsv?-out.all&-out.max=unlimited&-source=",
                    $table
                ),
            ],
            answered,
        }
    };
}

/// The catalogue's page at CDS.
macro_rules! page {
    ($catalogue:literal) => {
        concat!(
            "https://vizier.cds.unistra.fr/viz-bin/VizieR?-source=",
            $catalogue
        )
    };
}

/// Whom to cite: the paper, at ADS, and the VizieR catalogue.
macro_rules! credit {
    ($who:literal, $bibcode:literal, $catalogue:literal) => {
        concat!(
            $who,
            ", https://ui.adsabs.harvard.edu/abs/",
            $bibcode,
            "; VizieR catalogue ",
            $catalogue,
            " (CDS), terms at https://cds.unistra.fr/vizier-org/licences_vizier.html."
        )
    };
}

/// Which columns hold a list's times: `start=Date+Time;stop=Date+End;at=Shock`. The columns
/// of `start` and of `stop` are read together (see [`moment`]); `at` names other columns
/// holding a time, kept as attributes in RFC 3339. Without `stop` an event is a moment.
#[derive(Debug, Default, PartialEq)]
struct Spec<'a> {
    start: Vec<&'a str>,
    stop: Vec<&'a str>,
    at: Vec<&'a str>,
}

impl<'a> Spec<'a> {
    fn parse(text: &'a str) -> Result<Self, String> {
        let mut spec = Spec::default();
        for entry in text.split(';').filter(|entry| !entry.is_empty()) {
            let (key, columns) = entry
                .split_once('=')
                .ok_or_else(|| format!("bad entry {entry:?}"))?;
            let target = match key {
                "start" => &mut spec.start,
                "stop" => &mut spec.stop,
                "at" => &mut spec.at,
                _ => return Err(format!("unknown key {key:?}")),
            };
            *target = columns.split(['+', ',']).collect();
        }
        if spec.start.is_empty() {
            return Err("no start column".to_owned());
        }
        Ok(spec)
    }

    fn holds_time(&self, name: &str) -> bool {
        self.start.contains(&name) || self.stop.contains(&name)
    }
}

/// Whether VizieR answered with a table: when its database is out of reach it says so in
/// `#INFO Error=` comment lines, with HTTP 200, and an answer can also stop short.
fn answered(text: &str) -> Result<(), String> {
    if text.contains("QUERY_STATUS=ERROR") {
        let why = text
            .lines()
            .find_map(|line| line.split_once("Error=").map(|(_, why)| why.trim()))
            .unwrap_or("an error");
        return Err(format!("VizieR answered {why:?}"));
    }
    match text.lines().any(|line| line.starts_with("--")) {
        true => Ok(()),
        false => Err("VizieR's answer holds no table".to_owned()),
    }
}

/// The events of a VizieR table; `part` is the list's [`Spec`]. A row without a start is
/// left out: some tables leave a time blank, or write 0 for an unknown one.
pub(super) fn table(text: &str, part: &str) -> Result<Vec<Item>, String> {
    let spec = Spec::parse(part)?;
    let mut lines = text
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty());
    let names = cells(lines.next().ok_or("no header")?);
    let units = cells(lines.next().ok_or("no units row")?);
    let rows: Vec<Vec<&str>> = lines.skip(1).map(cells).collect();
    let column = |name: &str| {
        names
            .iter()
            .position(|header| *header == name)
            .ok_or_else(|| format!("no column {name:?}"))
    };
    let indices = |columns: &[&str]| -> Result<Vec<usize>, String> {
        columns.iter().map(|name| column(name)).collect()
    };
    let start = indices(&spec.start)?;
    let stop = indices(&spec.stop)?;
    let attributes = attribute_columns(&names, &units, &rows, &spec);
    rows.iter()
        .filter_map(|row| event(row, &start, &stop, &attributes).transpose())
        .collect()
}

fn cells(line: &str) -> Vec<&str> {
    line.split('\t').map(str::trim).collect()
}

/// How a column is kept: as a value, or as a time in RFC 3339.
enum Kept {
    Value,
    Time,
}

/// The attribute columns, as (index, attribute name, how kept). A unit qualifies a column of
/// numbers only: VizieR gives a unit to time columns too, and quotes a date's format.
fn attribute_columns(
    names: &[&str],
    units: &[&str],
    rows: &[Vec<&str>],
    spec: &Spec,
) -> Vec<(usize, String, Kept)> {
    names
        .iter()
        .enumerate()
        .filter(|(_, name)| **name != "recno" && !spec.holds_time(name))
        .map(|(index, name)| {
            let kept = if spec.at.contains(name) {
                Kept::Time
            } else {
                Kept::Value
            };
            let unit = units.get(index).copied().unwrap_or_default();
            let numbers = rows
                .iter()
                .filter_map(|row| row.get(index).filter(|cell| !cell.is_empty()))
                .all(|cell| cell.parse::<f64>().is_ok());
            let named = match slug(unit) {
                unit if numbers && !unit.is_empty() && matches!(kept, Kept::Value) => {
                    format!("{}_{unit}", slug(name))
                }
                _ => slug(name),
            };
            (index, named, kept)
        })
        .collect()
}

fn event(
    row: &[&str],
    start: &[usize],
    stop: &[usize],
    attributes: &[(usize, String, Kept)],
) -> Result<Option<Item>, String> {
    let pick = |columns: &[usize]| -> Vec<&str> {
        columns
            .iter()
            .map(|index| row.get(*index).copied().unwrap_or_default())
            .collect()
    };
    let Some(first) = moment(&pick(start))? else {
        return Ok(None);
    };
    let last = if stop.is_empty() {
        first
    } else {
        moment(&pick(stop))?.map_or(first, |last| past_midnight(first, last))
    };
    let mut kept = Map::new();
    for (index, name, how) in attributes {
        let cell = row.get(*index).copied().unwrap_or_default();
        let held = match how {
            Kept::Value => value(cell),
            Kept::Time => moment(&[cell])?
                .and_then(|at| at.rfc3339().ok())
                .map(Value::from),
        };
        if let Some(held) = held {
            kept.insert(name.clone(), held);
        }
    }
    Ok(Some(Item {
        // The tables are frozen: the whole row keys its event.
        id: row.join("|"),
        start: first,
        stop: last.max(first),
        attributes: kept,
    }))
}

/// An end time written on the start's date but before it is on the next day.
fn past_midnight(first: Timestamp, last: Timestamp) -> Timestamp {
    const DAY: f64 = 86_400.0;
    let behind = (first.unix_micros() - last.unix_micros()) as f64 / 1e6;
    match behind > 0.0 && behind < DAY {
        true => after(last, DAY).unwrap_or(last),
        false => last,
    }
}

/// A time from the cells that hold it, read by their shape:
///
/// - one cell: an ISO date and time (seconds optional), unix seconds, or `YYYY/DDD/hh:mm`;
/// - a date and a clock (`hh:mm[:ss]`, or decimal hours);
/// - a year, a day of the year and a clock (or decimal hours).
///
/// Nothing when a cell is blank, or a single cell is 0.
fn moment(pieces: &[&str]) -> Result<Option<Timestamp>, String> {
    if pieces.iter().any(|piece| piece.is_empty()) {
        return Ok(None);
    }
    let at = match pieces {
        [one] if one.bytes().all(|byte| byte == b'0') => return Ok(None),
        [one] => single(one),
        [date, clock] => midnight(date).and_then(|day| after(day, seconds(clock)?)),
        [year, doy, clock] => year_day(year, doy).and_then(|day| after(day, seconds(clock)?)),
        _ => None,
    };
    at.map(Some)
        .ok_or_else(|| format!("unreadable time {pieces:?}"))
}

fn single(text: &str) -> Option<Timestamp> {
    if text.len() >= 9 && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return unix_seconds(text.parse().ok()?);
    }
    if let [year, doy, clock] = text.split('/').collect::<Vec<_>>()[..] {
        return year_day(year, doy).and_then(|day| after(day, seconds(clock)?));
    }
    Timestamp::parse(&with_seconds(text))
}

/// `1996-08-16T07:45` as `1996-08-16T07:45:00`: RFC 3339 wants the seconds.
fn with_seconds(text: &str) -> String {
    match text.split_once('T') {
        Some((date, clock)) if clock.matches(':').count() == 1 => format!("{date}T{clock}:00"),
        _ => text.to_owned(),
    }
}

fn midnight(date: &str) -> Option<Timestamp> {
    Timestamp::parse(&format!("{date}T00:00:00Z"))
}

/// The start of a day of the year; a day past the year's end runs into the next year.
fn year_day(year: &str, doy: &str) -> Option<Timestamp> {
    let new_year = utc(year.parse().ok()?, 1, 1, 0, 0, 0.0)?;
    after(new_year, (doy.parse::<f64>().ok()? - 1.0) * 86_400.0)
}

/// A clock as seconds after midnight: `hh:mm[:ss.s]`, or decimal hours.
fn seconds(clock: &str) -> Option<f64> {
    if !clock.contains(':') {
        return Some(clock.parse::<f64>().ok()? * 3_600.0);
    }
    clock
        .split(':')
        .zip([3_600.0, 60.0, 1.0])
        .map(|(part, scale)| part.parse::<f64>().ok().map(|part| part * scale))
        .sum()
}

fn after(at: Timestamp, seconds: f64) -> Option<Timestamp> {
    Timestamp::from_unix_micros(at.unix_micros() + (seconds * 1e6).round() as i128)
}

pub(super) const LISTS: [List; 27] = [
    List {
        key: "vizier-cme-source-regions-yeates",
        folder: "Literature/Sun",
        name: "Low coronal sources of LASCO CMEs (Yeates et al. 2010)",
        about: "330 LASCO CMEs at their LASCO time, with the longitude and latitude range of their low coronal EUV source, its quality (1 best) and quadrant, the CME's central position angle and width.",
        credit: credit!(
            "Yeates et al. (2010), ApJ 709, 1238",
            "2010ApJ...709.1238Y",
            "J/ApJ/709/1238"
        ),
        url: page!("J/ApJ/709/1238"),
        files: table!("J/ApJ/709/1238/table5"),
        read: table,
        part: "start=Obs",
    },
    List {
        key: "vizier-ip-shocks-gopalswamy",
        folder: "Literature/Heliosphere",
        name: "Interplanetary shocks of solar cycle 23 and their solar sources (Gopalswamy et al. 2010)",
        about: "230 shocks of solar cycle 23 at Wind, ACE or SOHO, at their arrival, with their speed and Alfvén Mach number, the driving ICME's arrival, the CME's date, position angle, width, speed and acceleration, the flare's location, duration and size, and metric and DH type II bursts.",
        credit: credit!(
            "Gopalswamy et al. (2010), ApJ 710, 1111",
            "2010ApJ...710.1111G",
            "J/ApJ/710/1111"
        ),
        url: page!("J/ApJ/710/1111"),
        files: table!("J/ApJ/710/1111/table1"),
        read: table,
        part: "start=IP",
    },
    List {
        key: "vizier-type2-learmonth-lobzin",
        folder: "Literature/Sun",
        name: "Coronal type II radio bursts at Learmonth in 2002 (Lobzin et al. 2010)",
        about: "60 metric type II bursts seen by the Learmonth spectrograph in 2002, from start to end, with their intensity, start and end frequencies, and how an automatic detection did on each day.",
        credit: credit!(
            "Lobzin et al. (2010), ApJ 710, L58",
            "2010ApJ...710L..58L",
            "J/ApJ/710/L58"
        ),
        url: page!("J/ApJ/710/L58"),
        files: table!("J/ApJ/710/L58/table1"),
        read: table,
        part: "start=Obs+Stime;stop=Obs+Etime",
    },
    List {
        key: "vizier-flares-aia-aschwanden",
        folder: "Literature/Sun",
        name: "M and X flares seen by GOES and AIA (Aschwanden 2012)",
        about: "155 M- and X-class flares from GOES start to end, with their peak time, class, NOAA region, position, and the length scale and diffusion parameters fitted to their AIA images.",
        credit: credit!(
            "Aschwanden (2012), ApJ 757, 94",
            "2012ApJ...757...94A",
            "J/ApJ/757/94"
        ),
        url: page!("J/ApJ/757/94"),
        files: table!("J/ApJ/757/94/table1"),
        read: table,
        part: "start=Date+Tst;stop=Date+Tend",
    },
    List {
        key: "vizier-electron-events-wang",
        folder: "Literature/Heliosphere",
        name: "Solar energetic electron events at Wind/3DP, 1995-2005 (Wang et al. 2012)",
        about: "1191 electron events at the time of their solar injection, with the energy range, whether ³He-rich, type III and II bursts, the associated CME and GOES flare, and whether a >10 MeV proton event followed.",
        credit: credit!(
            "Wang et al. (2012), ApJ 759, 69",
            "2012ApJ...759...69W",
            "J/ApJ/759/69"
        ),
        url: page!("J/ApJ/759/69"),
        files: table!("J/ApJ/759/69/table2"),
        read: table,
        part: "start=Date+Obs.h",
    },
    List {
        key: "vizier-goes-xray-magnetosphere-yamamoto",
        folder: "Literature/Earth",
        name: "X-ray events in Earth's magnetosphere seen by GOES (Yamamoto et al. 2013)",
        about: "217 X-ray events GOES 11 to 14 saw in Earth's magnetosphere from 2006, from start to end, with their peak and the satellite.",
        credit: credit!(
            "Yamamoto et al. (2013), ApJ 775, 121",
            "2013ApJ...775..121Y",
            "J/ApJ/775/121"
        ),
        url: page!("J/ApJ/775/121"),
        files: table!("J/ApJ/775/121/table2"),
        read: table,
        part: "start=Start;stop=End",
    },
    List {
        key: "vizier-flare-qpp-inglis",
        folder: "Literature/Sun",
        name: "Quasi-periodic pulsations in M and X flares (Inglis et al. 2016)",
        about: "1000 GOES and Fermi/GBM observations of M- and X-class flares, 2011-2015, over the interval analysed, with the model comparison (BIC, χ², p-values) and the period of the best model when pulsations were found.",
        credit: credit!(
            "Inglis et al. (2016), ApJ 833, 284",
            "2016ApJ...833..284I",
            "J/ApJ/833/284"
        ),
        url: page!("J/ApJ/833/284"),
        files: table!("J/ApJ/833/284/events"),
        read: table,
        part: "start=Obs+StObs;stop=Obs+EndObs",
    },
    List {
        key: "vizier-white-light-flares-namekata",
        folder: "Literature/Sun",
        name: "White-light flares (Namekata et al. 2017)",
        about: "50 white-light flares from start to end, with their GOES class and peak, location, radiated energy, decay times, estimated and observed magnetic fields and loop lengths, and RHESSI hard X-ray area.",
        credit: credit!(
            "Namekata et al. (2017), ApJ 851, 91",
            "2017ApJ...851...91N",
            "J/ApJ/851/91"
        ),
        url: page!("J/ApJ/851/91"),
        files: table!("J/ApJ/851/91/flares"),
        read: table,
        part: "start=st;stop=end",
    },
    List {
        key: "vizier-icmes-ace-wind-feng",
        folder: "Literature/Heliosphere",
        name: "ICMEs seen by both ACE and Wind, 1998-2011 (Feng et al. 2018)",
        about: "219 ICMEs from start to end at ACE, with the shock time, the lowest and highest speeds, and whether a magnetic cloud.",
        credit: credit!(
            "Feng et al. (2018), ApJ 868, 124",
            "2018ApJ...868..124F",
            "J/ApJ/868/124"
        ),
        url: page!("J/ApJ/868/124"),
        files: table!("J/ApJ/868/124/table6"),
        read: table,
        part: "start=Start;stop=End",
    },
    List {
        key: "vizier-flares-cmes-seps-ling",
        folder: "Literature/Sun",
        name: "X-ray flares above M3 with their CMEs and SEPs (Ling et al. 2020)",
        about: "451 flares above M3 from 1998 to 2017 at their peak, with class, location, peak X-ray ratio, the associated CME's speed and width, and the SEP event's onset and peak intensity.",
        credit: credit!(
            "Ling et al. (2020), ApJ 891, 54",
            "2020ApJ...891...54L",
            "J/ApJ/891/54"
        ),
        url: page!("J/ApJ/891/54"),
        files: table!("J/ApJ/891/54/table3"),
        read: table,
        part: "start=Event+Peak",
    },
    List {
        key: "vizier-streamer-waves-decraemer",
        folder: "Literature/Sun",
        name: "Streamer waves seen by STEREO/COR2 (Decraemer et al. 2020)",
        about: "22 streamer wave events from 2008 to 2017 at their start, with the coronagraph, position angle, period, wavelength and phase speed, and the CME that set them off.",
        credit: credit!(
            "Decraemer et al. (2020), ApJ 893, 78",
            "2020ApJ...893...78D",
            "J/ApJ/893/78"
        ),
        url: page!("J/ApJ/893/78"),
        files: table!("J/ApJ/893/78/table1"),
        read: table,
        part: "start=Obs",
    },
    List {
        key: "vizier-forbush-icme-light",
        folder: "Literature/Heliosphere",
        name: "Forbush decreases from ICMEs, 2001-2019 (Light et al. 2020)",
        about: "ICME-driven Forbush decreases seen by at least 4 neutron monitors, over the ICME's magnetic cloud, with the shock's arrival and the dip size at each of 12 monitors.",
        credit: credit!(
            "Light et al. (2020), ApJ 896, 133",
            "2020ApJ...896..133L",
            "J/ApJ/896/133"
        ),
        url: page!("J/ApJ/896/133"),
        files: table!("J/ApJ/896/133/table3"),
        read: table,
        part: "start=Start;stop=End;at=Shock",
    },
    List {
        key: "vizier-icme-heavy-ions-gu",
        folder: "Literature/Heliosphere",
        name: "Heavy ions in ICMEs at ACE, 1998-2011 (Gu et al. 2020)",
        about: "281 ICMEs at their start, with the mean speed, He/O, Fe/O, Mg/O and O⁷⁺/O⁶⁺ ratios, mean charge states of C, O and Fe, the oxygen flux, and their charge distribution type.",
        credit: credit!(
            "Gu et al. (2020), ApJ 900, 123",
            "2020ApJ...900..123G",
            "J/ApJ/900/123"
        ),
        url: page!("J/ApJ/900/123"),
        files: table!("J/ApJ/900/123/table2"),
        read: table,
        part: "start=Obs",
    },
    List {
        key: "vizier-halo-cmes-dagnew",
        folder: "Literature/Sun",
        name: "Halo CMEs seen by SOHO/LASCO, 1996-2018 (Dagnew et al. 2020)",
        about: "715 CMEs at their first LASCO appearance, with sky and space speeds, position angle, source latitude and longitude, flare size and onset, and the Carrington rotation.",
        credit: credit!(
            "Dagnew et al. (2020), ApJ 903, 118",
            "2020ApJ...903..118D",
            "J/ApJ/903/118"
        ),
        url: page!("J/ApJ/903/118"),
        files: table!("J/ApJ/903/118/fig1"),
        read: table,
        part: "start=CME",
    },
    List {
        key: "vizier-forbush-ephin-belov",
        folder: "Literature/Heliosphere",
        name: "Forbush decreases seen by SOHO/EPHIN, 1995-2015 (Belov et al. 2021)",
        about: "421 Forbush decreases at their onset, with the amplitude at EPHIN and at neutron monitors, the spectral index, the structure that caused them (CIR or ICME) and whether two-step.",
        credit: credit!(
            "Belov et al. (2021), ApJ 908, 5",
            "2021ApJ...908....5B",
            "J/ApJ/908/5"
        ),
        url: page!("J/ApJ/908/5"),
        files: table!("J/ApJ/908/5/table1"),
        read: table,
        part: "start=Date+Time",
    },
    List {
        key: "vizier-icmes-juno-davies",
        folder: "Literature/Heliosphere",
        name: "ICMEs at Juno between 1 and 5.4 AU (Davies et al. 2021)",
        about: "82 ICMEs on Juno's cruise to Jupiter from start to end, with Juno's position, the mean and largest field, the flux rope's interval, type and handedness, and the quality (1 best).",
        credit: credit!(
            "Davies et al. (2021), ApJ 923, 136",
            "2021ApJ...923..136D",
            "J/ApJ/923/136"
        ),
        url: page!("J/ApJ/923/136"),
        files: table!("J/ApJ/923/136/table1"),
        read: table,
        part: "start=ICME-Start;stop=ICME-End",
    },
    List {
        key: "vizier-multipoint-cmes-mostl",
        folder: "Literature/Heliosphere",
        name: "CMEs seen at several points, 2020-2021 (Möstl et al. 2022)",
        about: "45 sightings of 17 CMEs, in heliospheric images or in situ, with the spacecraft, its distance and longitude, the CME's longitude and speed, the field, and the ICMECAT or HIGeoCAT id.",
        credit: credit!(
            "Möstl et al. (2022), ApJ 924, L6",
            "2022ApJ...924L...6M",
            "J/ApJ/924/L6"
        ),
        url: page!("J/ApJ/924/L6"),
        files: table!("J/ApJ/924/L6/table1"),
        read: table,
        part: "start=Start",
    },
    List {
        key: "vizier-multi-spacecraft-icmes-davies",
        folder: "Literature/Heliosphere",
        name: "ICMEs seen by Juno and an inner spacecraft (Davies et al. 2022)",
        about: "42 ICMEs seen first by a spacecraft near the Sun and again by Juno: the event is the magnetic ejecta at the first spacecraft, with both sightings' boundaries, positions and fields, and the speeds at the first.",
        credit: credit!(
            "Davies et al. (2022), ApJ 933, 127",
            "2022ApJ...933..127D",
            "J/ApJ/933/127"
        ),
        url: page!("J/ApJ/933/127"),
        files: table!("J/ApJ/933/127/table1"),
        read: table,
        part: "start=SC1-ME-St1;stop=SC1-ME-End1",
    },
    List {
        key: "vizier-limb-cmes-23-dagnew",
        folder: "Literature/Sun",
        name: "Limb CMEs of solar cycle 23 (Dagnew et al. 2022)",
        about: "600 limb CMEs from 1996 to 2008 at their LASCO appearance, with width, speed, position angle, the transition height and the height where their width stops growing, and the flare's onset, peak, size, location and region.",
        credit: credit!(
            "Dagnew et al. (2022), ApJ 936, 122",
            "2022ApJ...936..122D",
            "J/ApJ/936/122"
        ),
        url: page!("J/ApJ/936/122"),
        files: table!("J/ApJ/936/122/table1"),
        read: table,
        part: "start=CME",
    },
    List {
        key: "vizier-limb-cmes-24-dagnew",
        folder: "Literature/Sun",
        name: "Limb CMEs of solar cycle 24 (Dagnew et al. 2022)",
        about: "406 limb CMEs from 2009 to 2019 at their LASCO appearance, with width, speed, position angle, the transition height and the height where their width stops growing, and the flare's onset, peak, size, location and region.",
        credit: credit!(
            "Dagnew et al. (2022), ApJ 936, 122",
            "2022ApJ...936..122D",
            "J/ApJ/936/122"
        ),
        url: page!("J/ApJ/936/122"),
        files: table!("J/ApJ/936/122/table2"),
        read: table,
        part: "start=CME",
    },
    List {
        key: "vizier-extremely-slow-wind-tsurutani",
        folder: "Literature/Heliosphere",
        name: "Extremely slow solar wind, 1963-2021 (Tsurutani et al. 2022)",
        about: "297 intervals of extremely slow solar wind from 1963 to 2021, from start to end, to the hour.",
        credit: credit!(
            "Tsurutani et al. (2022), ApJ 936, 155",
            "2022ApJ...936..155T",
            "J/ApJ/936/155"
        ),
        url: page!("J/ApJ/936/155"),
        files: table!("J/ApJ/936/155/tables1"),
        read: table,
        part: "start=Obs.Y-s+Obs.d-s+Obs.H-S;stop=Obs.Y-e+Obs.d-e+Obs.H-e",
    },
    List {
        key: "vizier-shocks-high-energy-protons-lario",
        folder: "Literature/Heliosphere",
        name: "Shocks during enhanced ~40 MeV protons near Earth (Lario et al. 2023)",
        about: "191 shock passages at IMP 8, ACE or Wind during high-energy proton events, with the parent solar eruption, transit speed, θBn, shock speeds, density compression and Mach numbers, with their uncertainties.",
        credit: credit!(
            "Lario et al. (2023), ApJ 950, 89",
            "2023ApJ...950...89L",
            "J/ApJ/950/89"
        ),
        url: page!("J/ApJ/950/89"),
        files: table!("J/ApJ/950/89/table1"),
        read: table,
        part: "start=Time",
    },
    List {
        key: "vizier-eit-waves-thompson",
        folder: "Literature/Sun",
        name: "Coronal EIT waves seen by SOHO/EIT (Thompson et al. 2009)",
        about: "EIT wave transients at the first image showing the wave front, with the quality (5 best), apparent source location, plane-of-sky and projected speeds, and direction.",
        credit: credit!(
            "Thompson et al. (2009), ApJS 183, 225",
            "2009ApJS..183..225T",
            "J/ApJS/183/225"
        ),
        url: page!("J/ApJS/183/225"),
        files: table!("J/ApJS/183/225/table3"),
        read: table,
        part: "start=Date+t(ima)",
    },
    List {
        key: "vizier-voyager-pickup-ion-waves-hollick",
        folder: "Literature/Heliosphere",
        name: "Waves from newborn interstellar pickup ions at Voyager 1 and 2 (Hollick et al. 2018)",
        about: "636 intervals from 1977 to 1990 checked for waves of newborn pickup H⁺ and He⁺, with the spacecraft, the field data used, the distance from the Sun, whether each wave is present and its ellipticity.",
        credit: credit!(
            "Hollick et al. (2018), ApJS 237, 34",
            "2018ApJS..237...34H",
            "J/ApJS/237/34"
        ),
        url: page!("J/ApJS/237/34"),
        files: table!("J/ApJS/237/34/waves"),
        read: table,
        part: "start=Year+str-DOY+str-time;stop=Year+end-DOY+end-time",
    },
    List {
        key: "vizier-lyman-alpha-flares-lu",
        folder: "Literature/Sun",
        name: "Flares in GOES-15 Lyman-α, 2010-2016 (Lu et al. 2021)",
        about: "2038 flares seen in GOES-15 EUVS Lyman-α from start to end, with peak time, duration, start, peak and end fluxes, and how far the peak rises.",
        credit: credit!(
            "Lu et al. (2021), ApJS 253, 29",
            "2021ApJS..253...29L",
            "J/ApJS/253/29"
        ),
        url: page!("J/ApJS/253/29"),
        files: table!("J/ApJS/253/29/table1"),
        read: table,
        part: "start=Obs+Start;stop=Obs+End",
    },
    List {
        key: "vizier-he3-rich-periods-hart",
        folder: "Literature/Heliosphere",
        name: "³He-rich periods at ACE and STEREO, 1997-2021 (Hart et al. 2022)",
        about: "854 ³He-rich periods from start to end, with the spacecraft and the ³He/⁴He and Fe/O ratios in two energy ranges, with their uncertainties.",
        credit: credit!(
            "Hart et al. (2022), ApJS 263, 22",
            "2022ApJS..263...22H",
            "J/ApJS/263/22"
        ),
        url: page!("J/ApJS/263/22"),
        files: table!("J/ApJS/263/22/table1"),
        read: table,
        part: "start=Start;stop=Stop",
    },
    List {
        key: "vizier-cme-transit-times-sudar",
        folder: "Literature/Heliosphere",
        name: "CME transit times from the Sun to Earth (Sudar et al. 2016)",
        about: "153 CMEs from their start to their arrival at Earth, with transit time, speed, and source latitude and central meridian distance.",
        credit: credit!(
            "Sudar et al. (2016), MNRAS 456, 1542",
            "2016MNRAS.456.1542S",
            "J/MNRAS/456/1542"
        ),
        url: page!("J/MNRAS/456/1542"),
        files: table!("J/MNRAS/456/1542/table1"),
        read: table,
        part: "start=ObsS;stop=ObsA",
    },
];

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/relay/vizier_{name}.tsv",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(path).unwrap()
    }

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    #[test]
    fn an_iso_time_without_seconds_is_a_moment_and_units_name_numbers_only() {
        let items = table(&fixture("gopalswamy_shocks"), "start=IP").unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].start, at("1996-08-16T07:45:00Z"));
        assert_eq!(items[0].stop, items[0].start);
        assert_eq!(items[0].attributes["ip_vel_km_s"], json!(390.5));
        assert_eq!(items[0].attributes["inst"], json!("W"));
        assert_eq!(items[0].attributes["cme"], json!("1996-08-14T19:30"));
        assert!(!items[0].attributes.contains_key("recno"));
    }

    #[test]
    fn a_date_and_clocks_give_the_start_and_the_stop() {
        let items = table(&fixture("lobzin_type2"), "start=Obs+Stime;stop=Obs+Etime").unwrap();
        assert_eq!(items[0].start, at("2002-01-03T02:21:00Z"));
        assert_eq!(items[0].stop, at("2002-01-03T02:29:00Z"));
        assert_eq!(items[0].attributes["sfreq_mhz"], json!(31.0));
        assert!(!items[0].attributes.contains_key("obs"));
    }

    #[test]
    fn decimal_hours_count_from_the_date() {
        let items = table(&fixture("wang_electrons"), "start=Date+Obs.h").unwrap();
        assert_eq!(items[0].start, at("1995-01-02T16:36:00Z"));
    }

    #[test]
    fn a_year_a_day_of_the_year_and_an_hour_make_a_time_and_hour_0_is_midnight() {
        let spec = "start=Obs.Y-s+Obs.d-s+Obs.H-S;stop=Obs.Y-e+Obs.d-e+Obs.H-e";
        let items = table(&fixture("tsurutani_essw"), spec).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].start, at("1963-11-27T12:00:00Z"));
        assert_eq!(items[0].stop, at("1963-11-29T23:00:00Z"));
        assert_eq!(items[2].start, at("1963-12-17T00:00:00Z"));
    }

    #[test]
    fn a_slashed_year_day_and_clock_is_one_time() {
        let items = table(&fixture("lario_shocks"), "start=Time").unwrap();
        assert_eq!(items[0].start, at("1974-07-06T03:14:00Z"));
    }

    #[test]
    fn unix_seconds_read_as_times_and_a_zero_one_is_missing() {
        let items = table(&fixture("light_forbush"), "start=Start;stop=End;at=Shock").unwrap();
        assert_eq!(items.len(), 2, "the row whose start is 0 is left out");
        assert_eq!(items[0].start, at("2001-01-24T03:00:00Z"));
        assert_eq!(items[0].stop, at("2001-01-24T22:00:00Z"));
        assert_eq!(items[0].attributes["shock"], json!("2001-01-23T11:00:00Z"));
        assert!(!items[1].attributes.contains_key("shock"));
    }

    #[test]
    fn an_end_clock_before_the_start_clock_is_on_the_next_day() {
        assert_eq!(
            past_midnight(at("2002-01-03T23:50:00Z"), at("2002-01-03T00:10:00Z")),
            at("2002-01-04T00:10:00Z")
        );
    }

    #[test]
    fn a_column_the_spec_names_must_exist() {
        let error = table(&fixture("lobzin_type2"), "start=Nope").unwrap_err();
        assert!(error.contains("Nope"), "{error}");
    }

    #[test]
    fn an_answer_saying_the_database_is_out_of_reach_is_no_table() {
        let down =
            "#INFO\tError=The database is not currently reachable.\t\n#INFO\tQUERY_STATUS=ERROR\n";
        let error = answered(down).unwrap_err();
        assert!(error.contains("not currently reachable"), "{error}");
        assert!(
            answered("#RESOURCE=\n#Name:\n").is_err(),
            "an answer cut short"
        );
        assert!(answered(&fixture("lobzin_type2")).is_ok());
    }

    #[test]
    fn every_list_says_which_columns_hold_its_start() {
        for list in &LISTS {
            let spec = Spec::parse(list.part).unwrap();
            assert!(!spec.start.is_empty(), "{}", list.key);
        }
    }
}
