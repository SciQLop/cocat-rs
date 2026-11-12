//! Event lists published with papers, each from the open repository its authors deposited it
//! in (Zenodo, MASER, figshare, a mission team's site), or, for a list that only exists as a
//! table in a paper's supplement, from a copy converted once and kept here. Every list says
//! whom to cite and under which licence.
//!
//! A list is data: where its files are, and the function that reads one of them into events.
//! A crossing (a single time) is an event whose start and stop are equal.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use cocat_core::model::EventRecord;
use cocat_core::time::Timestamp;
use serde_json::{Map, Value};
use uuid::Uuid;

use super::catcsv;
use super::{Fetched, Provider};

pub struct Literature;

const NAMESPACE: Uuid = Uuid::from_u128(0x9d4e2a71_3c58_4b0f_a6e1_52f7c8d3b194);
const BUSY_PAUSE: Duration = Duration::from_secs(30);
const SHARP_FILE: &str = "https://sharp.fmi.fi/wp-content/All_shocks.csv";
const CARRINGTON_FILE: &str = "https://cdaweb.gsfc.nasa.gov/pub/event_lists/carrington_event_1859_auroral_sightings/Great_Magnetic_Storm_of_1859_List_of_Auroral_Sightings.csv";

/// One event read from a file: its key within the list, its ends and its attributes.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    id: String,
    start: Timestamp,
    stop: Timestamp,
    attributes: Map<String, Value>,
}

/// Reads one file of a list; `part` picks the list's rows in a file that holds several.
type Read = fn(text: &str, part: &str) -> Result<Vec<Item>, String>;

/// Where a list's events come from.
enum Files {
    /// Downloaded on each run; a list spread over several files reads them all.
    Remote(&'static [&'static str]),
    /// A table converted once from a paper's supplement, kept in this crate.
    Kept(&'static str),
    /// The pages an index links to whose link fits `pattern` (see [`linked`]): a year per
    /// page, found again on each run, so a new year is read without a change here.
    Linked {
        index: &'static str,
        pattern: &'static str,
    },
}

struct List {
    key: &'static str,
    folder: &'static str,
    name: &'static str,
    about: &'static str,
    /// Whom to cite, and the licence.
    credit: &'static str,
    /// Where people read the list at its source.
    url: &'static str,
    files: Files,
    read: Read,
    /// What `read` picks in a file shared by several lists; empty when the file is the list's.
    part: &'static str,
}

const JUNO: &str = "Louis, Hospodarsky, Jackman et al. (2022), Lists of magnetopause and bow shock crossings as measured by Juno/Waves and Juno/MAG, Zenodo, https://doi.org/10.5281/zenodo.7304517, licence CC-BY-4.0; see Louis et al. (2023), JGR, https://doi.org/10.1029/2022JA031155.";
const CASSINI: &str = "Jackman (2022), List of Saturn magnetopause and bow shock crossings by the Cassini spacecraft, Zenodo, https://doi.org/10.5281/zenodo.5913537, licence CC-BY-4.0; see Jackman et al. (2019), JGR, https://doi.org/10.1029/2019JA026628.";
const MESSENGER: &str = "Sun (2026), Lists of Magnetopause and Bow Shock Crossings of Mercury by MESSENGER Spacecraft, Zenodo, https://doi.org/10.5281/zenodo.18236915, licence CC-BY-4.0; see Sun et al. (2020), GRL, https://doi.org/10.1029/2020GL089784.";

