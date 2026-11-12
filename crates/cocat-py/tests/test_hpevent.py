"""SPASE HPEvent lists in and out of a project: CDAWeb's and the STEREO Science Center's
event lists."""

from datetime import datetime, timezone

import cocat_rs

SHOCKS = (
    "#EventTableVersion: HPEvent 1.0\n"
    "#ListTitle: Shocks at STEREO A\n"
    "#FieldNames: start_time stop_time shock_normal_angle comment\n"
    "#FieldUnits: DateTime DateTime degree None\n"
    "#FieldNulls: 0001-01-01T00:00:00.000Z 0001-01-01T00:00:00.000Z -1 None\n"
    "#Description:\n"
    "#Compiled by Lan Jian.\n"
    '2007-01-14T19:35:08.000Z\t2007-01-14T19:35:08.000Z\t67.6\t"F"\n'
    '2007-02-12T08:10:06.000Z\t2007-02-12T08:10:06.000Z\t-1.00\t"R"\n'
)


def test_an_hpevent_list_becomes_a_catalogue_named_after_its_title(project, tmp_path):
    path = tmp_path / "shocks.txt"
    path.write_text(SHOCKS)

    catalogue = project.import_hpevent(str(path))

    assert catalogue.name == "Shocks at STEREO A"
    assert catalogue.attributes["description"] == "Compiled by Lan Jian."
    first, second = sorted(catalogue.events, key=lambda event: event.start)
    assert first.start == first.stop == datetime(2007, 1, 14, 19, 35, 8, tzinfo=timezone.utc)
    assert first.attributes == {"shock_normal_angle": 67.6, "comment": "F"}
    assert second.attributes == {"comment": "R"}, "-1.00 is the null"


def test_an_exported_list_reads_back_with_its_units(project, tmp_path):
    path = tmp_path / "shocks.txt"
    path.write_text(SHOCKS)
    catalogue = project.import_hpevent(str(path), name="Shocks")
    exported = tmp_path / "out.txt"

    cocat_rs.export_hpevent([catalogue], exported)

    text = exported.read_text()
    assert text.startswith("#EventTableVersion: HPEvent")
    assert "#ListTitle: Shocks" in text
    assert "#FieldUnits: DateTime DateTime None degree" in text
    again = project.import_hpevent(str(exported), name="Shocks again")
    assert len(again.events) == 2
