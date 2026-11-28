-- The public catalogue survey's method, in one file that both engines run: DuckDB in the
-- browser (the SQL tools of the web app) and DuckDB in Python (the survey notebook behind
-- https://sciqlop.lpp.polytechnique.fr/catalogues/). So both give the same numbers.
-- Design: docs/design/browser-sql-analysis.md, section 2.
--
-- It names no file. Before it runs, the host defines the snapshot's tables from the files it
-- has: views `catalogues` and `events`, and a table macro `attribute_rows(id)` giving one
-- catalogue's attribute file, both from the Parquet files.
-- Times are UTC.
--
-- The rules are data: ordered regular expressions (RE2), the first match wins. When a
-- catalogue is misplaced, add a rule or an override, then check that no other one moved.
-- Traps seen: "magnetosheath" holds "sheath" (ICMEs) and "foreshock" holds "shock", so the
-- jet, transient and region rules come first; per-event spacecraft names like THEMIS-C need
-- an observer_names rule.

CREATE OR REPLACE TABLE theme_rules AS
SELECT rank::INTEGER AS rank, pattern::VARCHAR AS pattern, theme::VARCHAR AS theme FROM (VALUES
    (1, 'mms_burst|burst segment|sitl', 'Operations: MMS burst selections'),
    (2, 'magnetosheath jets', 'Magnetosheath jets'),
    (3, 'upstream structures \(tums\)', 'Foreshock transients'),
    (4, 'foreshock bubbles', 'Foreshock transients'),
    (5, 'lpp run of the olshevsky|turbulent magnetosheath|magnetosheath turbulence intervals|subsolar magnetosheath intervals', 'Plasma regions (where the spacecraft is)'),
    (6, 'emic wave', 'Plasma waves'),
    (7, 'switchback', 'Solar wind features'),
    (8, 'ion composition boundary', 'Magnetopauses'),
    (9, 'reconnecting current sheet|flux transfer', 'Current sheets, reconnection, flux ropes'),
    (10, 'quietest|disturbed days|ck-days', 'Quiet and disturbed days'),
    (11, 'sudden commencement', 'Geomagnetic activity'),
    (12, 'forbush', 'Forbush decreases'),
    (13, 'eit wave|streamer wave', 'Coronal waves'),
    (14, 'energetic electron|he-rich', 'Solar energetic particles'),
    (15, 'slow solar wind', 'Solar wind features'),
    (16, 'orbit_numbers|carrington_rot|flyby|opportunit|conjonction|apoapsis|hst|coordinated obs|lineupcat|windows_of|titan_around|datamining', 'Operations: orbits, flybys, coordination'),
    (17, 'plasmaregimes|model_regions|grmb', 'Plasma regions (where the spacecraft is)'),
    (18, 'arrcat', 'CME arrivals (predicted)'),
    (19, 'bow.?shock|bs_?crossings|_bs|bowshock|bow_cluster|mavenshock|mexshock|mgsshock|shock crossings, maven|bow shock', 'Bow shocks'),
    (20, 'magnetopause|mp_?crossings|mpx|_mp_', 'Magnetopauses'),
    (21, 'icme|magnetic.?cloud|sheath|interplanetary magnetic clouds', 'ICMEs, magnetic clouds, sheaths'),
    (22, 'cme|coronal mass|higeocat|linkcat|lasco', 'CMEs (coronagraph, imagers)'),
    (23, 'shock', 'Interplanetary shocks'),
    (24, '\bsir|_sir|sircat|cir|stream.?interface|high.?speed|speed streams|ace_v_600|interaction regions', 'SIRs, CIRs, fast streams'),
    (25, 'flare|stix|eui|spice', 'Flares and solar events'),
    (26, 'type ii|radio|akr', 'Radio bursts'),
    (27, 'sep|proton|energetic particle', 'Solar energetic particles'),
    (28, 'mirror mode', 'Mirror mode structures'),
    (29, 'storm|dst|substorm|radiation belt|rb-events|aurora', 'Geomagnetic activity'),
    (30, 'flux.?rope|fte|exhaust|reconnection|edr|current sheet|hcs|jets|mirror|tail_hall', 'Current sheets, reconnection, flux ropes'),
    (31, 'bz |ey direction|sector boundar|miscellaneous|low speed', 'Solar wind features')
) AS rows(rank, pattern, theme);

CREATE OR REPLACE TABLE observer_rules AS
SELECT rank::INTEGER AS rank, pattern::VARCHAR AS pattern, observer::VARCHAR AS observer FROM (VALUES
    (1, 'istp catalog|interplanetary shocks, themis', 'L1 (near-Earth solar wind)'),
    (2, 'near.?earth|llamaice|forbush|sector boundar|goes|sep events above', 'L1 (near-Earth solar wind)'),
    (3, 'icmes at juno', 'Juno (cruise, 1-5.4 au)'),
    (4, 'voyager', 'Voyager 1 and 2'),
    (5, 'transit times from the sun', 'Sun (remote sensing)'),
    (6, 'eit wave|streamer wave', 'Sun (remote sensing)'),
    (7, 'stereo.?a\b|sta_|_sta\b|shockssta', 'STEREO-A'),
    (8, 'stereo.?b\b|stb_|shocksstb', 'STEREO-B'),
    (9, 'parker|psp', 'Parker Solar Probe'),
    (10, 'solar orbiter|solo|\bso_', 'Solar Orbiter'),
    (11, 'ulysses', 'Ulysses'),
    (12, '\bhelios\b', 'Helios'),
    (13, 'bepi', 'BepiColombo'),
    (14, 'juice', 'JUICE'),
    (15, 'messenger|mercury', 'Mercury'),
    (16, 'venus|vex', 'Venus'),
    (17, 'mars|maven|mex|mgs', 'Mars'),
    (18, 'jupiter|juno|europa|ganymede|callisto|\bio\b', 'Jupiter'),
    (19, 'saturn|cassini|titan|enceladus', 'Saturn'),
    (20, 'earth|cluster|themis|mms|geotail|imp 8|artemis|substorm|storm|dst|aurora|radiation belt|rb-events|magnetopause crossings|donki/magnetopause', 'Earth (magnetosphere)'),
    (21, 'lasco|cactus|\bcme|coronal mass|higeocat|linkcat|flare|stix|eui|spice|hinode|iris|carrington_rot|type ii', 'Sun (remote sensing)'),
    (22, 'wind|\bace\b|omni|dscovr|istp|l1|onera|solar_wind|richardson|nguyen|gosling|borovsky|cartwright|magnetic_clouds|sampex|1au|serpentine|donki|interplanetary|ipshocks|solar_proton', 'L1 (near-Earth solar wind)')
) AS rows(rank, pattern, observer);

CREATE OR REPLACE TABLE observer_names AS
SELECT rank::INTEGER AS rank, pattern::VARCHAR AS pattern, observer::VARCHAR AS observer FROM (VALUES
    (1, '^(wind|ace|omni|dscovr|imp.?8|earth|l1|soho|cluster)', 'L1 (near-Earth solar wind)'),
    (2, '^(themis|rbsp)', 'Earth (magnetosphere)'),
    (3, '^voyager.?1', 'Voyager 1'),
    (4, '^voyager.?2', 'Voyager 2'),
    (5, '^(a|sta|stereo.?a|stereo-a|stereo a)$', 'STEREO-A'),
    (6, '^(b|stb|stereo.?b|stereo-b|stereo b)$', 'STEREO-B'),
    (7, '^(psp|parker)', 'Parker Solar Probe'),
    (8, '^(so|solo|solarorbiter|solar orbiter)$', 'Solar Orbiter'),
    (9, '^ulysses', 'Ulysses'),
    (10, '^helios', 'Helios'),
    (11, '^bepi', 'BepiColombo'),
    (12, '^(messenger|mercury)', 'Mercury'),
    (13, '^(vex|venus)', 'Venus'),
    (14, '^(maven|mars)', 'Mars')
) AS rows(rank, pattern, observer);

CREATE OR REPLACE TABLE overrides AS
SELECT name::VARCHAR AS name, theme::VARCHAR AS theme, observer::VARCHAR AS observer FROM (VALUES
    ('LineupCAT', 'Operations: orbits, flybys, coordination', 'Many'),
    ('LINKCAT, CMEs linked to their ICMEs (HELCATS)', 'CMEs (coronagraph, imagers)', 'Sun (remote sensing)'),
    ('Solar energetic particles', NULL, 'L1 (near-Earth solar wind)'),
    ('WAVES type II bursts', NULL, 'Sun (remote sensing)'),
    ('AKR bursts, Wind/WAVES 1995-2004 (Fogg et al.)', NULL, 'Earth (magnetosphere)'),
    ('Changes of the Ey direction (ISTP catalog, Wind and IMP 8)', 'Solar wind features', NULL),
    ('Low speed streams (ISTP catalog, Wind and IMP 8)', 'SIRs, CIRs, fast streams', NULL),
    ('Solar flare effects, 1970-2026 (ISGI)', NULL, 'Earth (magnetosphere)'),
    ('Near-Earth CME candidates, ACE and Wind 1995-2006 (LLAMAICE)', 'ICMEs, magnetic clouds, sheaths', NULL),
    ('Stream interaction regions at L1 matching PSP''s, 2018-2019 (Allen et al. 2021)', NULL, 'L1 (near-Earth solar wind)'),
    ('Type III radio bursts, STEREO-A/WAVES HFR, 2007-2011 (HELIO HFC)', NULL, 'Sun (remote sensing)'),
    ('Type III radio bursts, STEREO-B/WAVES HFR, 2007-2011 (HELIO HFC)', NULL, 'Sun (remote sensing)')
) AS rows(name, theme, observer);

CREATE OR REPLACE TABLE tolerances AS
SELECT theme::VARCHAR AS theme, hours::DOUBLE AS hours FROM (VALUES
    ('Interplanetary shocks', 1.0),
    ('Bow shocks', 0.25),
    ('Magnetopauses', 0.25),
    ('ICMEs, magnetic clouds, sheaths', 3.0),
    ('SIRs, CIRs, fast streams', 6.0),
    ('CMEs (coronagraph, imagers)', 1.0),
    ('Flares and solar events', 0.25),
    ('Solar energetic particles', 6.0),
    ('Radio bursts', 0.5),
    ('Magnetosheath jets', 0.01),
    ('Plasma waves', 0.25),
    ('Geomagnetic activity', 3.0),
    ('Quiet and disturbed days', 0.0),
    ('Forbush decreases', 6.0),
    ('Coronal waves', 0.5),
    ('Foreshock transients', 0.05)
) AS rows(theme, hours);


-- Shifts that give the chance level of an overlap: a few weeks, never a multiple of the
-- 27-day solar rotation, which would line up recurrent streams.
CREATE OR REPLACE TABLE chance_shifts AS
SELECT days::DOUBLE AS days FROM (VALUES (-61.9), (-19.1), (13.3), (40.7)) AS rows(days);

-- Themes that are not natural phenomena, besides every 'Operations: …' one.
CREATE OR REPLACE TABLE not_science AS
SELECT theme::VARCHAR AS theme FROM (VALUES
    ('Other'), ('Plasma regions (where the spacecraft is)'), ('CME arrivals (predicted)')
) AS rows(theme);

-- Folders whose many catalogues count as one when looking for copies.
CREATE OR REPLACE TABLE collapsed_units AS
SELECT folder::VARCHAR AS folder, unit::VARCHAR AS unit FROM (VALUES
    ('AMDA/MMS_BURST_MODE', 'AMDA: MMS_Burst_Mode (one catalogue a month)')
) AS rows(folder, unit);

-- The tolerance of a phenomenon without its own.
CREATE OR REPLACE MACRO default_tolerance_h() AS 1.0::DOUBLE;
-- A catalogue covering more than this share of its span confirms anything: it never confirms.
CREATE OR REPLACE MACRO dense_share() AS 0.5::DOUBLE;
-- An event of a catalogue with dates after 2100, after now + 1 year, was stored as an unsigned
-- 32-bit count of seconds from before 1970: it goes back 2^32 seconds.
-- In core functions only: the browser's engine has no time zone extension (ICU).
CREATE OR REPLACE MACRO wrap_limit() AS make_timestamp(epoch_us(now())) + INTERVAL 1 YEAR;

CREATE OR REPLACE MACRO hours(t) AS epoch_us(t) / 3.6e9;

-- A duration in seconds as pandas computes it, to the last bit: whole seconds (floored), plus
-- the microseconds over 1e6.
CREATE OR REPLACE MACRO seconds(span) AS
    (epoch_us(span) - (epoch_us(span) % 1000000 + 1000000) % 1000000) // 1000000
    + ((epoch_us(span) % 1000000 + 1000000) % 1000000) / 1e6;

-- A source's short name: 'AMDA' for 'AMDA/EARTH', 'DONKI' for 'DONKI (NASA CCMC)'.
CREATE OR REPLACE MACRO short_source(source) AS
    regexp_replace(regexp_split_to_array(source, ' \(|/')[1], '^\s+|\s+$', '', 'g');

-- Python's str.title(): a letter after a non-letter is upper case, every other letter lower.
CREATE OR REPLACE MACRO title_case(text) AS array_to_string(list_transform(
    range(1, length(text) + 1),
    lambda i: CASE WHEN i = 1 OR NOT regexp_matches(substr(text, i - 1, 1), '\pL')
                   THEN upper(substr(text, i, 1)) ELSE lower(substr(text, i, 1)) END), '');

CREATE OR REPLACE MACRO theme_rule(text) AS
    (SELECT theme FROM theme_rules WHERE regexp_matches(text, pattern) ORDER BY rank LIMIT 1);
CREATE OR REPLACE MACRO observer_rule(text) AS
    (SELECT observer FROM observer_rules WHERE regexp_matches(text, pattern) ORDER BY rank LIMIT 1);
CREATE OR REPLACE MACRO observer_name(text) AS
    (SELECT observer FROM observer_names WHERE regexp_matches(text, pattern) ORDER BY rank LIMIT 1);

-- What a catalogue is about, from its folder and name: the first matching rule wins.
CREATE OR REPLACE MACRO theme_of(folder, name) AS
    coalesce(theme_rule(lower(coalesce(folder, '') || ' / ' || name)), 'Other');

-- Where it was observed. The name says more than the folder: 'AMDA/EARTH' holds solar wind
-- lists too.
CREATE OR REPLACE MACRO observer_of(folder, name) AS CASE
    WHEN contains(coalesce(folder, ''), 'ARRCAT')
        THEN coalesce(observer_rule(replace(lower(name), 'earth l1', 'l1')), name)
    ELSE coalesce(observer_rule(lower(name)), observer_rule(lower(coalesce(folder, ''))), 'Unknown')
END;

CREATE OR REPLACE MACRO tolerance_h(phenomenon) AS
    coalesce((SELECT hours FROM tolerances WHERE theme = phenomenon), default_tolerance_h());

CREATE OR REPLACE MACRO is_science(phenomenon) AS
    NOT starts_with(phenomenon, 'Operations') AND phenomenon NOT IN (SELECT theme FROM not_science);


-- What the rules say about each catalogue. A table: computed once, read by every query.
CREATE OR REPLACE TABLE catalogue_rules AS
SELECT c.catalogue, c.name, coalesce(c.folder, '') AS folder, coalesce(c.source, '') AS source,
    coalesce(o.theme, theme_of(c.folder, c.name)) AS theme,
    coalesce(o.observer, observer_of(c.folder, c.name)) AS observer,
    short_source(coalesce(c.source, '')) AS short_source,
    short_source(coalesce(c.source, '')) || ': ' || left(c.name, 60) AS label
FROM catalogues c LEFT JOIN overrides o ON o.name = c.name;

-- The events, with wrapped dates moved back, each event's own observer, and the times in
-- hours since 1970 for interval arithmetic.
CREATE OR REPLACE VIEW clean_events AS
WITH wrapped AS (SELECT DISTINCT catalogue FROM events WHERE start > TIMESTAMP '2100-01-01'),
fixed AS (
    SELECT e.*, w.catalogue IS NOT NULL AND e.start > wrap_limit() AS repaired
    FROM events e LEFT JOIN wrapped w USING (catalogue)),
named AS (
    SELECT f.*, t.theme, t.observer AS catalogue_observer, t.label,
        lower(regexp_replace(coalesce(f.observer_raw, ''), '^\s+|\s+$', '', 'g')) AS raw
    FROM fixed f JOIN catalogue_rules t USING (catalogue)),
moved AS (
    SELECT * REPLACE (
        CASE WHEN repaired THEN start - to_seconds(4294967296) ELSE start END AS start,
        CASE WHEN repaired THEN stop - to_seconds(4294967296) ELSE stop END AS stop)
    FROM named)
SELECT event, catalogue, start, stop, theme,
    -- Crossings of a planet's boundaries happen at the planet, whichever spacecraft saw
    -- them; a solar list's location is a place on the Sun, not an observer.
    CASE WHEN theme IN ('Bow shocks', 'Magnetopauses') OR catalogue_observer = 'Sun (remote sensing)'
              OR raw = '' THEN catalogue_observer
         ELSE coalesce(observer_name(raw), title_case(raw)) END AS observer,
    catalogue_observer,
    label,
    seconds(stop - start) / 3600 AS duration_h,
    hours(start) AS start_h, hours(stop) AS stop_h,
    repaired, status, confidence, start_err, stop_err, tags
FROM moved;

-- Each catalogue with its phenomenon, observer, label, size, span, whether its dates were
-- repaired, and the share of its span its events cover ('covers').
CREATE OR REPLACE TABLE catalogue_tags AS
WITH spans AS (
    SELECT label, start_h, max(stop_h) OVER (PARTITION BY label ORDER BY start_h) AS reach
    FROM clean_events),
starts AS (
    SELECT label, start_h, reach,
        start_h > coalesce(lag(reach) OVER (PARTITION BY label ORDER BY start_h, reach), 'infinity'::DOUBLE) AS new
    FROM spans),
blocks AS (
    SELECT label, start_h, reach,
        sum(new::INT) OVER (PARTITION BY label ORDER BY start_h, reach ROWS UNBOUNDED PRECEDING) AS block
    FROM starts),
density AS (
    SELECT label, CASE WHEN max(span_h) > 0 THEN sum(block_h) / max(span_h) ELSE 0.0 END AS covers
    FROM (
        SELECT label, block, max(reach) - min(start_h) AS block_h,
            max(max(reach)) OVER (PARTITION BY label) - min(min(start_h)) OVER (PARTITION BY label) AS span_h
        FROM blocks GROUP BY label, block)
    GROUP BY label),
counts AS (
    SELECT catalogue, count(*) AS n, min(start) AS first, max(start) AS last, bool_or(repaired) AS wrapped
    FROM clean_events GROUP BY catalogue)
SELECT r.catalogue, r.name, r.folder, r.source, r.theme, r.observer, r.short_source, r.label,
    coalesce(n.n, 0) AS n, n.first, n.last, coalesce(n.wrapped, false) AS wrapped, d.covers,
    c.description, c.relay_published
FROM catalogue_rules r JOIN catalogues c USING (catalogue)
LEFT JOIN counts n USING (catalogue) LEFT JOIN density d USING (label);

-- How many catalogues and events each phenomenon and observer holds, and the years they span.
CREATE OR REPLACE VIEW coverage AS
SELECT theme, observer, count(DISTINCT catalogue) AS catalogues, count(*) AS events,
    year(min(start)) AS "from", year(max(start)) AS "to",
    -- As numpy rounds: scale, round half to even, scale back.
    round_even(median(duration_h) * 100, 0) / 100 AS median_duration_h
FROM clean_events GROUP BY theme, observer;

-- Phenomenon and observer pairs held by one catalogue: they cannot be cross-checked against
-- an independent list.
CREATE OR REPLACE VIEW single_list_pairs AS
SELECT theme, observer, count(*) AS events, any_value(label) AS only, min(start) AS first, max(start) AS last
FROM clean_events WHERE is_science(theme)
GROUP BY theme, observer HAVING count(DISTINCT catalogue) = 1;

-- Events per year, of one phenomenon or observer or all.
CREATE OR REPLACE MACRO events_per_year(phenomenon := NULL, seen_at := NULL) AS TABLE
SELECT theme, year(start) AS year, count(*) AS events FROM clean_events
WHERE (phenomenon IS NULL OR theme = phenomenon) AND (seen_at IS NULL OR observer = seen_at)
GROUP BY theme, year ORDER BY theme, year;

-- A catalogue's events joined with its attributes, typed as its own columns.
CREATE OR REPLACE MACRO attributes(id) AS TABLE
SELECT e.*, a.* EXCLUDE (_event)
FROM clean_events e JOIN attribute_rows(id) a ON a._event = e.event;

-- Catalogue pairs that share events to the minute: copies of one list. 'of_smaller' is the
-- share of the smaller one.
CREATE OR REPLACE VIEW exact_copies AS
WITH units AS (
    SELECT catalogue, coalesce(u.unit, label) AS unit, n FROM catalogue_tags LEFT JOIN collapsed_units u USING (folder)),
sizes AS (SELECT unit, sum(n) AS size FROM units GROUP BY unit),
keyed AS (SELECT DISTINCT catalogue, date_trunc('minute', start) AS s, date_trunc('minute', stop) AS e FROM clean_events),
shared AS (
    SELECT s, e, list_sort(list_distinct(list(unit))) AS owners
    FROM keyed JOIN units USING (catalogue) GROUP BY s, e HAVING count(DISTINCT catalogue) > 1),
owned AS (SELECT s, e, unnest(owners) AS unit, generate_subscripts(owners, 1) AS i FROM shared),
pairs AS (
    SELECT a.unit AS a, b.unit AS b, count(*) AS shared
    FROM owned a JOIN owned b ON a.s = b.s AND a.e = b.e AND a.i < b.i GROUP BY a.unit, b.unit)
SELECT a AS catalogue_a, b AS catalogue_b, shared, shared / sa.size AS of_a, shared / sb.size AS of_b,
    greatest(shared / sa.size, shared / sb.size) AS of_smaller
FROM pairs JOIN sizes sa ON sa.unit = a JOIN sizes sb ON sb.unit = b
WHERE greatest(shared / sa.size, shared / sb.size) >= 0.2;

-- Matching. An event of A matches B when one of B's events overlaps it, both widened by the
-- tolerance. A "unit" is one catalogue at one observer, so a mixed list splits by spacecraft.
-- For each ordered pair of units and each shift (0, then the chance shifts): how many of A's
-- events fall in B's span ('compared') and the share of them that match ('share').
-- `which_themes`: 'same' (the survey), 'different' (links between phenomena) or 'any'.
-- `a` and `b` name a catalogue by id or label; `after` and `before` cut both to a period.
CREATE OR REPLACE MACRO match_rates(phenomenon := NULL, seen_at := NULL, a := NULL, b := NULL,
        which_themes := 'same', after := NULL, before := NULL) AS TABLE
WITH cut AS (
    SELECT catalogue, observer, theme, start_h, stop_h FROM clean_events
    WHERE (after IS NULL OR start >= after) AND (before IS NULL OR start < before)),
units AS (
    SELECT row_number() OVER () AS unit, * FROM (
        SELECT catalogue, observer, any_value(theme) AS theme, min(start_h) AS first, max(stop_h) AS last
        FROM cut GROUP BY catalogue, observer)),
labelled AS (SELECT u.*, t.label FROM units u JOIN catalogue_tags t USING (catalogue)),
pairs AS (
    SELECT row_number() OVER () AS pair, x.unit AS a_unit, y.unit AS b_unit, tolerance_h(x.theme) AS tol,
        y.first, y.last
    FROM labelled x JOIN labelled y ON x.observer = y.observer AND x.catalogue <> y.catalogue
    WHERE (phenomenon IS NULL OR x.theme = phenomenon) AND (seen_at IS NULL OR x.observer = seen_at)
      AND (a IS NULL OR a IN (x.catalogue, x.label)) AND (b IS NULL OR b IN (y.catalogue, y.label))
      AND CASE which_themes WHEN 'same' THEN x.theme = y.theme
                            WHEN 'different' THEN x.theme <> y.theme
                            ELSE true END),
numbered AS (SELECT u.unit, c.start_h, c.stop_h FROM cut c JOIN units u USING (catalogue, observer)),
idx AS (
    -- RANGE frame: events starting together share the latest stop of them all.
    SELECT unit, start_h, max(stop_h) OVER (PARTITION BY unit ORDER BY start_h) AS latest_stop
    FROM numbered WHERE unit IN (SELECT b_unit FROM pairs)),
mine AS (
    SELECT p.pair, p.b_unit, p.tol, n.start_h, n.stop_h
    FROM pairs p JOIN numbered n ON n.unit = p.a_unit
    WHERE n.start_h BETWEEN p.first - p.tol AND p.last + p.tol),
shifted AS (
    SELECT pair, b_unit, tol, d.days, start_h + 24 * d.days AS shifted_start, stop_h + 24 * d.days + tol AS reach
    FROM mine, (SELECT 0.0::DOUBLE AS days UNION ALL SELECT days FROM chance_shifts) d),
hits AS (
    SELECT s.pair, s.days, coalesce(i.latest_stop >= s.shifted_start - s.tol, false) AS hit
    FROM shifted s ASOF LEFT JOIN idx i ON i.unit = s.b_unit AND s.reach >= i.start_h),
rates AS (SELECT pair, days, count(*) AS compared, avg(hit::INT) AS share FROM hits GROUP BY pair, days)
SELECT x.catalogue AS a_catalogue, y.catalogue AS b_catalogue, x.observer, x.theme, y.theme AS theme_b,
    x.label AS label_a, y.label AS label_b, r.days, r.compared, r.share
FROM rates r JOIN pairs p USING (pair)
JOIN labelled x ON x.unit = p.a_unit JOIN labelled y ON y.unit = p.b_unit;

-- The mean of the shifted shares, added in the shifts' order as numpy does, to the last bit.
CREATE OR REPLACE MACRO chance_share(shares) AS list_reduce(shares, lambda x, y: x + y) / len(shares);

-- For each ordered pair: how many of A's events fall in B's span ('compared'), the share of
-- them found in B ('matched'), the same share with A shifted by a few weeks ('chance'), and
-- their ratio ('lift'). A lift near 1 is chance; a high lift means the same events.
CREATE OR REPLACE MACRO agreement(a := NULL, b := NULL, phenomenon := NULL, seen_at := NULL,
        which_themes := 'any', after := NULL, before := NULL) AS TABLE
SELECT theme, observer, a_catalogue, b_catalogue, label_a, label_b, theme_b,
    any_value(compared) FILTER (days = 0) AS compared,
    any_value(share) FILTER (days = 0) AS matched,
    chance_share(list(share ORDER BY days) FILTER (days <> 0)) AS chance,
    any_value(share) FILTER (days = 0) / greatest(chance_share(list(share ORDER BY days) FILTER (days <> 0)), 1e-3) AS lift
FROM match_rates(phenomenon := phenomenon, seen_at := seen_at, a := a, b := b,
                 which_themes := which_themes, after := after, before := before)
GROUP BY ALL;

-- Every ordered pair of catalogues of one phenomenon at one observer.
CREATE OR REPLACE MACRO agreement_matrix(phenomenon, seen_at, after := NULL, before := NULL) AS TABLE
FROM agreement(phenomenon := phenomenon, seen_at := seen_at, which_themes := 'same', after := after, before := before);

-- Pairs that describe the same events (the /catalogues/ page's table): one row per pair, how
-- much of each is found in the other; most of one found in the other, at least 3 times
-- chance, both with 10 events or more.
CREATE OR REPLACE MACRO same_events(phenomenon := NULL, seen_at := NULL) AS TABLE
WITH science AS (
    FROM agreement(phenomenon := phenomenon, seen_at := seen_at, which_themes := 'same')
    WHERE NOT starts_with(theme, 'Operations'))
SELECT x.theme, x.observer, x.label_a, x.label_b, x.a_catalogue, x.b_catalogue,
    x.matched AS a_in_b, y.matched AS b_in_a, least(x.lift, y.lift) AS lift,
    x.compared AS events_a, y.compared AS events_b
FROM science x JOIN science y
    ON y.a_catalogue = x.b_catalogue AND y.b_catalogue = x.a_catalogue AND y.observer = x.observer
WHERE x.a_catalogue < x.b_catalogue
  AND greatest(x.matched, y.matched) >= 0.5 AND least(x.lift, y.lift) >= 3
  AND least(x.compared, y.compared) >= 10;

-- For each catalogue of a phenomenon at an observer: of its events that fall in the span of
-- another running catalogue, the share no other lists ('only_here'), the same share when
-- shifted in time ('only_here_if_shifted': chance), and the share confirmed by half the
-- running ones. Dense catalogues are checked but never confirm.
CREATE OR REPLACE MACRO confirmations(phenomenon, seen_at) AS TABLE
WITH dense AS (SELECT catalogue FROM catalogue_tags WHERE covers > dense_share()),
cut AS (SELECT catalogue, observer, event, start_h, stop_h FROM clean_events WHERE theme = phenomenon AND observer = seen_at),
spans AS (SELECT catalogue, min(start_h) AS first, max(stop_h) AS last FROM cut GROUP BY catalogue),
idx AS (
    SELECT catalogue, start_h, max(stop_h) OVER (PARTITION BY catalogue ORDER BY start_h) AS latest_stop FROM cut),
mine AS (
    SELECT c.catalogue, c.event, c.start_h, c.stop_h, s.catalogue AS other,
        c.start_h BETWEEN s.first AND s.last AS live
    FROM cut c JOIN spans s ON s.catalogue <> c.catalogue
    WHERE s.catalogue NOT IN (SELECT catalogue FROM dense)),
shifted AS (
    SELECT mine.*, d.days, start_h + 24 * d.days AS shifted_start, stop_h + 24 * d.days + tolerance_h(phenomenon) AS reach
    FROM mine, (SELECT 0.0::DOUBLE AS days UNION ALL SELECT days FROM chance_shifts) d),
hits AS (
    SELECT s.catalogue, s.event, s.other, s.days, s.live,
        coalesce(i.latest_stop >= s.shifted_start - tolerance_h(phenomenon), false) AS hit
    FROM shifted s ASOF LEFT JOIN idx i ON i.catalogue = s.other AND s.reach >= i.start_h),
per_event AS (
    SELECT catalogue, event,
        count(*) FILTER (days = 0 AND live)::DOUBLE AS running,
        count(*) FILTER (days = 0 AND live AND hit) AS found
    FROM hits GROUP BY catalogue, event),
chance AS (
    SELECT catalogue, event, days, count(*) FILTER (live AND hit) AS found
    FROM hits WHERE days <> 0 GROUP BY catalogue, event, days),
checked AS (SELECT * FROM per_event WHERE running > 0),
shifted_alone AS (
    SELECT catalogue, avg((c.found = 0)::INT) AS only_here_if_shifted
    FROM chance c SEMI JOIN checked USING (catalogue, event) GROUP BY catalogue)
SELECT k.catalogue, t.label, phenomenon AS theme, seen_at AS observer, count(*) AS events_checked,
    avg((k.found = 0)::INT) AS only_here, any_value(s.only_here_if_shifted) AS only_here_if_shifted,
    avg((k.found >= k.running / 2)::INT) AS confirmed_by_half,
    median(k.running) AS running_median
FROM checked k JOIN catalogue_tags t USING (catalogue) JOIN shifted_alone s USING (catalogue)
GROUP BY k.catalogue, t.label HAVING count(*) >= 10;