const LISTS: [List; 47] = [
    List {
        key: "juno-magnetopause",
        folder: "Literature/Jupiter",
        name: "Jupiter magnetopause crossings, Juno (Louis et al. 2023)",
        about: "Crossings of Jupiter's magnetopause by Juno from 2016, seen by Waves and MAG; `direction` in is from the magnetosphere to the magnetosheath. Positions are in Jovian radii.",
        credit: JUNO,
        url: "https://doi.org/10.5281/zenodo.7304517",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/7304517/files/boundary_crossings_caracteristics_MP.csv/content",
        ]),
        read: juno_crossings,
        part: "",
    },
    List {
        key: "juno-bow-shock",
        folder: "Literature/Jupiter",
        name: "Jupiter bow shock crossings, Juno (Louis et al. 2023)",
        about: "Crossings of Jupiter's bow shock by Juno in 2016, seen by Waves and MAG. Positions are in Jovian radii.",
        credit: JUNO,
        url: "https://doi.org/10.5281/zenodo.7304517",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/7304517/files/boundary_crossings_caracteristics_BS.csv/content",
        ]),
        read: juno_crossings,
        part: "",
    },
    List {
        key: "cassini-magnetopause",
        folder: "Literature/Saturn",
        name: "Saturn magnetopause crossings, Cassini (Jackman et al. 2019)",
        about: "Crossings of Saturn's magnetopause by Cassini, 2004-2016; `direction` is in or out. Positions, where given, are in Saturn radii.",
        credit: CASSINI,
        url: "https://doi.org/10.5281/zenodo.5913537",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/5913537/files/Master_BS_MP_Crossing_List_04_16_incCMJcorr_revised_120821.txt/content",
        ]),
        read: cassini_crossings,
        part: "MP",
    },
    List {
        key: "cassini-bow-shock",
        folder: "Literature/Saturn",
        name: "Saturn bow shock crossings, Cassini (Jackman et al. 2019)",
        about: "Crossings of Saturn's bow shock by Cassini, 2004-2016; `direction` is in or out. Positions, where given, are in Saturn radii.",
        credit: CASSINI,
        url: "https://doi.org/10.5281/zenodo.5913537",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/5913537/files/Master_BS_MP_Crossing_List_04_16_incCMJcorr_revised_120821.txt/content",
        ]),
        read: cassini_crossings,
        part: "BS",
    },
    List {
        key: "messenger-bow-shock-in",
        folder: "Literature/Mercury",
        name: "Mercury bow shock, inbound, MESSENGER (Sun)",
        about: "Inbound crossings of Mercury's bow shock by MESSENGER, 2011-2015: from the first to the last crossing; `crossing` is single or multiple.",
        credit: MESSENGER,
        url: "https://doi.org/10.5281/zenodo.18236915",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/18236915/files/Bow_Shock_In_Time_Duration_public_version_WeijieSun_20260113.txt/content",
        ]),
        read: messenger_crossings,
        part: "",
    },
    List {
        key: "messenger-bow-shock-out",
        folder: "Literature/Mercury",
        name: "Mercury bow shock, outbound, MESSENGER (Sun)",
        about: "Outbound crossings of Mercury's bow shock by MESSENGER, 2011-2015: from the first to the last crossing; `crossing` is single or multiple.",
        credit: MESSENGER,
        url: "https://doi.org/10.5281/zenodo.18236915",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/18236915/files/Bow_Shock_Out_Time_Duration_public_version_WeijieSun_20260113.txt/content",
        ]),
        read: messenger_crossings,
        part: "",
    },
    List {
        key: "messenger-magnetopause-in",
        folder: "Literature/Mercury",
        name: "Mercury magnetopause, inbound, MESSENGER (Sun)",
        about: "Inbound crossings of Mercury's magnetopause by MESSENGER, 2011-2015: from the first to the last crossing; `crossing` is single or multiple.",
        credit: MESSENGER,
        url: "https://doi.org/10.5281/zenodo.18236915",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/18236915/files/MagPause_In_Time_Duration_public_version_WeijieSun_20260113.txt/content",
        ]),
        read: messenger_crossings,
        part: "",
    },
    List {
        key: "messenger-magnetopause-out",
        folder: "Literature/Mercury",
        name: "Mercury magnetopause, outbound, MESSENGER (Sun)",
        about: "Outbound crossings of Mercury's magnetopause by MESSENGER, 2011-2015: from the first to the last crossing; `crossing` is single or multiple.",
        credit: MESSENGER,
        url: "https://doi.org/10.5281/zenodo.18236915",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/18236915/files/MagPause_Out_Time_Duration_public_version_WeijieSun_20260113.txt/content",
        ]),
        read: messenger_crossings,
        part: "",
    },
    List {
        key: "wind-akr-bursts",
        folder: "Literature/Earth",
        name: "AKR bursts, Wind/WAVES 1995-2004 (Fogg et al.)",
        about: "Bursts of auroral kilometric radiation picked one by one in Wind/WAVES data; `f_min_khz` and `f_max_khz` bound each burst, the position is Wind's mean in GSE (Earth radii, degrees, hours of local time).",
        credit: "Fogg, Jackman, Waters, Bonnin, Lamy, Cecconi, Issautier, Louis (2023), Bursts of Auroral Kilometric Radiation individually selected from Wind/WAVES data (1995-2004), PADC, https://doi.org/10.25935/ayzp-1833, licence CC-BY-4.0; see Fogg et al. (2022), JGR, https://doi.org/10.1029/2021JA030209.",
        url: "https://doi.org/10.25935/ayzp-1833",
        files: Files::Remote(&[
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_1995.json",
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_1996.json",
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_1997.json",
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_1998.json",
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_1999.json",
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_2000.json",
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_2001.json",
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_2002.json",
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_2003.json",
            "https://maser.obspm.fr/doi/10.25935/ayzp-1833/content/fogg_akr_burst_list_2004.json",
        ]),
        read: akr_bursts,
        part: "",
    },
    List {
        key: "mms-magnetopause-ballerini",
        folder: "Literature/Earth",
        name: "MMS magnetopause crossings (Ballerini et al. 2024)",
        about: "Magnetopause crossings by MMS, 2015-2017, with their hodogram class, mean normals from B and from the ions (GSE), dimensionality and non-gyrotropy indices, and widths. Converted once from the paper's supplementary table.",
        credit: "Ballerini et al. (2024), J. Plasma Phys. 90, 905900612, https://doi.org/10.1017/S0022377824001089, licence CC-BY-4.0 (supplementary material).",
        url: "https://doi.org/10.1017/S0022377824001089",
        files: Files::Kept(include_str!("data/ballerini_2024_magnetopause.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms-sitl-selections",
        folder: "Literature/Earth",
        name: "MMS burst segment selections (SITL)",
        about: "The burst data segments the MMS scientists in the loop selected, from 2015: `fom` is the figure of merit, `status` where the segment is, `sitl` who selected it and `discussion` why.",
        credit: "MMS SITL selections from the Berkeley EVA tables (https://research.ssl.berkeley.edu/~moka/eva/); acknowledge the MMS SITL team and the MMS Science Data Center.",
        url: "https://research.ssl.berkeley.edu/~moka/eva/bss_table_full.html",
        files: Files::Remote(&["https://research.ssl.berkeley.edu/~moka/eva/bss/bss_full.csv"]),
        read: sitl_segments,
        part: "",
    },
    List {
        key: "helcats-linkcat",
        folder: "Literature/Heliosphere",
        name: "LINKCAT, CMEs linked to their ICMEs (HELCATS)",
        about: "CMEs tracked by the STEREO heliospheric imagers linked to the ICMEs observed in situ, 2007-2013: each event is the magnetic obstacle, with the CME's launch, its predicted arrival and the flare at its source.",
        credit: "Moestl, Boakes, Isavnin, Kilpua, Mrotzek, Bothmer (2017), HELCATS LINKCAT, figshare, https://doi.org/10.6084/m9.figshare.4588330.v2, licence CC-BY-4.0.",
        url: "https://doi.org/10.6084/m9.figshare.4588330.v2",
        files: Files::Remote(&["https://ndownloader.figshare.com/files/7428409"]),
        read: linkcat,
        part: "",
    },
    List {
        key: "sharp-mms",
        folder: "Literature/Earth",
        name: "Earth bow shock crossings, MMS (SHARP)",
        about: "Crossings of Earth's bow shock by MMS, 2015-2020, with the burst interval that covers each. `direction` is in or out; upstream field, density, temperature and velocity, the shock's normal, θBn and Mach numbers, and the jumps across it, as the SHARP database gives them (fill values left out). GSE.",
        credit: "The shock parameters were obtained from https://sharp.fmi.fi/shock-database/ and developed in the SHARP project, funded by the European Union under grant agreement number 101004131. For MMS, also cite Lalti et al. (2022), JGR, https://doi.org/10.1029/2022JA030454.",
        url: "https://sharp.fmi.fi/shock-database/",
        files: Files::Remote(&[SHARP_FILE]),
        read: sharp_shocks,
        part: "MMS",
    },
    List {
        key: "sharp-cluster",
        folder: "Literature/Earth",
        name: "Earth bow shock crossings, Cluster (SHARP)",
        about: "Crossings of Earth's bow shock by Cluster, 2001-2012. `direction` is in or out; upstream field, density, temperature and velocity, the shock's normal, θBn and Mach numbers, and the jumps across it, as the SHARP database gives them (fill values left out). GSE.",
        credit: "The shock parameters were obtained from https://sharp.fmi.fi/shock-database/ and developed in the SHARP project, funded by the European Union under grant agreement number 101004131.",
        url: "https://sharp.fmi.fi/shock-database/",
        files: Files::Remote(&[SHARP_FILE]),
        read: sharp_shocks,
        part: "CLUSTER",
    },
    List {
        key: "sharp-maven",
        folder: "Literature/Mars",
        name: "Mars bow shock crossings, MAVEN (SHARP)",
        about: "Crossings of the Martian bow shock by MAVEN, 2014-2017. `direction` is in or out; upstream field, density, temperature and velocity, the shock's normal, θBn and Mach numbers, and the jumps across it, as the SHARP database gives them (fill values left out). MSO.",
        credit: "The shock parameters were obtained from https://sharp.fmi.fi/shock-database/ and developed in the SHARP project, funded by the European Union under grant agreement number 101004131.",
        url: "https://sharp.fmi.fi/shock-database/",
        files: Files::Remote(&[SHARP_FILE]),
        read: sharp_shocks,
        part: "MAVEN",
    },
    List {
        key: "sharp-vex",
        folder: "Literature/Venus",
        name: "Venus bow shock crossings, Venus Express (SHARP)",
        about: "Crossings of the Venusian bow shock by Venus Express, 2008-2012. `direction` is in or out; upstream field, density, temperature and velocity, the shock's normal, θBn and Mach numbers, and the jumps across it, as the SHARP database gives them (fill values left out). VSO.",
        credit: "The shock parameters were obtained from https://sharp.fmi.fi/shock-database/ and developed in the SHARP project, funded by the European Union under grant agreement number 101004131.",
        url: "https://sharp.fmi.fi/shock-database/",
        files: Files::Remote(&[SHARP_FILE]),
        read: sharp_shocks,
        part: "VEX",
    },
    List {
        key: "sharp-themis-b",
        folder: "Literature/Heliosphere",
        name: "Interplanetary shocks, THEMIS B (SHARP)",
        about: "Interplanetary shocks seen by THEMIS B (ARTEMIS P1), 2008-2019. `direction` is in or out; upstream field, density, temperature and velocity, the shock's normal, θBn and Mach numbers, and the jumps across it, as the SHARP database gives them (fill values left out).",
        credit: "The shock parameters were obtained from https://sharp.fmi.fi/shock-database/ and developed in the SHARP project, funded by the European Union under grant agreement number 101004131.",
        url: "https://sharp.fmi.fi/shock-database/",
        files: Files::Remote(&[SHARP_FILE]),
        read: sharp_shocks,
        part: "THEMIS B",
    },
    List {
        key: "sharp-themis-c",
        folder: "Literature/Heliosphere",
        name: "Interplanetary shocks, THEMIS C (SHARP)",
        about: "Interplanetary shocks seen by THEMIS C (ARTEMIS P2), 2008-2019. `direction` is in or out; upstream field, density, temperature and velocity, the shock's normal, θBn and Mach numbers, and the jumps across it, as the SHARP database gives them (fill values left out).",
        credit: "The shock parameters were obtained from https://sharp.fmi.fi/shock-database/ and developed in the SHARP project, funded by the European Union under grant agreement number 101004131.",
        url: "https://sharp.fmi.fi/shock-database/",
        files: Files::Remote(&[SHARP_FILE]),
        read: sharp_shocks,
        part: "THEMIS C",
    },
    List {
        key: "ipshocks",
        folder: "Literature/Heliosphere",
        name: "Interplanetary shocks (IPShocks, Helsinki)",
        about: "Fast forward and fast reverse interplanetary shocks seen by some twenty spacecraft from 1975 (Helios, Voyager, Ulysses, Wind, ACE, STEREO, Solar Orbiter…), with the upstream and downstream plasma and field, the shock's normal, speed, θBn and Mach numbers; `spacecraft` names the observer.",
        credit: "This work uses data from the IPShocks database, generated and maintained at the University of Helsinki (Kilpua et al. 2026), Zenodo, https://doi.org/10.5281/zenodo.19730292, licence CC-BY-4.0.",
        url: "https://ipshocks.helsinki.fi/",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/19730292/files/ipshocks_database.csv/content",
        ]),
        read: ipshocks,
        part: "",
    },
    List {
        key: "istp-clouds",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "Interplanetary magnetic clouds (ISTP catalog, Wind and IMP 8)",
        about: "Interplanetary magnetic clouds, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20Interplanetary%20magnetic%20clouds",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "istp-shocks",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "Interplanetary shocks (ISTP catalog, Wind and IMP 8)",
        about: "Interplanetary shocks, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20interplanetary%20shocks",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "istp-high-speed-streams",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "High speed streams (ISTP catalog, Wind and IMP 8)",
        about: "High speed streams, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20SW%20high%20speed%20streams",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "istp-low-speed-streams",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "Low speed streams (ISTP catalog, Wind and IMP 8)",
        about: "Low speed streams, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20low%20speed%20stream%20intervals%20in%20the%20SW",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "istp-interaction-regions",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "Interaction regions (ISTP catalog, Wind and IMP 8)",
        about: "Interaction regions, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20interactive%20regions%20in%20the%20SW",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "istp-sector-boundaries",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "Sector boundary crossings (ISTP catalog, Wind and IMP 8)",
        about: "Sector boundary crossings, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20magnetic%20field%20sector%20boundary%20crossings",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "istp-bz-north",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "IMF Bz northward (ISTP catalog, Wind and IMP 8)",
        about: "IMF Bz northward, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20intervals%20of%20IMF%20Bz%20northward",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "istp-bz-south",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "IMF Bz southward (ISTP catalog, Wind and IMP 8)",
        about: "IMF Bz southward, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20intervals%20of%20IMF%20Bz%20southward",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "istp-ey-changes",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "Changes of the Ey direction (ISTP catalog, Wind and IMP 8)",
        about: "Changes of the Ey direction, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20changes%20in%20the%20direction%20of%20the%20Ey%20component%20of%20the%20electric%20field",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "istp-miscellaneous",
        folder: "Literature/Heliosphere/ISTP Solar Wind Catalog",
        name: "Miscellaneous solar wind features (ISTP catalog, Wind and IMP 8)",
        about: "Miscellaneous solar wind features, 1993-2000, from the ISTP Solar Wind Catalog by Berdichevsky and Lepping; `s_c` is W for Wind, 8 for IMP 8.",
        credit: "Lepping and Berdichevsky, ISTP Solar Wind Catalog (spase://NASA/Catalog/ISTP/SWCAT), from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/Berdichevsky%20-%20miscelaneous%20features%20in%20the%20SW",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "wind-current-sheet-exhausts",
        folder: "Literature/Heliosphere",
        name: "Reconnection exhausts at solar wind current sheets, Wind (Eriksson et al.)",
        about: "Current sheets with a reconnection exhaust seen by Wind at 1 AU, 2004-2014, with each sheet's LMN frame (unit vectors in GSE).",
        credit: "Eriksson et al. (2022), Characteristics of Multi-Scale Current Sheets in the Solar Wind at 1 AU Associated with Magnetic Reconnection and the Case for a Heliospheric Current Sheet Avalanche, ApJ; list from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/eriksson_wind_current_sheet_exhaust_lmn_v01.txt",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "artemis-magnetopause",
        folder: "Literature/Earth",
        name: "Magnetopause crossings, ARTEMIS P1 (O'Brien)",
        about: "Crossings of Earth's magnetopause by ARTEMIS P1 (THEMIS B), 2011-2017, with its position in GSE.",
        credit: "Connor O'Brien, ARTEMIS Magnetopause Crossing Database, from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/other-event-lists/aaa_boundary_crossings/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/other-event-lists/aaa_boundary_crossings/magnetopause_crossings/artemis_connor-obrien/artemis_hpevent_sorted_20110811-20171205_v01.txt",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "magnetopause-wang",
        folder: "Literature/Earth",
        name: "Magnetopause crossings, 23 missions 1966-2008 (Wang et al. 2013)",
        about: "Crossings of Earth's magnetopause by 23 missions from IMP 3 to THEMIS, 1966-2008; `sc_name` is the spacecraft, the position is in GSM.",
        credit: "Wang et al. (2013), JGR Space Physics 118(5), https://doi.org/10.1002/jgra.50226; dataset by Yongli Wang and Connor O'Brien, from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/other-event-lists/aaa_boundary_crossings/",
        files: Files::Remote(&[
            "https://cdaweb.gsfc.nasa.gov/pub/event_lists/other-event-lists/aaa_boundary_crossings/magnetopause_crossings/yongli-wang/wang_hpevent_sorted_19661000-20081000_v01.txt",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "carrington-1859-aurora",
        folder: "Literature/Earth",
        name: "Aurora sightings of the Carrington event, 1859 (Green & Boardsen)",
        about: "Where and when the aurorae of the great storms of late August and early September 1859 were seen, from the historical records; `source` cites the record.",
        credit: "Green & Boardsen (2006), Adv. Space Res., https://doi.org/10.1016/j.asr.2005.08.054; list from the CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/carrington_event_1859_auroral_sightings/",
        files: Files::Remote(&[CARRINGTON_FILE]),
        read: aurora_sightings,
        part: "",
    },
    List {
        key: "helcats-higeocat",
        folder: "Literature/Heliosphere",
        name: "HIGeoCAT, CMEs tracked by the STEREO heliospheric imagers (HELCATS, 2007-2017)",
        about: "CMEs seen by the STEREO-A and -B heliospheric imagers, 2007-2017, at their first sighting in HI1, with the direction, speed and back-projected launch time from three fits (FP, SSE, HM); `quality` good, fair or poor (do not use poor). Version 5, frozen in 2018: the live version has no stable home since the HELCATS site closed.",
        credit: "Barnes et al. (2019), Solar Physics 294, https://doi.org/10.1007/s11207-019-1444-4; HELCATS HIGeoCAT, figshare, https://doi.org/10.6084/m9.figshare.5803176, licence CC-BY-4.0.",
        url: "https://doi.org/10.6084/m9.figshare.5803176",
        files: Files::Remote(&["https://ndownloader.figshare.com/files/10257246"]),
        read: higeocat,
        part: "",
    },
    List {
        key: "cfa-wind",
        folder: "Literature/Heliosphere",
        name: "Interplanetary shocks, Wind (CfA)",
        about: "Interplanetary shocks seen by Wind, 1995-2024: `type` is FF, FR, SF or SR (fast or slow, forward or reverse), the position is Wind's in GSE (Earth radii), and `page` is the shock's analysis at the CfA (asymptotic plasma, normals from eight methods, θBn, speed, Mach numbers).",
        credit: "The Harvard-Smithsonian Center for Astrophysics Interplanetary Shock Database (Wind, ACE, DSCOVR), maintained by M. Stevens, https://lweb.cfa.harvard.edu/shocks/; it states no licence, cite the database.",
        url: "https://lweb.cfa.harvard.edu/shocks/",
        files: Files::Linked {
            index: "https://lweb.cfa.harvard.edu/shocks/wi_data/",
            pattern: "wi_####.html",
        },
        read: cfa_shocks,
        part: "https://lweb.cfa.harvard.edu/shocks/wi_data/",
    },
    List {
        key: "cfa-ace",
        folder: "Literature/Heliosphere",
        name: "Interplanetary shocks, ACE (CfA)",
        about: "Interplanetary shocks seen by ACE, 1998-2014: `type` is FF, FR, SF or SR (fast or slow, forward or reverse), the position is ACE's in GSE (Earth radii), and `page` is the shock's analysis at the CfA (asymptotic plasma, normals from eight methods, θBn, speed, Mach numbers).",
        credit: "The Harvard-Smithsonian Center for Astrophysics Interplanetary Shock Database (Wind, ACE, DSCOVR), maintained by M. Stevens, https://lweb.cfa.harvard.edu/shocks/; it states no licence, cite the database.",
        url: "https://lweb.cfa.harvard.edu/shocks/",
        files: Files::Linked {
            index: "https://lweb.cfa.harvard.edu/shocks/ac_master_data/",
            pattern: "ac_master_####.html",
        },
        read: cfa_shocks,
        part: "https://lweb.cfa.harvard.edu/shocks/ac_master_data/",
    },
    List {
        key: "cfa-dscovr",
        folder: "Literature/Heliosphere",
        name: "Interplanetary shocks, DSCOVR (CfA)",
        about: "Interplanetary shocks seen by DSCOVR, 2016-2017: `type` is FF, FR, SF or SR (fast or slow, forward or reverse), the position is DSCOVR's in GSE (Earth radii), and `page` is the shock's analysis at the CfA (asymptotic plasma, normals from eight methods, θBn, speed, Mach numbers).",
        credit: "The Harvard-Smithsonian Center for Astrophysics Interplanetary Shock Database (Wind, ACE, DSCOVR), maintained by M. Stevens, https://lweb.cfa.harvard.edu/shocks/; it states no licence, cite the database.",
        url: "https://lweb.cfa.harvard.edu/shocks/",
        files: Files::Linked {
            index: "https://lweb.cfa.harvard.edu/shocks/dsc_data/",
            pattern: "dsc_data_####.html",
        },
        read: cfa_shocks,
        part: "https://lweb.cfa.harvard.edu/shocks/dsc_data/",
    },
    List {
        key: "fluxropes-wind",
        folder: "Literature/Heliosphere/Small-scale flux ropes",
        name: "Small-scale flux ropes, Wind (Hu et al.)",
        about: "Small-scale magnetic flux ropes in the solar wind found in Wind data by the Grad-Shafranov based detection of Hu et al., 1996-2016: duration, fit residue, mean and peak field, plasma beta, solar wind speed, proton temperature (MK) and the axis orientation (θ, φ in degrees).",
        credit: "Hu, Zheng, Chen et al. (2018), Automated detection of small-scale magnetic flux ropes in the solar wind: first results from the Wind spacecraft measurements, ApJS 239, 12, https://doi.org/10.3847/1538-4365/aae57d; database at http://fluxrope.info/ (it states no licence).",
        url: "http://fluxrope.info/",
        files: Files::Linked {
            index: "http://fluxrope.info/",
            pattern: "####/year####.html",
        },
        read: flux_ropes,
        part: "",
    },
    List {
        key: "fluxropes-ace",
        folder: "Literature/Heliosphere/Small-scale flux ropes",
        name: "Small-scale flux ropes, ACE (Hu et al.)",
        about: "Small-scale magnetic flux ropes in the solar wind found in ACE data by the Grad-Shafranov based detection of Hu et al., 1998-2018: duration, fit residue, mean and peak field, plasma beta, solar wind speed, proton temperature (MK) and the axis orientation (θ, φ in degrees).",
        credit: "Hu, Zheng, Chen et al. (2018), Automated detection of small-scale magnetic flux ropes in the solar wind: first results from the Wind spacecraft measurements, ApJS 239, 12, https://doi.org/10.3847/1538-4365/aae57d; database at http://fluxrope.info/ (it states no licence).",
        url: "http://fluxrope.info/",
        files: Files::Linked {
            index: "http://fluxrope.info/",
            pattern: "ACE####/year####.html",
        },
        read: flux_ropes,
        part: "",
    },
    List {
        key: "fluxropes-ulysses",
        folder: "Literature/Heliosphere/Small-scale flux ropes",
        name: "Small-scale flux ropes, Ulysses (Hu et al.)",
        about: "Small-scale magnetic flux ropes in the solar wind found in Ulysses data by the Grad-Shafranov based detection of Hu et al., 1991-2009: duration, fit residue, mean and peak field, plasma beta, solar wind speed, proton temperature (MK) and the axis orientation (θ, φ in degrees).",
        credit: "Hu, Zheng, Chen et al. (2018), Automated detection of small-scale magnetic flux ropes in the solar wind: first results from the Wind spacecraft measurements, ApJS 239, 12, https://doi.org/10.3847/1538-4365/aae57d; database at http://fluxrope.info/ (it states no licence).",
        url: "http://fluxrope.info/",
        files: Files::Linked {
            index: "http://fluxrope.info/",
            pattern: "Ulysses####/year####.html",
        },
        read: flux_ropes,
        part: "",
    },
    List {
        key: "fluxropes-psp",
        folder: "Literature/Heliosphere/Small-scale flux ropes",
        name: "Small-scale flux ropes, Parker Solar Probe (Hu et al.)",
        about: "Small-scale magnetic flux ropes in the solar wind found in Parker Solar Probe data by the Grad-Shafranov based detection of Hu et al., from 2018: duration, fit residue, mean and peak field, plasma beta, solar wind speed, proton temperature (MK) and the axis orientation (θ, φ in degrees). Durations are in seconds, `r_au` is the distance to the Sun.",
        credit: "Hu, Zheng, Chen et al. (2018), Automated detection of small-scale magnetic flux ropes in the solar wind: first results from the Wind spacecraft measurements, ApJS 239, 12, https://doi.org/10.3847/1538-4365/aae57d; database at http://fluxrope.info/ (it states no licence).",
        url: "http://fluxrope.info/",
        files: Files::Linked {
            index: "http://fluxrope.info/",
            pattern: "PSP####/year####.html",
        },
        read: flux_ropes,
        part: "",
    },
    List {
        key: "cdaweb-bow-shocks",
        folder: "Literature/Earth",
        name: "Earth bow shock crossings, IMP 8, Geotail, Magion 4, Cluster (CDAWeb)",
        about: "Crossings of Earth's bow shock by IMP 8 (1973-2000), Geotail (1995-1997), Magion 4 (1996-1997) and Cluster (2001-2002), with the spacecraft's position and the upstream solar wind (field, velocity given as its opposite, density, temperature, speeds, Mach numbers, β, pressure, IMF cone and clock angles); IMP 8 adds the downstream density, `shock_strength`, the crossing's `transition` and quality flags.",
        credit: "IMP 8 magnetometer and plasma teams (GSFC: A. Szabo, J. Merka; MIT: J. Richardson, K. Paularena), R. Kessel (NSSDC) for Geotail and Cluster, J. Safrankova and Z. Nemecek for Magion 4; SPDF/GSFC CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/other-event-lists/aaa_boundary_crossings/bowshock_crossings/.",
        url: "https://omniweb.gsfc.nasa.gov/ftpbrowser/bowshock.html",
        files: Files::Linked {
            index: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/other-event-lists/aaa_boundary_crossings/bowshock_crossings/",
            pattern: "*_####.txt",
        },
        read: bow_shocks,
        part: "",
    },
    List {
        key: "cdaweb-magnetopause-omni",
        folder: "Literature/Earth",
        name: "Earth magnetopause crossings 1963-1994, with OMNI solar wind (CDAWeb)",
        about: "Crossings of Earth's magnetopause by seventeen spacecraft from IMP 1 to Geotail, 1963-1994, each with its position and the solar wind of its hour from OMNI (field, velocity, density, temperature, pressure, Ey, β, Mach numbers, IMF angles) and the Kp, sunspot, Dst and AE indices.",
        credit: "R. Kessel (NASA LWS TR&T) and SPDF/GSFC, CDAWeb event lists, https://cdaweb.gsfc.nasa.gov/pub/event_lists/other-event-lists/aaa_boundary_crossings/magnetopause_crossings/; solar wind from OMNI.",
        url: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/other-event-lists/aaa_boundary_crossings/magnetopause_crossings/",
        files: Files::Linked {
            index: "https://cdaweb.gsfc.nasa.gov/pub/event_lists/other-event-lists/aaa_boundary_crossings/magnetopause_crossings/",
            pattern: "*_####.txt",
        },
        read: omni_magnetopause,
        part: "",
    },
    List {
        key: "stereo-icmes",
        folder: "Literature/Heliosphere/STEREO (Lan Jian)",
        name: "ICMEs at STEREO A and B (Jian)",
        about: "Interplanetary coronal mass ejections seen in situ by STEREO A and B from 2006, compiled by Lan Jian: `stereo` is A or B, the magnetic obstacle's start, hybrid and ambiguous markers, peak total pressure, field and speed with and without the sheath, speed change, Group and magnetic cloud index, and a comment.",
        credit: "Lan K. Jian, STEREO Level 3 event lists, STEREO Science Center, https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/; see Jian et al. (2013, 2018), Solar Physics. Public (NASA Heliophysics data, CC0).",
        url: "https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/",
        files: Files::Remote(&[
            "https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/LanJian_STEREO_ICME_List.txt",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "stereo-sirs",
        folder: "Literature/Heliosphere/STEREO (Lan Jian)",
        name: "Stream interaction regions at STEREO A and B (Jian)",
        about: "Stream interaction regions seen by STEREO A and B from 2007, compiled by Lan Jian: `stereo` is A or B, the time of the peak total pressure, the peak pressure, field and density, and the lowest and highest speeds; hybrid (with an ICME) and ambiguous markers.",
        credit: "Lan K. Jian, STEREO Level 3 event lists, STEREO Science Center, https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/; see Jian et al. (2013, 2018), Solar Physics. Public (NASA Heliophysics data, CC0).",
        url: "https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/",
        files: Files::Remote(&[
            "https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/LanJian_STEREO_SIR_List.txt",
        ]),
        read: hpevent,
        part: "SIR_start_time SIR_end_time hybrid_event_marker ambiguous_event_marker STEREO Ptmax_time Ptmax Bmax Npmax Vmin Vmax",
    },
    List {
        key: "stereo-shocks",
        folder: "Literature/Heliosphere/STEREO (Lan Jian)",
        name: "Interplanetary shocks at STEREO A and B (Jian)",
        about: "Shocks seen by STEREO A and B from 2007, compiled by Lan Jian: `stereo` is A or B, the field jump, the shock normal angle, upstream β, magnetosonic Mach number, burst data availability, the driver and a comment (F forward, R reverse).",
        credit: "Lan K. Jian, STEREO Level 3 event lists, STEREO Science Center, https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/; see Jian et al. (2013, 2018), Solar Physics. Public (NASA Heliophysics data, CC0).",
        url: "https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/",
        files: Files::Remote(&[
            "https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/LanJian_STEREO_Shock_List.txt",
        ]),
        read: hpevent,
        part: "",
    },
    List {
        key: "stereo-seps",
        folder: "Literature/Heliosphere/STEREO (Lan Jian)",
        name: "Solar energetic particle events at STEREO A and B (Jian)",
        about: "SEP events at STEREO A and B from 2006, when 13-100 MeV protons from HET exceed 10 pfu (like NOAA's solar proton events): `stereo` is A or B, the peak flux and the fluence, and a comment.",
        credit: "Lan K. Jian, STEREO Level 3 event lists, STEREO Science Center, https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/; see Jian et al. (2013, 2018), Solar Physics. Public (NASA Heliophysics data, CC0).",
        url: "https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/",
        files: Files::Remote(&[
            "https://stereo-ssc.nascom.nasa.gov/pub/ins_data/impact/level3/SEPs_spase_hpevent.txt",
        ]),
        read: hpevent,
        part: "",
    },
];

/// A UTC time from its calendar parts; the second may carry a fraction.
fn utc(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: f64) -> Option<Timestamp> {
    let month = time::Month::try_from(month).ok()?;
    on_day(
        time::Date::from_calendar_date(year, month, day).ok()?,
        hour,
        minute,
        second,
    )
}

/// A UTC time from its year, day of the year and clock.
fn utc_doy(year: i32, doy: u16, hour: u8, minute: u8, second: f64) -> Option<Timestamp> {
    on_day(
        time::Date::from_ordinal_date(year, doy).ok()?,
        hour,
        minute,
        second,
    )
}

/// A second outside 0-60 counts from the minute: MESSENGER's lists write 18:29:59.8 as
/// `18 30 -0.2`, rounding.
fn on_day(date: time::Date, hour: u8, minute: u8, second: f64) -> Option<Timestamp> {
    if !(-60.0..120.0).contains(&second) || hour > 23 || minute > 59 {
        return None;
    }
    let midnight = date.midnight().assume_utc().unix_timestamp_nanos();
    let clock = i128::from(hour) * 3_600_000_000_000
        + i128::from(minute) * 60_000_000_000
        + (second * 1e6).round() as i128 * 1_000;
    Timestamp::from_unix_nanos(i64::try_from(midnight + clock).ok()?)
}

fn number<T: std::str::FromStr>(text: &str, what: &str) -> Result<T, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("bad {what}: {text:?}"))
}

/// The value of a cell, a number when it reads as one; nothing for a missing one.
fn value(text: &str) -> Option<Value> {
    let text = text.trim();
    (!catcsv::missing(text))
        .then(|| catcsv::number_or_text(text))
        .flatten()
}

/// Puts each (name, cell) that holds a value into the attributes.
fn keep<'a>(
    attributes: &mut Map<String, Value>,
    cells: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    for (name, cell) in cells {
        if let Some(value) = value(cell) {
            attributes.insert(name.to_owned(), value);
        }
    }
}

