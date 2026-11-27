"""The survey's method (crates/cocat-server/sql/survey.sql) on a small snapshot made here, with
the columns the server writes. The browser and the survey notebook run this same file."""

from pathlib import Path

import pytest

duckdb = pytest.importorskip("duckdb")

SURVEY = Path(__file__).resolve().parents[2] / "cocat-server" / "sql" / "survey.sql"
WRAP_S = 2**32

CATALOGUES = [
    ("public.a", "Earth bow shock crossings, Cluster (A)", "Literature/Earth", "Published lists"),
    ("public.b", "Earth bow shock crossings, Cluster (B)", "Literature/Earth", "Published lists"),
    ("public.icme", "ICME multi-catalogue", "Literature/Heliosphere", "Published lists"),
    ("public.storm", "Storm_SC", "AMDA/EARTH", "AMDA/EARTH"),
]


def snapshot(folder, events):
    """Writes catalogues.parquet and events.parquet; `events` are (catalogue, start hour,
    stop hour, observer_raw), hours counted from 2010-01-01."""
    db = duckdb.connect()
    db.execute("""CREATE TABLE c (catalogue VARCHAR, name VARCHAR, folder VARCHAR, source VARCHAR)""")
    db.executemany("INSERT INTO c VALUES (?, ?, ?, ?)", CATALOGUES)
    db.execute(f"""COPY (SELECT catalogue, name, folder, source, NULL::VARCHAR AS description,
        NULL::VARCHAR AS relay_source, NULL::VARCHAR AS relay_url, NULL::BOOLEAN AS relay_published,
        0::BIGINT AS events, []::VARCHAR[] AS attributes, NULL::VARCHAR AS attributes_file FROM c)
        TO '{folder}/catalogues.parquet' (FORMAT parquet)""")
    db.execute("CREATE TABLE e (catalogue VARCHAR, start_h DOUBLE, stop_h DOUBLE, raw VARCHAR)")
    db.executemany("INSERT INTO e VALUES (?, ?, ?, ?)", events)
    db.execute(f"""COPY (SELECT row_number() OVER (ORDER BY catalogue, start_h)::BIGINT AS event, catalogue,
        TIMESTAMP '2010-01-01' + to_microseconds((start_h * 3.6e9)::BIGINT) AS start,
        TIMESTAMP '2010-01-01' + to_microseconds((stop_h * 3.6e9)::BIGINT) AS stop,
        'confirmed' AS status, NULL::DOUBLE AS confidence, NULL::DOUBLE AS start_err,
        NULL::DOUBLE AS stop_err, []::VARCHAR[] AS tags, raw AS observer_raw FROM e)
        TO '{folder}/events.parquet' (FORMAT parquet)""")


def survey(folder):
    db = duckdb.connect()
    db.execute(f"SET file_search_path = '{folder}'")
    db.execute(SURVEY.read_text())
    return db


def rows(db, query):
    cursor = db.execute(query)
    names = [column[0] for column in cursor.description]
    return [dict(zip(names, row)) for row in cursor.fetchall()]


@pytest.fixture
def world(tmp_path):
    # A: crossings at 0, 10 and 20 h; B: one 6 min after A's first, one 30 min after its
    # second (beyond the 15 min tolerance of bow shocks), one at 30 h. Twelve shared ones
    # later make a pair that describes the same events.
    shared = [(100.0 + 24 * day, 100.0 + 24 * day) for day in range(12)]
    events = [("public.a", h, h, None) for h in (0.0, 10.0, 20.0)]
    events += [("public.b", h, h, None) for h in (0.1, 10.5, 30.0)]
    events += [("public.a", s, e, None) for s, e in shared]
    events += [("public.b", s + 0.01, e + 0.01, None) for s, e in shared]
    events += [("public.icme", 0.0, 24.0, "stereo-a"), ("public.icme", 48.0, 72.0, "Wind "),
               ("public.icme", 96.0, 120.0, "mms3")]
    # 2104 is 1968 stored as an unsigned 32-bit count of seconds.
    wrapped = (2104 - 2010) * 365.25 * 24
    events += [("public.storm", wrapped, wrapped + 1, None), ("public.storm", 5.0, 6.0, None)]
    snapshot(tmp_path, events)
    return survey(tmp_path)


def test_the_rules_tag_each_catalogue(world):
    tags = {row["catalogue"]: row for row in rows(world, "FROM catalogue_tags")}
    assert tags["public.a"]["theme"] == "Bow shocks"
    assert tags["public.a"]["observer"] == "Earth (magnetosphere)"
    assert tags["public.a"]["label"] == "Published lists: Earth bow shock crossings, Cluster (A)"
    assert tags["public.icme"]["theme"] == "ICMEs, magnetic clouds, sheaths"
    assert tags["public.storm"]["short_source"] == "AMDA"
    assert tags["public.storm"]["wrapped"]


def test_each_event_of_a_mixed_list_has_its_own_observer(world):
    found = [row["observer"] for row in rows(world, "SELECT observer FROM clean_events WHERE catalogue = 'public.icme' ORDER BY start")]
    assert found == ["STEREO-A", "L1 (near-Earth solar wind)", "Mms3"], "an unknown name keeps its own"


def test_a_wrapped_date_goes_back_two_to_the_32_seconds(world):
    starts = rows(world, """SELECT e.start AS stored, c.start AS clean FROM events e JOIN clean_events c USING (event)
                            WHERE e.catalogue = 'public.storm' ORDER BY e.start DESC""")
    assert (starts[0]["stored"] - starts[0]["clean"]).total_seconds() == WRAP_S
    assert starts[1]["stored"] == starts[1]["clean"], "a date before the limit stays"


def test_a_match_needs_an_overlap_within_the_tolerance(world):
    rate = rows(world, """FROM agreement(a := 'public.a', b := 'public.b', before := TIMESTAMP '2010-01-03')""")
    assert len(rate) == 1
    assert rate[0]["compared"] == 3
    assert rate[0]["matched"] == pytest.approx(1 / 3)
    assert rate[0]["chance"] == 0
    assert rate[0]["lift"] == pytest.approx(1 / 3 / 1e-3)


def test_pairs_that_list_the_same_events_are_found_both_ways(world):
    same = rows(world, "FROM same_events('Bow shocks')")
    assert len(same) == 1
    assert (same[0]["a_catalogue"], same[0]["b_catalogue"]) == ("public.a", "public.b")
    assert same[0]["b_in_a"] == pytest.approx(13 / 15)
    assert same[0]["events_a"] == 15


def test_events_per_year_count_by_phenomenon(world):
    years = rows(world, "FROM events_per_year('ICMEs, magnetic clouds, sheaths')")
    assert years == [{"theme": "ICMEs, magnetic clouds, sheaths", "year": 2010, "events": 3}]