fn point(id: String, at: Timestamp, attributes: Map<String, Value>) -> Item {
    Item {
        id,
        start: at,
        stop: at,
        attributes,
    }
}

/// Juno's crossings: `;`-separated with a header, a date `2016/06/25` and a time `21:20`.
fn juno_crossings(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    const KEPT: [(&str, &str); 10] = [
        ("In/Out", "direction"),
        ("Notes", "notes"),
        ("x (JSS)", "x_jss_rj"),
        ("y (JSS)", "y_jss_rj"),
        ("z (JSS)", "z_jss_rj"),
        ("r (IAU)", "r_rj"),
        ("Dynamic Pressure (nPa)", "pdyn_npa"),
        (
            "Magnetopause Standoff Distance (Jovian radius)",
            "mp_standoff_rj",
        ),
        (
            "Bow Shock Standoff Distance (Jovian radius)",
            "bs_standoff_rj",
        ),
        ("Day of Year", "day_of_year"),
    ];
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_reader(text.as_bytes());
    let headers = reader.headers().map_err(|error| error.to_string())?.clone();
    let column = |name: &str| headers.iter().position(|header| header.trim() == name);
    let (Some(id), Some(date), Some(clock)) = (
        column("#"),
        column("Date (year/month/day)"),
        column("Time (HH:MM)"),
    ) else {
        return Err("the crossings file lacks its id, date or time column".to_owned());
    };
    reader
        .records()
        .map(|record| {
            let record = record.map_err(|error| error.to_string())?;
            let cell = |index: usize| record.get(index).unwrap_or("").trim();
            let at = juno_time(cell(date), cell(clock))
                .ok_or_else(|| format!("{}: bad date or time", cell(id)))?;
            let mut attributes = Map::new();
            keep(
                &mut attributes,
                KEPT.iter()
                    .filter_map(|(header, name)| Some((*name, cell(column(header)?)))),
            );
            Ok(point(cell(id).to_owned(), at, attributes))
        })
        .collect()
}

fn juno_time(date: &str, clock: &str) -> Option<Timestamp> {
    let mut day = date.split('/').map(str::parse::<u32>);
    let (year, month, day) = (day.next()?.ok()?, day.next()?.ok()?, day.next()?.ok()?);
    let (hour, minute) = clock.split_once(':')?;
    utc(
        i32::try_from(year).ok()?,
        u8::try_from(month).ok()?,
        u8::try_from(day).ok()?,
        hour.parse().ok()?,
        minute.parse().ok()?,
        0.0,
    )
}

/// Cassini's crossings of one `boundary`: `Year DOY Hour MIN Type Dir X Y Z`, whitespace
/// separated; the file's other types (data gaps, `DG`) are not crossings. A position of
/// `0 0 0` is none.
fn cassini_crossings(text: &str, boundary: &str) -> Result<Vec<Item>, String> {
    text.lines()
        .skip(1)
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|cells| cells.get(4) == Some(&boundary))
        .map(|cells| {
            let [year, doy, hour, minute, _, direction, ..] = cells[..] else {
                return Err(format!("short line: {cells:?}"));
            };
            let at = utc_doy(
                number(year, "year")?,
                number(doy, "day")?,
                number(hour, "hour")?,
                number(minute, "minute")?,
                0.0,
            )
            .ok_or_else(|| format!("bad time: {cells:?}"))?;
            let mut attributes = Map::new();
            let direction = match direction {
                "I" => "in",
                "O" => "out",
                other => other,
            };
            attributes.insert("direction".to_owned(), Value::from(direction));
            let position = &cells[6..cells.len().min(9)];
            if position.len() == 3
                && position
                    .iter()
                    .any(|cell| cell.parse::<f64>().is_ok_and(|v| v != 0.0))
            {
                keep(
                    &mut attributes,
                    ["x_rs", "y_rs", "z_rs"]
                        .into_iter()
                        .zip(position.iter().copied()),
                );
            }
            let id = format!("{year}-{doy}-{hour}{minute}-{boundary}-{direction}");
            Ok(point(id, at, attributes))
        })
        .collect()
}

/// MESSENGER's crossings: the first crossing's year, month, day, hour, minute and second,
/// the last one's, the boundary's code and `s`ingle or `m`ultiple. The two times of a single
/// crossing can differ by rounding, in either order.
fn messenger_crossings(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let cells: Vec<&str> = line.split_whitespace().collect();
            let time = |at: usize| -> Result<Timestamp, String> {
                let part = |offset: usize| cells.get(at + offset).copied().unwrap_or("");
                utc(
                    number(part(0), "year")?,
                    number(part(1), "month")?,
                    number(part(2), "day")?,
                    number(part(3), "hour")?,
                    number(part(4), "minute")?,
                    number(part(5), "second")?,
                )
                .ok_or_else(|| format!("bad time in {line:?}"))
            };
            let (first, last) = (time(0)?, time(6)?);
            let mut attributes = Map::new();
            let crossing = match cells.get(13) {
                Some(&"s") => "single",
                Some(&"m") => "multiple",
                _ => return Err(format!("no crossing kind in {line:?}")),
            };
            attributes.insert("crossing".to_owned(), Value::from(crossing));
            let id = cells[..6].join("-");
            Ok(Item {
                id,
                start: first.min(last),
                stop: first.max(last),
                attributes,
            })
        })
        .collect()
}

/// A year of AKR bursts, in TFCat: a GeoJSON feature per burst, outlined in time (unix
/// seconds) and frequency (kHz), with Wind's mean position as properties.
fn akr_bursts(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    const KEPT: [(&str, &str); 7] = [
        ("LT_gse", "lt_gse_h"),
        ("radius", "radius_re"),
        ("lat_gse", "lat_gse_deg"),
        ("lon_gse", "lon_gse_deg"),
        ("x_gse", "x_gse_re"),
        ("y_gse", "y_gse_re"),
        ("z_gse", "z_gse_re"),
    ];
    let collection: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let features = collection["features"].as_array().ok_or("no features")?;
    features
        .iter()
        .map(|feature| {
            let mut corners = Vec::new();
            vertices(&feature["geometry"]["coordinates"], &mut corners);
            let times = corners.iter().map(|(time, _)| *time);
            let frequencies = corners.iter().map(|(_, frequency)| *frequency);
            let (Some(first), Some(last)) =
                (times.clone().reduce(f64::min), times.reduce(f64::max))
            else {
                return Err(format!("burst {} has no outline", feature["id"]));
            };
            let at = |seconds: f64| {
                Timestamp::from_unix_nanos((seconds * 1e6).round() as i64 * 1_000)
                    .ok_or_else(|| format!("bad time {seconds}"))
            };
            let mut attributes = Map::new();
            if let (Some(low), Some(high)) = (
                frequencies.clone().reduce(f64::min),
                frequencies.reduce(f64::max),
            ) {
                attributes.insert("f_min_khz".to_owned(), Value::from(low));
                attributes.insert("f_max_khz".to_owned(), Value::from(high));
            }
            for (property, name) in KEPT {
                if let Some(number) = feature["properties"][property].as_f64() {
                    attributes.insert(name.to_owned(), Value::from(number));
                }
            }
            let start = at(first)?;
            Ok(Item {
                id: format!("{}", (first * 1e6).round() as i64),
                start,
                stop: at(last)?,
                attributes,
            })
        })
        .collect()
}

/// Every (time, frequency) pair of a GeoJSON coordinate array, however deeply nested.
fn vertices(coordinates: &Value, into: &mut Vec<(f64, f64)>) {
    match coordinates.as_array().map(Vec::as_slice) {
        Some([Value::Number(time), Value::Number(frequency), ..]) => {
            if let (Some(time), Some(frequency)) = (time.as_f64(), frequency.as_f64()) {
                into.push((time, frequency));
            }
        }
        Some(nested) => nested.iter().for_each(|inner| vertices(inner, into)),
        None => {}
    }
}

/// A table kept in this crate: a header, `start` and `stop` in RFC 3339, then the attributes.
fn kept_table(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    timed_csv(text, "start", "stop")
}

/// A frozen CSV with a header, its `start` and `stop` columns in RFC 3339 (a stop in year 1 is
/// none: a point), every other column an attribute.
fn timed_csv(text: &str, start: &str, stop: &str) -> Result<Vec<Item>, String> {
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers = reader.headers().map_err(|error| error.to_string())?.clone();
    let column = |name: &str| headers.iter().position(|header| header == name);
    let (Some(start_at), Some(stop_at)) = (column(start), column(stop)) else {
        return Err(format!("no {start} or {stop} column"));
    };
    reader
        .records()
        .map(|record| {
            let record = record.map_err(|error| error.to_string())?;
            let first = record
                .get(start_at)
                .and_then(Timestamp::parse)
                .ok_or_else(|| format!("bad {start} in {record:?}"))?;
            let last = record
                .get(stop_at)
                .filter(|cell| !cell.starts_with("0001-"))
                .and_then(Timestamp::parse)
                .map_or(first, |last| last.max(first));
            let mut attributes = Map::new();
            keep(
                &mut attributes,
                headers
                    .iter()
                    .zip(record.iter())
                    .enumerate()
                    .filter(|(index, _)| *index != start_at && *index != stop_at)
                    .map(|(_, pair)| pair),
            );
            // The files read this way are frozen: the whole row keys its event.
            let id = record.iter().collect::<Vec<_>>().join("|");
            Ok(Item {
                id,
                start: first,
                stop: last,
                attributes,
            })
        })
        .collect()
}

/// The SITL segments: no header, `, ` between 25 cells (the last, the discussion, may hold
/// more), times as `2015-05-06 01:45:25`.
fn sitl_segments(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let cells: Vec<&str> = line.splitn(25, ", ").collect();
            let [id, start, stop, _, fom, _, _, status, _, sitl, ..] = cells[..] else {
                return Err(format!("short segment: {line:?}"));
            };
            let time = |text: &str| {
                Timestamp::parse(&format!("{}Z", text.trim().replacen(' ', "T", 1)))
                    .ok_or_else(|| format!("segment {id}: bad time {text:?}"))
            };
            let mut attributes = Map::new();
            let discussion = cells.get(24).copied().unwrap_or("");
            keep(
                &mut attributes,
                [
                    ("fom", fom),
                    ("status", status),
                    ("sitl", sitl),
                    ("discussion", discussion),
                ],
            );
            Ok(Item {
                id: id.trim().to_owned(),
                start: time(start)?,
                stop: time(stop)?,
                attributes,
            })
        })
        .collect()
}

/// LINKCAT's 39 whitespace-separated columns, in the order of its header file, with the
/// units the header file gives; `None` for the columns that are the event's key and ends.
const LINKCAT_COLUMNS: [Option<&str>; 39] = [
    Some("hicat_id"),
    Some("sse_launch_time"),
    Some("target_name"),
    Some("target_arrival_time"),
    None,
    Some("sc_insitu"),
    Some("icme_start_time"),
    Some("arrival_difference_h"),
    Some("sc_heliodistance_au"),
    Some("sc_long_heeq_deg"),
    Some("sc_lat_heeq_deg"),
    Some("source_type"),
    Some("source_long_heeq_deg"),
    Some("source_lat_heeq_deg"),
    Some("flare_class"),
    Some("flare_start_time"),
    Some("flare_end_time"),
    Some("flare_peak_time"),
    Some("cme_sse_long_heeq_deg"),
    Some("cme_sse_lat_heeq_deg"),
    Some("cme_sse_speed_km_s"),
    Some("cme_target_speed_km_s"),
    None,
    None,
    Some("mo_bmean_nt"),
    Some("mo_bstd_nt"),
    Some("mo_bzmean_nt"),
    Some("mo_bzmin_nt"),
    Some("mo_mva_axis_long_deg"),
    Some("mo_mva_axis_lat_deg"),
    Some("mo_mva_ratio"),
    Some("gsr_start_time"),
    Some("gsr_end_time"),
    Some("gsr_axis_long_deg"),
    Some("gsr_axis_lat_deg"),
    Some("gsr_impact_au"),
    Some("vht_x_km_s"),
    Some("vht_y_km_s"),
    Some("vht_z_km_s"),
];
const LINKCAT_ICMECAT_ID: usize = 4;
const LINKCAT_ICME_START: usize = 6;
const LINKCAT_MO_START: usize = 22;
const LINKCAT_MO_END: usize = 23;

/// `2007-05-25T03:35Z`, as HELCATS writes a time.
fn helcats_time(text: &str) -> Option<Timestamp> {
    let minutes = text.strip_suffix('Z')?;
    Timestamp::parse(&format!("{minutes}:00Z"))
}

/// LINKCAT: one row per CME and in situ target; the event is the magnetic obstacle.
fn linkcat(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let cells: Vec<&str> = line.split_whitespace().collect();
            if cells.len() != LINKCAT_COLUMNS.len() {
                return Err(format!(
                    "{} columns, not {}: {line:?}",
                    cells.len(),
                    LINKCAT_COLUMNS.len()
                ));
            }
            let id = cells[LINKCAT_ICMECAT_ID];
            // A missing time is written 9999-99-99T99:99Z: without the obstacle's start, the
            // ICME's; without its end, a point.
            let start = helcats_time(cells[LINKCAT_MO_START])
                .or_else(|| helcats_time(cells[LINKCAT_ICME_START]))
                .ok_or_else(|| format!("{id}: no start"))?;
            let stop = helcats_time(cells[LINKCAT_MO_END]).map_or(start, |stop| stop.max(start));
            let mut attributes = Map::new();
            for (name, cell) in LINKCAT_COLUMNS.iter().zip(&cells) {
                let Some(name) = name else { continue };
                let value = match name.ends_with("_time") {
                    true => helcats_time(cell)
                        .and_then(|at| at.rfc3339().ok())
                        .map(Value::from),
                    false => value(cell),
                };
                if let Some(value) = value {
                    attributes.insert((*name).to_owned(), value);
                }
            }
            Ok(Item {
                id: id.to_owned(),
                start,
                stop,
                attributes,
            })
        })
        .collect()
}

/// A column name as an attribute name: `Magnetic Field Upstream (nT)` becomes
/// `magnetic_field_upstream_nt`.
fn slug(name: &str) -> String {
    let spaced: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    spaced.split_whitespace().collect::<Vec<_>>().join("_")
}

/// A time in unix seconds, to the microsecond.
fn unix_seconds(seconds: f64) -> Option<Timestamp> {
    Timestamp::from_unix_nanos((seconds * 1e6).round() as i64 * 1_000)
}

/// The SHARP database of shocks: `#` comment lines, the last of them naming the columns,
/// then one shock per line; `part` is the spacecraft. The time is in unix seconds, an MMS
/// burst interval also when there is one, and `-1e30` fills a value missing.
fn sharp_shocks(text: &str, part: &str) -> Result<Vec<Item>, String> {
    const FILL: f64 = -1e29;
    let names: Vec<String> = text
        .lines()
        .filter_map(|line| line.strip_prefix("#Spacecraft,"))
        .map(|rest| rest.split(',').map(|name| name.trim().to_owned()).collect())
        .next()
        .ok_or("no column line (#Spacecraft,…)")?;
    let column = |name: &str| names.iter().position(|known| known == name);
    let time = column("time").ok_or("no time column")?;
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.split(',').map(str::trim).collect::<Vec<_>>())
        .filter(|cells| cells.first() == Some(&part))
        .map(|cells| {
            let values = &cells[1..];
            let seconds = |at: usize| values.get(at).and_then(|cell| cell.parse::<f64>().ok());
            let at = seconds(time)
                .and_then(unix_seconds)
                .ok_or_else(|| format!("bad time in {cells:?}"))?;
            let mut attributes = Map::new();
            for (index, name) in names.iter().enumerate() {
                let Some(cell) = values.get(index) else {
                    continue;
                };
                match name.as_str() {
                    "time" => {}
                    "direction" => {
                        let direction = match *cell {
                            "1" => "in",
                            "-1" => "out",
                            other => other,
                        };
                        attributes.insert("direction".to_owned(), Value::from(direction));
                    }
                    "burst_start" | "burst_end" => {
                        let burst = seconds(index)
                            .filter(|seconds| *seconds > 0.0)
                            .and_then(unix_seconds);
                        if let Some(burst) = burst.and_then(|at| at.rfc3339().ok()) {
                            attributes.insert(format!("{name}_time"), Value::from(burst));
                        }
                    }
                    _ if cell.parse::<f64>().is_ok_and(|value| value <= FILL) => {}
                    _ => keep(&mut attributes, [(name.to_lowercase().as_str(), *cell)]),
                }
            }
            Ok(point(values[time].to_owned(), at, attributes))
        })
        .collect()
}

/// The IPShocks database (Helsinki): a CSV whose first six columns are the shock's year,
/// month, day, hour, minute and second, the others its parameters, named with their units.
fn ipshocks(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let names: Vec<String> = reader
        .headers()
        .map_err(|error| error.to_string())?
        .iter()
        .map(slug)
        .collect();
    reader
        .records()
        .map(|record| {
            let record = record.map_err(|error| error.to_string())?;
            let part = |at: usize| record.get(at).unwrap_or("");
            let at = utc(
                number(part(0), "year")?,
                number(part(1), "month")?,
                number(part(2), "day")?,
                number(part(3), "hour")?,
                number(part(4), "minute")?,
                number(part(5), "second")?,
            )
            .ok_or_else(|| format!("bad time in {record:?}"))?;
            let mut attributes = Map::new();
            keep(
                &mut attributes,
                names.iter().map(String::as_str).zip(record.iter()).skip(6),
            );
            let id = format!("{}/{}", part(6), at.rfc3339().unwrap_or_default());
            Ok(point(id, at, attributes))
        })
        .collect()
}

/// A list in the HPEvent format (CDAWeb's event lists, the STEREO Science Center's), read
/// by cocat's own HPEvent reader. Attributes take the field's name in lower case and its
/// unit; comments write `_` for spaces; STEREO's 1 and -1 are A and B. `part`, when given,
/// names the fields, for a list whose header miscounts them.
fn hpevent(text: &str, part: &str) -> Result<Vec<Item>, String> {
    let names: Vec<&str> = part.split_whitespace().collect();
    let names = (!names.is_empty()).then_some(names.as_slice());
    let list = cocat_core::hpevent::read(text, names).map_err(|error| error.to_string())?;
    let renamed: HashMap<&str, String> = list
        .fields
        .iter()
        .map(|field| {
            let name = field.name.to_lowercase();
            let name = match &field.unit {
                Some(unit) => format!("{name}_{}", slug(unit)),
                None => name,
            };
            (field.name.as_str(), name)
        })
        .collect();
    let first_field = list.fields.first().map(|field| field.name.clone());
    Ok(list
        .events
        .into_iter()
        .map(|event| {
            let label = first_field
                .as_ref()
                .and_then(|name| event.attributes.get(name))
                .map(|value| value.to_string())
                .unwrap_or_default();
            let id = format!("{}|{}|{label}", event.start, event.stop);
            let attributes = event
                .attributes
                .into_iter()
                .map(|(name, value)| {
                    let renamed = renamed
                        .get(name.as_str())
                        .cloned()
                        .unwrap_or_else(|| name.to_lowercase());
                    let value = match (renamed.as_str(), value) {
                        ("stereo", Value::Number(side)) => match side.as_i64() {
                            Some(1) => Value::from("A"),
                            Some(-1) => Value::from("B"),
                            _ => Value::Number(side),
                        },
                        (name, Value::String(text)) if name.contains("comment") => {
                            Value::from(text.replace('_', " "))
                        }
                        (_, value) => value,
                    };
                    (renamed, value)
                })
                .collect();
            Item {
                id,
                start: event.start,
                stop: event.stop,
                attributes,
            }
        })
        .collect())
}

/// The sightings of the Carrington event's aurora (1859), a CSV with its times in UTC.
fn aurora_sightings(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    timed_csv(text, "start_time_utc", "stop_time_utc")
}

/// HIGeoCAT, as JSON: `columns` naming each cell of the `data` rows, units in brackets.
/// The event is the CME's first sighting in HI1; its back-projected launches (one per fit:
/// FP, SSE, HM) are times among its attributes.
fn higeocat(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    let table: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let columns: Vec<&str> = table["columns"]
        .as_array()
        .ok_or("no columns")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let names: Vec<String> = columns
        .iter()
        .map(|column| slug(&column.replace("[kms-1]", "km_s").replace("[UTC]", "time")))
        .collect();
    let column = |name: &str| columns.iter().position(|known| *known == name);
    let (Some(id), Some(date)) = (column("ID"), column("Date [UTC]")) else {
        return Err("no ID or Date [UTC] column".to_owned());
    };
    let time = |text: &str| Timestamp::parse(&format!("{}:00Z", text.trim().replacen(' ', "T", 1)));
    table["data"]
        .as_array()
        .ok_or("no data")?
        .iter()
        .map(|row| {
            let cells: Vec<&str> = row
                .as_array()
                .ok_or("a row is not a list")?
                .iter()
                .map(|cell| cell.as_str().unwrap_or(""))
                .collect();
            let key = cells.get(id).copied().unwrap_or_default();
            let at = cells
                .get(date)
                .and_then(|cell| time(cell))
                .ok_or_else(|| format!("{key}: bad date"))?;
            let mut attributes = Map::new();
            for (index, (name, cell)) in names.iter().zip(&cells).enumerate() {
                if index == id || index == date {
                    continue;
                }
                match name.ends_with("_time") {
                    true => {
                        if let Some(launch) = time(cell).and_then(|at| at.rfc3339().ok()) {
                            attributes.insert(name.clone(), Value::from(launch));
                        }
                    }
                    false => keep(&mut attributes, [(name.as_str(), *cell)]),
                }
            }
            Ok(point(key.to_owned(), at, attributes))
        })
        .collect()
}

/// Whether link `href` fits `pattern`: `#` is a digit, `*` any run of characters, the
/// rest itself. `wi_####.html` finds the CfA's yearly Wind pages.
fn linked(pattern: &str, href: &str) -> bool {
    match (pattern.chars().next(), href.chars().next()) {
        (None, None) => true,
        (Some('*'), _) => {
            linked(&pattern[1..], href)
                || href
                    .chars()
                    .next()
                    .is_some_and(|c| linked(pattern, &href[c.len_utf8()..]))
        }
        (Some('#'), Some(c)) if c.is_ascii_digit() => linked(&pattern[1..], &href[1..]),
        (Some(p), Some(c)) if p == c && p != '#' => {
            linked(&pattern[p.len_utf8()..], &href[c.len_utf8()..])
        }
        _ => false,
    }
}

/// The text of each row's cells, from an HTML page's tables.
fn table_rows(html: &str) -> Vec<Vec<String>> {
    let page = scraper::Html::parse_document(html);
    let rows = scraper::Selector::parse("tr").expect("a valid selector");
    let cells = scraper::Selector::parse("td, th").expect("a valid selector");
    page.select(&rows)
        .map(|row| {
            row.select(&cells)
                .map(|cell| {
                    cell.text()
                        .collect::<String>()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect()
        })
        .collect()
}

/// A day of the year with its fraction, as the CfA writes a shock's time.
fn fractional_day(year: i32, day: f64) -> Option<Timestamp> {
    let midnight = utc_doy(year, 1, 0, 0, 0.0)?;
    let nanos = midnight.unix_nanos()? + ((day - 1.0) * 86_400e6).round() as i64 * 1_000;
    Timestamp::from_unix_nanos(nanos)
}

/// A yearly page of the CfA shock database: rows of Year, Month, Day, DOY (with its
/// fraction), UT, X, Y, Z (GSE, Earth radii), Type, a link to the shock's page and when it
/// was updated. `part` is the page's directory, for the link.
fn cfa_shocks(text: &str, part: &str) -> Result<Vec<Item>, String> {
    let page = scraper::Html::parse_document(text);
    let rows = scraper::Selector::parse("tr").expect("a valid selector");
    let cells = scraper::Selector::parse("td").expect("a valid selector");
    let links = scraper::Selector::parse("a").expect("a valid selector");
    page.select(&rows)
        .map(|row| row.select(&cells).collect::<Vec<_>>())
        .filter(|row| row.len() == 11)
        .filter(|row| {
            let year = row[0].text().collect::<String>();
            year.trim().len() == 4 && year.trim().parse::<i32>().is_ok()
        })
        .map(|row| {
            let cell = |at: usize| row[at].text().collect::<String>().trim().to_owned();
            let at = fractional_day(number(&cell(0), "year")?, number(&cell(3), "day")?)
                .ok_or_else(|| format!("bad day {}", cell(3)))?;
            let mut attributes = Map::new();
            let position = [cell(5), cell(6), cell(7)];
            keep(
                &mut attributes,
                [
                    ("x_gse_re", position[0].as_str()),
                    ("y_gse_re", position[1].as_str()),
                    ("z_gse_re", position[2].as_str()),
                    ("type", cell(8).as_str()),
                ],
            );
            let page = row[9]
                .select(&links)
                .next()
                .and_then(|link| link.value().attr("href"));
            if let Some(page) = page {
                let page = format!("{part}{}", page.trim().trim_start_matches("./"));
                attributes.insert("page".to_owned(), Value::from(page));
            }
            let id = format!("{}-{}", cell(0), cell(3));
            Ok(point(id, at, attributes))
        })
        .collect()
}

/// A flux rope page's time, `1996/01/01 02:01` or with seconds.
fn slash_time(text: &str) -> Option<Timestamp> {
    let iso = text.trim().replace('/', "-").replacen(' ', "T", 1);
    let iso = match iso.len() {
        16 => format!("{iso}:00Z"),
        _ => format!("{iso}Z"),
    };
    Timestamp::parse(&iso)
}

/// What a fluxrope.info column holds, by its header: the attributes it fills, in order.
fn flux_rope_names(header: &str) -> Vec<String> {
    let names: &[&str] = match header {
        "Duration" => &["duration_min"],
        "Duration (s)" => &["duration_s"],
        "Residue" => &["residue"],
        "Rfit, Rdiff" => &["r_fit", "r_diff"],
        "<B> (nT)" => &["b_mean_nt"],
        "Bmax (nT)" => &["b_max_nt"],
        "<β>, <βp>" => &["beta", "beta_p"],
        "<βp>" => &["beta_p"],
        "<Vsw> (km/s)" => &["vsw_km_s"],
        "<Tp> (106K)" => &["tp_mk"],
        "Orientation" => &["theta_deg", "phi_deg"],
        "r (AU)" => &["r_au"],
        other => return vec![slug(other)],
    };
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// A yearly page of the small-scale flux rope database (Hu et al.): one table, a header
/// row, then one rope per row: its number, `start ~ end`, and its parameters. A cell can
/// hold two values (`nan, 1.01`, `θ=60, φ=340`), one per name.
fn flux_ropes(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    let rows = table_rows(text);
    let Some(header) = rows
        .iter()
        .position(|row| row.iter().any(|cell| cell == "Time Range"))
    else {
        return Err("no Time Range column".to_owned());
    };
    let headers = &rows[header];
    let range = headers
        .iter()
        .position(|cell| cell == "Time Range")
        .unwrap_or(1);
    rows[header + 1..]
        .iter()
        .filter(|row| row.len() == headers.len())
        .map(|row| {
            let (first, last) = row[range]
                .split_once('~')
                .ok_or_else(|| format!("no range in {row:?}"))?;
            let (start, stop) = (slash_time(first), slash_time(last));
            let (Some(start), Some(stop)) = (start, stop) else {
                return Err(format!("bad range {:?}", row[range]));
            };
            let mut attributes = Map::new();
            for (header, cell) in headers.iter().zip(row) {
                if header == "No." || header == "Time Range" {
                    continue;
                }
                let names = flux_rope_names(header);
                let values: Vec<&str> = match names.len() {
                    1 => vec![cell.as_str()],
                    _ => cell
                        .split(',')
                        .map(|part| part.rsplit('=').next().unwrap_or(part).trim())
                        .collect(),
                };
                keep(
                    &mut attributes,
                    names.iter().map(String::as_str).zip(values),
                );
            }
            Ok(Item {
                id: row[range].clone(),
                start,
                stop: stop.max(start),
                attributes,
            })
        })
        .collect()
}

/// Whether a cell is one of the CDAWeb tables' fill values, a run of 9s: `999.9`, `9999999.`.
fn nines(cell: &str) -> bool {
    let digits: String = cell.chars().filter(|c| *c != '.').collect();
    !digits.is_empty() && digits.chars().all(|c| c == '9')
}

fn keep_unless_nines<'a>(
    attributes: &mut Map<String, Value>,
    cells: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    keep(
        attributes,
        cells.into_iter().filter(|(_, cell)| !nines(cell.trim())),
    );
}

const CROSSING_SPACECRAFT: [(&str, &str); 21] = [
    ("I8", "IMP 8"),
    ("GE", "Geotail"),
    ("M4", "Magion 4"),
    ("CL", "Cluster"),
    ("A1", "AMPTE CCE"),
    ("A2", "AMPTE IRM"),
    ("E3", "Explorer 33"),
    ("HW", "Hawkeye"),
    ("H2", "HEOS 2"),
    ("I1", "IMP 1"),
    ("I2", "IMP 2"),
    ("I3", "IMP 3"),
    ("I4", "IMP 4"),
    ("I6", "IMP 6"),
    ("IS", "ISEE"),
    ("O5", "OGO 5"),
    ("P7", "Prognoz 7"),
    ("P8", "Prognoz 8"),
    ("P1", "Prognoz 10"),
    ("I7", "IMP 7"),
    ("E2", "Explorer 2"),
];

fn spacecraft(code: &str) -> String {
    CROSSING_SPACECRAFT
        .iter()
        .find(|(known, _)| *known == code)
        .map_or_else(|| code.to_owned(), |(_, name)| (*name).to_owned())
}

/// The bow shock records' 43 fixed-width fields (readme of CDAWeb's bowshock_crossings):
/// width and attribute name; `""` for the time fields read apart.
const BOW_SHOCK_FIELDS: [(usize, &str); 43] = [
    (2, ""),
    (5, ""),
    (4, ""),
    (3, ""),
    (3, ""),
    (3, ""),
    (6, "x_gse_re"),
    (6, "y_gse_re"),
    (6, "z_gse_re"),
    (6, "y_gsm_re"),
    (6, "z_gsm_re"),
    (6, "bx_gse_nt"),
    (6, "by_gse_nt"),
    (6, "bz_gse_nt"),
    (6, "by_gsm_nt"),
    (6, "bz_gsm_nt"),
    (7, "minus_vx_gse_km_s"),
    (7, "minus_vy_gse_km_s"),
    (7, "minus_vz_gse_km_s"),
    (6, "n_up_cm3"),
    (6, "n_down_cm3"),
    (8, "t_up_k"),
    (5, "time_span_min"),
    (6, "b_nt"),
    (7, "flow_speed_km_s"),
    (7, "sound_speed_km_s"),
    (7, "alfven_speed_km_s"),
    (7, "magnetosonic_speed_km_s"),
    (5, "mach_sonic"),
    (6, "mach_alfven"),
    (5, "mach_magnetosonic"),
    (8, "beta"),
    (6, "flow_pressure_npa"),
    (4, "shock_strength"),
    (4, "imf_cone_deg"),
    (4, "imf_clock_deg"),
    (4, "orbit"),
    (2, "leg"),
    (3, "sequence"),
    (2, ""),
    (2, "flag_observation"),
    (2, "flag_shock"),
    (2, "flag_upstream"),
];
const BOW_SHOCK_TRANSITION: usize = 39;

/// A record whose year has two digits or whose day is 0: the Geotail table of 1996 holds
/// one (`GE   96   0 16 11`). Skipped, rather than losing the whole list over it; a record
/// that does not read at all still fails the list, as a changed layout would.
fn impossible_date(line: &str) -> bool {
    let field = |range: std::ops::Range<usize>| {
        line.get(range)
            .map(str::trim)
            .and_then(|cell| cell.parse::<i32>().ok())
    };
    field(2..7).is_some_and(|year| year < 1900) || field(7..11) == Some(0)
}

/// CDAWeb's yearly bow shock tables (IMP 8, Geotail, Magion 4, Cluster): fixed-width
/// records, the shock's time then its position and the upstream solar wind; IMP 8 adds the
/// downstream density, the crossing's direction and quality flags. Seconds of 99 are none.
fn bow_shocks(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !impossible_date(line))
        .map(|line| {
            let mut cells = Vec::with_capacity(BOW_SHOCK_FIELDS.len());
            let mut at = 0;
            for (width, _) in BOW_SHOCK_FIELDS {
                cells.push(line.get(at..at + width).unwrap_or("").trim());
                at += width;
            }
            let second = match cells[5] {
                "99" | "" => 0.0,
                second => number(second, "second")?,
            };
            let start = utc_doy(
                number(cells[1], "year")?,
                number(cells[2], "day")?,
                number(cells[3], "hour")?,
                number(cells[4], "minute")?,
                second,
            )
            .ok_or_else(|| format!("bad time in {line:?}"))?;
            let mut attributes = Map::new();
            attributes.insert("spacecraft".to_owned(), Value::from(spacecraft(cells[0])));
            keep_unless_nines(
                &mut attributes,
                BOW_SHOCK_FIELDS
                    .iter()
                    .zip(&cells)
                    .filter(|((_, name), _)| !name.is_empty())
                    .map(|((_, name), cell)| (*name, *cell)),
            );
            let transition = match cells[BOW_SHOCK_TRANSITION] {
                "0" => Some("into solar wind"),
                "1" => Some("into magnetosheath"),
                "2" => Some("unresolved"),
                _ => None,
            };
            if let Some(transition) = transition {
                attributes.insert("transition".to_owned(), Value::from(transition));
            }
            let id = format!(
                "{}-{}-{}-{}{}{}",
                cells[0], cells[1], cells[2], cells[3], cells[4], cells[5]
            );
            Ok(point(id, start, attributes))
        })
        .collect()
}

/// The magnetopause records' 39 whitespace-separated fields (readme of CDAWeb's
/// magnetopause_crossings); `""` for the time fields read apart.
const MAGNETOPAUSE_FIELDS: [&str; 39] = [
    "",
    "",
    "",
    "",
    "",
    "x_gse_re",
    "y_gse_re",
    "z_gse_re",
    "y_gsm_re",
    "z_gsm_re",
    "b_avg_nt",
    "b_nt",
    "bx_gse_nt",
    "by_gse_nt",
    "bz_gse_nt",
    "by_gsm_nt",
    "bz_gsm_nt",
    "flow_speed_km_s",
    "minus_vx_gse_km_s",
    "minus_vy_gse_km_s",
    "minus_vz_gse_km_s",
    "t_k",
    "n_cm3",
    "flow_pressure_npa",
    "ey_mv_m",
    "beta",
    "alfven_speed_km_s",
    "sound_speed_km_s",
    "magnetosonic_speed_km_s",
    "mach_alfven",
    "mach_sonic",
    "mach_magnetosonic",
    "imf_cone_deg",
    "imf_clock_deg",
    "kp_x10",
    "sunspot_number",
    "dst_nt",
    "ae_nt",
    "source_code",
];

/// CDAWeb's yearly magnetopause tables, 1963-1994: the crossing's time and position, then
/// the solar wind of its hour from OMNI and the geomagnetic indices.
fn omni_magnetopause(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let cells: Vec<&str> = line.split_whitespace().collect();
            if cells.len() != MAGNETOPAUSE_FIELDS.len() {
                return Err(format!(
                    "{} fields, not {}: {line:?}",
                    cells.len(),
                    MAGNETOPAUSE_FIELDS.len()
                ));
            }
            let start = utc_doy(
                number(cells[1], "year")?,
                number(cells[2], "day")?,
                number(cells[3], "hour")?,
                number(cells[4], "minute")?,
                0.0,
            )
            .ok_or_else(|| format!("bad time in {line:?}"))?;
            let mut attributes = Map::new();
            attributes.insert("spacecraft".to_owned(), Value::from(spacecraft(cells[0])));
            keep_unless_nines(
                &mut attributes,
                MAGNETOPAUSE_FIELDS
                    .iter()
                    .zip(&cells)
                    .filter(|(name, _)| !name.is_empty())
                    .map(|(name, cell)| (*name, *cell)),
            );
            let id = format!(
                "{}-{}-{}-{}{}",
                cells[0], cells[1], cells[2], cells[3], cells[4]
            );
            Ok(point(id, start, attributes))
        })
        .collect()
}

fn event(list: &List, item: Item) -> EventRecord {
    let uuid = Uuid::new_v5(&NAMESPACE, format!("{}/{}", list.key, item.id).as_bytes());
    let mut event = EventRecord::new(uuid.to_string(), item.start, item.stop);
    event.attributes = item.attributes;
    event
}

fn fetched(list: &List, events: Result<Vec<EventRecord>, String>) -> Fetched {
    Fetched {
        key: list.key.to_owned(),
        folder: list.folder.to_owned(),
        name: list.name.to_owned(),
        description: format!("{} Cite: {}", list.about, list.credit),
        url: list.url.to_owned(),
        catalogue: Uuid::new_v5(&NAMESPACE, list.key.as_bytes()).to_string(),
        columns: Vec::new(),
        events,
    }
}

/// The list's events, read from every file it is spread over; a file without an event means
/// its layout changed.
fn events(list: &List, texts: &[String]) -> Result<Vec<EventRecord>, String> {
    let mut events = Vec::new();
    for text in texts {
        let items = (list.read)(text, list.part)?;
        if items.is_empty() {
            return Err("no event found, has the file layout changed?".to_owned());
        }
        events.extend(items.into_iter().map(|item| event(list, item)));
    }
    Ok(events)
}

async fn download(http: &reqwest::Client, url: &str) -> Result<String, String> {
    // Lenient: some headers carry a name in another encoding (Wang's list at CDAWeb).
    let bytes = super::patient::fetch(http, url, BUSY_PAUSE).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The pages `index` links to that fit `pattern`, as absolute URLs, each once, in order.
fn linked_pages(index: &str, html: &str, pattern: &str) -> Result<Vec<String>, String> {
    let base = reqwest::Url::parse(index).map_err(|error| format!("{index}: {error}"))?;
    let page = scraper::Html::parse_document(html);
    let anchors = scraper::Selector::parse("a[href]").expect("a valid selector");
    let mut urls: Vec<String> = page
        .select(&anchors)
        .filter_map(|anchor| anchor.value().attr("href"))
        .map(|href| href.trim().trim_start_matches("./"))
        .filter(|href| linked(pattern, href))
        .filter_map(|href| base.join(href).ok().map(String::from))
        .collect();
    urls.sort();
    urls.dedup();
    match urls.is_empty() {
        true => Err(format!(
            "{index} links to no page like {pattern}, has the site changed?"
        )),
        false => Ok(urls),
    }
}

/// The list's files; a file several lists share (SHARP's) is downloaded once a run.
async fn texts(
    http: &reqwest::Client,
    files: &Files,
    downloaded: &mut HashMap<&'static str, Result<String, String>>,
) -> Result<Vec<String>, String> {
    match files {
        Files::Kept(text) => Ok(vec![(*text).to_owned()]),
        Files::Linked { index, pattern } => {
            let mut texts = Vec::new();
            for url in linked_pages(index, &download(http, index).await?, pattern)? {
                texts.push(download(http, &url).await?);
            }
            Ok(texts)
        }
        Files::Remote(urls) => {
            let mut texts = Vec::with_capacity(urls.len());
            for url in *urls {
                if !downloaded.contains_key(url) {
                    downloaded.insert(url, download(http, url).await);
                }
                texts.push(downloaded[url].clone()?);
            }
            Ok(texts)
        }
    }
}

#[async_trait]
impl Provider for Literature {
    fn id(&self) -> &'static str {
        "literature"
    }

    fn source(&self) -> &'static str {
        "Published lists"
    }

    fn default_schedule(&self) -> &'static str {
        "45 6 * * 1"
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let mut lists = Vec::with_capacity(LISTS.len());
        let mut downloaded = HashMap::new();
        for list in &LISTS {
            let events = match texts(http, &list.files, &mut downloaded).await {
                Ok(texts) => events(list, &texts),
                Err(error) => Err(error),
            };
            lists.push(fetched(list, events));
        }
        Ok(lists)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const JUNO_MP: &str = include_str!("../../tests/fixtures/relay/literature_jupiter_mp.csv");
    const CASSINI_FILE: &str = include_str!("../../tests/fixtures/relay/literature_saturn.txt");
    const MESSENGER_BS_IN: &str =
        include_str!("../../tests/fixtures/relay/literature_mercury_bs_in.txt");
    const AKR: &str = include_str!("../../tests/fixtures/relay/literature_akr.json");
    const SITL: &str = include_str!("../../tests/fixtures/relay/literature_sitl.csv");
    const LINKCAT: &str = include_str!("../../tests/fixtures/relay/literature_linkcat.txt");

    fn at(text: &str) -> Timestamp {
        Timestamp::parse(text).unwrap()
    }

    #[test]
    fn a_juno_crossing_is_a_point_with_its_direction_and_position() {
        let items = juno_crossings(JUNO_MP, "").unwrap();
        assert_eq!(items.len(), 4);
        let first = &items[0];
        assert_eq!(first.id, "MP1");
        assert_eq!(
            (first.start, first.stop),
            (at("2016-06-25T21:20:00Z"), at("2016-06-25T21:20:00Z"))
        );
        assert_eq!(first.attributes["direction"], json!("in"));
        assert_eq!(first.attributes["x_jss_rj"], json!(5.744));
        assert_eq!(first.attributes["pdyn_npa"], json!(0.067));
        let with_nan = &items[3];
        assert!(
            !with_nan
                .attributes
                .values()
                .any(|value| value == &json!("nan"))
        );
    }

    #[test]
    fn cassini_crossings_split_by_boundary_and_skip_missing_positions() {
        let magnetopause = cassini_crossings(CASSINI_FILE, "MP").unwrap();
        assert_eq!(magnetopause.len(), 2);
        assert_eq!(
            magnetopause[0].start,
            at("2004-06-28T18:43:00Z"),
            "day 180 of 2004"
        );
        assert_eq!(magnetopause[0].attributes["direction"], json!("in"));
        assert_eq!(magnetopause[1].attributes["direction"], json!("out"));
        assert_eq!(magnetopause[0].attributes["x_rs"], json!(16.97));
        let bow_shock = cassini_crossings(CASSINI_FILE, "BS").unwrap();
        assert_eq!(bow_shock.len(), 1, "data gaps are not crossings");
        assert!(
            !bow_shock[0].attributes.contains_key("x_rs"),
            "0 0 0 is no position"
        );
    }

    #[test]
    fn a_messenger_crossing_spans_from_its_first_to_its_last() {
        let items = messenger_crossings(MESSENGER_BS_IN, "").unwrap();
        assert_eq!(items.len(), 6);
        assert_eq!(
            items[5].stop,
            at("2011-05-07T18:29:59.8Z"),
            "a negative second, from rounding"
        );
        assert_eq!(items[0].start, at("2011-03-23T23:46:56.5Z"));
        assert_eq!(items[0].stop, at("2011-03-23T23:59:41.3Z"));
        assert_eq!(items[0].attributes["crossing"], json!("multiple"));
        assert_eq!(
            items[2].stop,
            at("2011-03-25T01:00:02.2Z"),
            "a one-digit second"
        );
        let single = &items[3];
        assert_eq!(single.attributes["crossing"], json!("single"));
        assert!(
            single.start <= single.stop,
            "its two times differ by rounding"
        );
    }

    #[test]
    fn an_akr_burst_spans_its_time_frequency_outline() {
        let items = akr_bursts(AKR, "").unwrap();
        assert_eq!(items.len(), 2);
        let first = &items[0];
        assert_eq!(
            first.start,
            Timestamp::from_unix_nanos(798_597_938_992_828_000).unwrap()
        );
        assert!(first.stop > first.start);
        assert_eq!(first.attributes["f_min_khz"], json!(80.0));
        assert_eq!(first.attributes["lt_gse_h"], json!(12.915667987497772));
        assert!(first.attributes.contains_key("radius_re"));
    }

    #[test]
    fn the_kept_ballerini_table_reads_whole() {
        let items = kept_table(include_str!("data/ballerini_2024_magnetopause.csv"), "").unwrap();
        assert_eq!(items.len(), 146);
        assert_eq!(items[0].start, at("2015-09-02T15:26:26.4Z"));
        assert_eq!(items[0].stop, at("2015-09-02T15:26:29.6Z"));
        assert_eq!(items[0].attributes["hodogram"], json!("other"));
        assert_eq!(items[0].attributes["width_km"], json!(540.46));
    }

    #[test]
    fn an_sitl_segment_keeps_its_merit_status_and_reason() {
        let items = sitl_segments(SITL, "").unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, "900");
        assert_eq!(items[0].start, at("2015-05-06T01:45:25Z"));
        assert_eq!(items[0].stop, at("2015-05-06T02:35:25Z"));
        assert_eq!(items[0].attributes["fom"], json!(70.0));
        assert_eq!(items[0].attributes["status"], json!("COMPLETE+FINISHED"));
        assert_eq!(items[0].attributes["sitl"], json!("Russ Panneton"));
        assert_eq!(
            items[0].attributes["discussion"],
            json!("FPI.010 -- data segment 4")
        );
    }

    #[test]
    fn a_linkcat_event_is_the_magnetic_obstacle() {
        let items = linkcat(LINKCAT, "").unwrap();
        assert_eq!(items.len(), 4);
        let unknown = &items[3];
        assert_eq!(unknown.id, "ICME_MES_WINSLOW_20111216_01");
        assert_eq!(
            unknown.start, unknown.stop,
            "no magnetic obstacle times: the ICME's start"
        );
        let first = &items[0];
        assert_eq!(first.id, "ICME_VEX_SGOOD_20070525_01");
        assert_eq!(first.start, at("2007-05-25T03:35:00Z"));
        assert_eq!(first.stop, at("2007-05-25T19:59:00Z"));
        assert_eq!(first.attributes["target_name"], json!("VENUS"));
        assert_eq!(first.attributes["sc_heliodistance_au"], json!(0.7208));
        assert_eq!(
            first.attributes["sse_launch_time"],
            json!("2007-05-21T15:26:00Z")
        );
        assert!(
            !first
                .attributes
                .values()
                .any(|value| value == &json!("NaN"))
        );
    }

    const SHARP: &str = include_str!("../../tests/fixtures/relay/literature_sharp.csv");
    const IPSHOCKS: &str = include_str!("../../tests/fixtures/relay/literature_ipshocks.csv");
    const CLOUDS: &str = include_str!("../../tests/fixtures/relay/literature_hpe_clouds.txt");
    const WANG: &str = include_str!("../../tests/fixtures/relay/literature_hpe_wang.txt");
    const CARRINGTON: &str = include_str!("../../tests/fixtures/relay/literature_carrington.csv");

    #[test]
    fn sharp_shocks_split_by_spacecraft_and_skip_fill_values() {
        let mms = sharp_shocks(SHARP, "MMS").unwrap();
        assert_eq!(mms.len(), 2);
        let first = &mms[0];
        assert_eq!(
            first.start,
            Timestamp::from_unix_nanos(1_444_216_750_718_250_000).unwrap()
        );
        assert_eq!(first.start, first.stop, "a crossing");
        assert_eq!(first.attributes["direction"], json!("out"));
        assert!(
            !first.attributes.contains_key("bx_us"),
            "-1e30 is the fill value"
        );
        assert_eq!(first.attributes["by_us"], json!(10.293));
        assert!(first.attributes.contains_key("burst_start_time"));
        assert_eq!(sharp_shocks(SHARP, "THEMIS B").unwrap().len(), 1);
    }

    #[test]
    fn an_ipshocks_row_is_a_shock_with_its_parameters_named_after_their_units() {
        let items = ipshocks(IPSHOCKS, "").unwrap();
        assert_eq!(items.len(), 2);
        let first = &items[0];
        assert_eq!(first.start, at("2024-03-25T10:04:50Z"));
        assert_eq!(first.attributes["spacecraft"], json!("Wind"));
        assert_eq!(first.attributes["shock_type"], json!("FR"));
        assert_eq!(first.attributes["magnetic_field_upstream_nt"], json!(5.99));
        assert!(
            !first
                .attributes
                .values()
                .any(|value| value == &json!("NaN"))
        );
    }

    #[test]
    fn an_hpevent_list_names_its_columns_with_their_units() {
        let clouds = hpevent(CLOUDS, "").unwrap();
        assert_eq!(clouds.len(), 3);
        assert_eq!(clouds[0].start, at("1994-03-03T08:30:00Z"));
        assert_eq!(clouds[0].stop, at("1994-03-03T19:00:00Z"));
        assert_eq!(clouds[0].attributes["category"], json!("IMC"));
        assert_eq!(
            clouds[0].attributes["comments"],
            json!("Likely start earlier during data gap,")
        );
        let wang = hpevent(WANG, "").unwrap();
        assert_eq!(wang.len(), 3);
        assert_eq!(wang[0].start, wang[0].stop, "a crossing");
        assert_eq!(wang[0].attributes["sc_name"], json!("IMP3"));
        assert_eq!(wang[0].attributes["r_re"], json!(11.2039));
        assert_eq!(
            wang[0].attributes["lat_gsm_radians"],
            json!(1.1755200000000001)
        );
    }

    #[test]
    fn a_carrington_sighting_keeps_its_place() {
        let items = aurora_sightings(CARRINGTON, "").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].start, at("1859-09-02T23:10:52.8Z"));
        assert_eq!(items[0].attributes["observatory"], json!("Preston-England"));
        assert_eq!(items[0].attributes["geo_latitude_deg"], json!(53.77));
    }

    #[test]
    fn a_higeocat_cme_is_its_first_sighting_with_its_launch_times() {
        let items = higeocat(
            include_str!("../../tests/fixtures/relay/literature_higeocat.json"),
            "",
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        let first = &items[0];
        assert_eq!(first.id, "HCME_A__20070419_01");
        assert_eq!(
            (first.start, first.stop),
            (at("2007-04-19T13:30:00Z"), at("2007-04-19T13:30:00Z"))
        );
        assert_eq!(first.attributes["sc"], json!("A"));
        assert_eq!(first.attributes["quality"], json!("good"));
        assert_eq!(first.attributes["sse_speed_km_s"], json!(389.0));
        assert_eq!(
            first.attributes["sse_launch_time"],
            json!("2007-04-19T06:50:00Z")
        );
        assert!(
            !first.attributes.contains_key("l_n"),
            "empty cells are left out"
        );
    }

    #[test]
    fn a_link_pattern_takes_digits_for_hashes_and_anything_for_stars() {
        assert!(linked("wi_####.html", "wi_2000.html"));
        assert!(!linked("wi_####.html", "wi_data.html"));
        assert!(linked("ACE####/year####.html", "ACE1998/year1998.html"));
        assert!(!linked("####/year####.html", "ACE1998/year1998.html"));
        assert!(linked("*_####.txt", "ampte_cce_1984.txt"));
        assert!(!linked("*_####.txt", "00readme.txt"));
    }

    #[test]
    fn a_cfa_shock_is_at_its_fractional_day_with_its_page() {
        let base = "https://lweb.cfa.harvard.edu/shocks/wi_data/";
        let items = cfa_shocks(
            include_str!("../../tests/fixtures/relay/literature_cfa_wind.html"),
            base,
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        let first = &items[0];
        // Day 36.643385 of 2000: 5 February, 15:26:28.46.
        assert_eq!(first.start, at("2000-02-05T15:26:28.464Z"));
        assert_eq!(first.start, first.stop);
        assert_eq!(first.attributes["type"], json!("FF"));
        assert_eq!(first.attributes["x_gse_re"], json!(90.81));
        assert_eq!(
            first.attributes["page"],
            json!("https://lweb.cfa.harvard.edu/shocks/wi_data/00152/wi_00152.html")
        );
    }

    #[test]
    fn a_flux_rope_spans_its_time_range_and_splits_paired_cells() {
        let wind = flux_ropes(
            include_str!("../../tests/fixtures/relay/literature_fluxropes_wind.html"),
            "",
        )
        .unwrap();
        assert_eq!(wind.len(), 2);
        assert_eq!(wind[0].start, at("1996-01-01T02:01:00Z"));
        assert_eq!(wind[0].stop, at("1996-01-01T02:56:00Z"));
        assert_eq!(wind[0].attributes["duration_min"], json!(56.0));
        assert_eq!(wind[0].attributes["beta_p"], json!(1.01));
        assert!(!wind[0].attributes.contains_key("beta"), "nan is missing");
        assert_eq!(wind[0].attributes["theta_deg"], json!(60.0));
        assert_eq!(wind[0].attributes["phi_deg"], json!(340.0));
        assert_eq!(wind[0].attributes["vsw_km_s"], json!(388.0));
        let psp = flux_ropes(
            include_str!("../../tests/fixtures/relay/literature_fluxropes_psp.html"),
            "",
        )
        .unwrap();
        assert_eq!(psp[0].start, at("2024-01-01T00:13:32Z"));
        assert_eq!(psp[0].attributes["r_fit"], json!(0.0788));
        assert_eq!(psp[0].attributes["r_au"], json!(0.17));
    }

    #[test]
    fn a_bow_shock_record_is_cut_at_its_documented_widths() {
        let items = bow_shocks(
            include_str!("../../tests/fixtures/relay/literature_bowshocks.txt"),
            "",
        )
        .unwrap();
        assert_eq!(items.len(), 3);
        let cluster = &items[0];
        assert_eq!(cluster.start, at("2001-02-02T20:34:00Z"), "day 33 of 2001");
        assert_eq!(cluster.attributes["spacecraft"], json!("Cluster"));
        assert_eq!(cluster.attributes["minus_vx_gse_km_s"], json!(378.7));
        assert!(
            !cluster.attributes.contains_key("n_down_cm3"),
            "999.9 is a fill value"
        );
        let imp = &items[2];
        assert_eq!(imp.attributes["spacecraft"], json!("IMP 8"));
        assert_eq!(imp.attributes["time_span_min"], json!(6.0));
        assert_eq!(imp.attributes["sequence"], json!(4.0));
        assert_eq!(
            imp.attributes["transition"],
            json!("unresolved"),
            "code 2: the same domain on both sides"
        );
    }

    #[test]
    fn an_omni_magnetopause_record_keeps_the_hour_of_solar_wind_around_it() {
        let items = omni_magnetopause(
            include_str!("../../tests/fixtures/relay/literature_omni_magnetopause.txt"),
            "",
        )
        .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].start, at("1978-01-02T03:10:00Z"));
        assert_eq!(items[0].attributes["spacecraft"], json!("ISEE"));
        assert_eq!(items[0].attributes["x_gse_re"], json!(3.9));
        assert_eq!(items[0].attributes["imf_cone_deg"], json!(65.0));
        assert!(
            !items[0].attributes.contains_key("flow_speed_km_s"),
            "9999.9 is a fill value"
        );
    }

    #[test]
    fn a_stereo_sir_takes_the_names_its_header_miscounts() {
        let sirs = include_str!("../../tests/fixtures/relay/literature_stereo_sirs.txt");
        let list = LISTS.iter().find(|list| list.key == "stereo-sirs").unwrap();
        let items = hpevent(sirs, list.part).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].start, at("2007-02-25T17:40:00Z"));
        assert_eq!(items[0].stop, at("2007-03-01T01:10:00Z"));
        assert_eq!(items[0].attributes["stereo"], json!("A"));
        assert_eq!(items[0].attributes["vmax_km_s"], json!(693));
        assert_eq!(
            items[0].attributes["ptmax_time"],
            json!("2007-02-26T02:15:00.000Z")
        );
    }

    #[test]
    fn every_list_has_a_unique_key_and_says_whom_to_cite() {
        let mut keys: Vec<_> = LISTS.iter().map(|list| list.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), LISTS.len());
        assert!(
            LISTS
                .iter()
                .all(|list| list.credit.contains("doi.org") || list.credit.contains("http"))
        );
    }
}
