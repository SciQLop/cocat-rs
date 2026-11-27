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

mod vizier;

pub struct Literature;

const NAMESPACE: Uuid = Uuid::from_u128(0x9d4e2a71_3c58_4b0f_a6e1_52f7c8d3b194);
const BUSY_PAUSE: Duration = Duration::from_secs(30);
const MIRROR_ROUNDS: usize = 3;
const MIRROR_PAUSE: Duration = Duration::from_secs(10);
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
#[derive(Clone)]
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
    /// One file served by several mirrors: the first answer `answered` accepts. A mirror can
    /// answer with an error page that is not an HTTP error (VizieR when its database is down),
    /// so the mirrors are tried again, a few times, after a pause.
    Mirrored {
        urls: &'static [&'static str],
        answered: fn(&str) -> Result<(), String>,
    },
    /// Files that only make sense together: downloaded on each run, they reach `read` as one
    /// text, each after a [`FILE_BREAK`].
    Together(&'static [&'static str]),
    /// The answer to `form` posted to `url`, for a site that exports its table only through a
    /// form's button (TUMS): asked on each run, once however many lists read it.
    Posted {
        url: &'static str,
        form: &'static [(&'static str, &'static str)],
    },
}

#[derive(Clone)]
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

const LLAMAICE_CREDIT: &str = "Kay, C., Davies, E. E., Dumbović, M., Martinić, K., Palmerio, E., Rüdisser, H. T., Weiler, E. & Möstl, C. (2026), Collection, Collation, and Comparison of Near-Earth In Situ CME Boundaries, Space Weather, https://doi.org/10.1029/2025SW004753 (paper CC-BY-4.0); LLAMAICE v1.0 at https://osprei.space/llamaice, which states no licence for the files.";
const LLAMAICE_FOLDER: &str = "Literature/Heliosphere/LLAMAICE near-Earth CMEs (Kay)";
const LLAMAICE_SOURCES: &str = "https://osprei.space/llamadata/otherCats_1.0.dat";

/// The description of a region list from our own run of the Olshevsky et al. (2021) classifier:
/// it must say first that this is our version, with how the run differs from the paper.
const KOLLER: &str = "Koller (2025), THEMIS Dayside Magnetosheath Intervals and detected Jets 2008 - 2023; SIR and HSS Event Lists 1995-2023, Zenodo, https://doi.org/10.5281/zenodo.13843752, licence CC-BY-4.0; see Koller et al. (2025), Stability of the Earth's Dayside Magnetosheath, JGR Space Physics 130, https://doi.org/10.1029/2025JA034098.";
const TYPE_III: &str = "HELIO Heliophysics Feature Catalogue (Observatoire de Paris), feature TYPE III, code RABAT3 v2.03 (X. Bonnin), https://voparis-helio.obspm.fr/hfc-gui/; method of Lobzin et al. (2009), Space Weather 7, https://doi.org/10.1029/2008SW000425. No licence stated.";
const TYPE_III_ABOUT: &str = "Type III radio bursts found automatically (not checked by hand) in the dynamic spectra by the HELIO Feature Catalogue's RABAT3 code: start and end, the frequency range of the burst's box, peak intensity above background, the code's trust level, and `multiple` when bursts overlap. The catalogue stops at the end of 2011. Converted once from the HFC query service.";
const ALLEN: &str = "Allen et al. (2021), A living catalog of stream interaction regions in the Parker Solar Probe era, A&A 650, A25, https://doi.org/10.1051/0004-6361/202039833 (open access).";
const SODA: &str = "SODA interplanetary shock list, NASA GSFC, https://parker.gsfc.nasa.gov/shocks.html (no licence stated); see Kruparova et al. (2025), ApJL 979, L10, https://doi.org/10.3847/2041-8213/ada558.";
const SODA_ABOUT: &str = "Shocks found by the ShOck Detection Algorithm (SODA) and kept up to date by NASA GSFC: `type` is FF, FR, SF or SR (fast or slow, forward or reverse), the list's quality and `rhs` flags, field and density compression, speed jump, upstream beta, shock angle, Alfvén and magnetosonic Mach numbers, distance in solar radii, HEEQ latitude and longitude, and the driver.";
const SODA_FILE: &str = "https://parker.gsfc.nasa.gov/static_data/soda2/soda.json";
const LASSOS: &str = "LASSOS ICME catalogues, NASA GSFC Heliophysics Science Division, https://science.gsfc.nasa.gov/lassos/ (no licence stated); magnetic obstacle model of Nieves-Chinchilla et al. (2016), ApJ 823, 27, https://doi.org/10.3847/0004-637X/823/1/27.";
const LASSOS_FOLDER: &str = "Literature/Heliosphere/LASSOS ICME catalogues (NASA GSFC)";
const LASSOS_ABOUT: &str = "ICMEs in situ, from the ICME start (its shock when it has one) to its end, kept up to date by the LASSOS working group: magnetic obstacle start and class (Fr flux rope, F- small and F+ large rotation flux rope, Cx complex, E ejecta), its mean field, mean speed and expansion speed (left out when no plasma was measured), distortion parameter, distance in au and whether a shock led it.";

const TUMS_SITE: &str = "https://tums.unam.mx/";
/// The site's "Export table to CSV" button: every row, whatever the page's filters.
const TUMS_EXPORT: &[(&str, &str)] = &[("export_table_to_csv", "Export table to CSV")];
const TUMS_CREDIT: &str = "Acknowledge: \"This paper uses data from the Database of Transient Upstream Mesoscale Structures, generated and maintained by the Universidad Nacional Autónoma de México.\" Also cite the paper in each event's Reference (all at https://tums.unam.mx/Sources). TUMS, https://tums.unam.mx/; no licence stated; its maintainers ask for an electronic copy of each publication that acknowledges it.";

/// One TUMS list: the structures of one type (`*`: the rare ones) seen at one planet.
macro_rules! tums {
    ($planet:literal, $kind:literal, $key:literal, $name:literal, $about:literal) => {
        List {
            key: concat!("tums-", $key),
            folder: concat!("Literature/Upstream structures (TUMS)/", $planet),
            name: concat!($name, " at ", $planet, " (TUMS)"),
            about: concat!(
                $about,
                " From the Database of Transient Upstream Mesoscale Structures (UNAM), which collects them from published lists, read again on each run. ",
                "Each event is the structure's time, as TUMS gives it (no end). `mission` is the spacecraft, `type` TUMS's code for the structure, `reference` and `doi` the paper that listed it, when TUMS names one."
            ),
            credit: TUMS_CREDIT,
            url: TUMS_SITE,
            files: Files::Posted {
                url: TUMS_SITE,
                form: TUMS_EXPORT,
            },
            read: tums,
            part: concat!($planet, ":", $kind),
        }
    };
}

macro_rules! lpp_mms_run {
    ($region:literal) => {
        concat!(
            "Our version, not the authors' catalogue: our own run of the Olshevsky et al. (2021) classifier, made at LPP on 2026-11-16 over all MMS1 FPI fast ion distributions from July 2015 to October 2026, on newer data and libraries than the paper. ",
            "Intervals where MMS1 was in the ", $region, ", as the classifier labels it. ",
            "Run: the authors' code (bitbucket.org/volshevsky/mmslearning, commit 7b93d08) and both published models (trained on 2017-11 and 2017-12), their probabilities averaged; Python 3.11.5, TensorFlow 2.21.0, numpy 2.4.6, pycdfpp 0.20.0; data mms1_fpi_fast_l2_dis-dist, mostly file version v3.4.0. ",
            "Code changes: CDF files read with pycdfpp's cdflib layer instead of cdflib (same arrays); no axis swap, as today's readers already give the order the models expect; no background subtraction, as in the paper's classification. ",
            "Rules: labels smoothed to the most common over 5 samples (about 22 s); a gap over 60 s ends an interval; intervals under 60 s are dropped; only dayside samples (X GSE > 0) are kept, as the models were trained on the dayside and label the tail lobes as solar wind; intervals whose mean probability is under 0.8 are dropped. ",
            "Checked against the human labels of 2017-11 and 2017-12: 97.7% and 97.1% of samples agree. ",
            "`confidence` is the interval's mean probability, `samples` its number of 4.5 s distributions. Converted once from the run's output."
        )
    };
}

static LISTS: [List; 137] = [
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
        key: "mms1-solar-wind-olshevsky-lpp",
        folder: "Literature/Earth/MMS1 regions (Olshevsky classifier, LPP run)",
        name: "MMS1 solar wind, 2015-2026 (LPP run of the Olshevsky et al. 2021 classifier)",
        about: lpp_mms_run!("solar wind"),
        credit: "Classifier and models of Olshevsky et al. (2021), Automated Classification of Plasma Regions Using 3D Particle Energy Distributions, JGR Space Physics 126, e2021JA029620, https://doi.org/10.1029/2021JA029620, code at https://bitbucket.org/volshevsky/mmslearning; run by LPP on MMS FPI data (MMS Science Data Center). Cite the paper and say the list is LPP's run.",
        url: "https://doi.org/10.1029/2021JA029620",
        files: Files::Kept(include_str!(
            "data/olshevsky_2021_mms1_solar_wind_lpp_run.csv"
        )),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms1-foreshock-olshevsky-lpp",
        folder: "Literature/Earth/MMS1 regions (Olshevsky classifier, LPP run)",
        name: "MMS1 ion foreshock, 2015-2026 (LPP run of the Olshevsky et al. 2021 classifier)",
        about: lpp_mms_run!("ion foreshock"),
        credit: "Classifier and models of Olshevsky et al. (2021), Automated Classification of Plasma Regions Using 3D Particle Energy Distributions, JGR Space Physics 126, e2021JA029620, https://doi.org/10.1029/2021JA029620, code at https://bitbucket.org/volshevsky/mmslearning; run by LPP on MMS FPI data (MMS Science Data Center). Cite the paper and say the list is LPP's run.",
        url: "https://doi.org/10.1029/2021JA029620",
        files: Files::Kept(include_str!(
            "data/olshevsky_2021_mms1_foreshock_lpp_run.csv"
        )),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms1-magnetosheath-olshevsky-lpp",
        folder: "Literature/Earth/MMS1 regions (Olshevsky classifier, LPP run)",
        name: "MMS1 magnetosheath, 2015-2026 (LPP run of the Olshevsky et al. 2021 classifier)",
        about: lpp_mms_run!("magnetosheath"),
        credit: "Classifier and models of Olshevsky et al. (2021), Automated Classification of Plasma Regions Using 3D Particle Energy Distributions, JGR Space Physics 126, e2021JA029620, https://doi.org/10.1029/2021JA029620, code at https://bitbucket.org/volshevsky/mmslearning; run by LPP on MMS FPI data (MMS Science Data Center). Cite the paper and say the list is LPP's run.",
        url: "https://doi.org/10.1029/2021JA029620",
        files: Files::Kept(include_str!(
            "data/olshevsky_2021_mms1_magnetosheath_lpp_run.csv"
        )),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms1-magnetosphere-olshevsky-lpp",
        folder: "Literature/Earth/MMS1 regions (Olshevsky classifier, LPP run)",
        name: "MMS1 magnetosphere, 2015-2026 (LPP run of the Olshevsky et al. 2021 classifier)",
        about: lpp_mms_run!("magnetosphere"),
        credit: "Classifier and models of Olshevsky et al. (2021), Automated Classification of Plasma Regions Using 3D Particle Energy Distributions, JGR Space Physics 126, e2021JA029620, https://doi.org/10.1029/2021JA029620, code at https://bitbucket.org/volshevsky/mmslearning; run by LPP on MMS FPI data (MMS Science Data Center). Cite the paper and say the list is LPP's run.",
        url: "https://doi.org/10.1029/2021JA029620",
        files: Files::Kept(include_str!(
            "data/olshevsky_2021_mms1_magnetosphere_lpp_run.csv"
        )),
        read: kept_table,
        part: "",
    },
    List {
        key: "isgi-sudden-commencements",
        folder: "Literature/Earth/ISGI geomagnetic events",
        name: "Sudden commencements, 1869-2026 (ISGI)",
        about: "Sudden commencements: sharp increases of the horizontal geomagnetic field, from a jump in solar wind dynamic pressure, seen at low-latitude observatories. `type` is SSC (followed by a storm) or SI (sudden impulse), blank before it was given; `mean_rise_time_min` and `mean_amplitude_nt` are averaged over the observatories; `quality_codes` are 0-3 per observatory or record collection (3 or 2: unmistakably an SC; 1: possibly; 0: doubtful; * or -: unknown); `observatories` (from 1975) or `record_collections` (N, S, T, before) name the records used. `status` is definitive or provisional (from 2023). Copied once from the ISGI download service on 2026-11-16; provisional events may still change.",
        credit: "Sudden commencements from the Observatori de l'Ebre (Spain), distributed by ISGI, the International Service of Geomagnetic Indices (https://isgi.unistra.fr); licence CC BY-NC 4.0 (non-commercial use). Acknowledge the Ebre Observatory, ISGI and the contributing magnetic observatories.",
        url: "https://isgi.unistra.fr/events_sc.php",
        files: Files::Kept(include_str!("data/isgi_sudden_commencements.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "isgi-solar-flare-effects",
        folder: "Literature/Earth/ISGI geomagnetic events",
        name: "Solar flare effects, 1970-2026 (ISGI)",
        about: "Solar flare effects: sudden geomagnetic perturbations on the dayside from the ionospheric currents a flare enhances. `quality` is 1 (doubtful), 2 (shape and extent of an SFE) or 3 (a notable SFE); `confirmed` says whether ionospheric or solar data confirmed it (given only in some years); `observatories` reported it. `status` is definitive or provisional. Copied once from the ISGI download service on 2026-11-16; provisional events may still change.",
        credit: "Solar flare effects from the Observatori de l'Ebre (Spain), distributed by ISGI, the International Service of Geomagnetic Indices (https://isgi.unistra.fr); licence CC BY-NC 4.0 (non-commercial use). Acknowledge the Ebre Observatory, ISGI and the contributing magnetic observatories.",
        url: "https://isgi.unistra.fr/events_sfe.php",
        files: Files::Kept(include_str!("data/isgi_solar_flare_effects.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "isgi-quiet-days",
        folder: "Literature/Earth/ISGI geomagnetic events",
        name: "International quietest days (Q-days), 1932-2026 (ISGI)",
        about: "The ten quietest UT days of each month, ranked from the Kp index (sum, sum of squares and maximum of the eight daily values). `rank` is 1 (quietest) to 10; `not_really_quiet` says why a selected day is not truly quiet (Ap above 6, or Kp above 3 once or above 2+ twice). The ranking is relative to the month. Copied once from the ISGI download service on 2026-11-16.",
        credit: "International Q-days from GFZ Potsdam (Matzka et al. 2021, Geomagnetic Kp index, V. 1.0, GFZ Data Services, https://doi.org/10.5880/Kp.0001), distributed by ISGI (https://isgi.unistra.fr); licence CC BY 4.0.",
        url: "https://isgi.unistra.fr/events_qdays.php",
        files: Files::Kept(include_str!("data/isgi_quiet_days.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "isgi-disturbed-days",
        folder: "Literature/Earth/ISGI geomagnetic events",
        name: "International most disturbed days (D-days), 1932-2026 (ISGI)",
        about: "The five most disturbed UT days of each month, ranked from the Kp index (sum, sum of squares and maximum of the eight daily values). `rank` is 1 (most disturbed) to 5; `not_really_disturbed` is yes when the day's Ap is under 20. The ranking is relative to the month. Copied once from the ISGI download service on 2026-11-16.",
        credit: "International D-days from GFZ Potsdam (Matzka et al. 2021, Geomagnetic Kp index, V. 1.0, GFZ Data Services, https://doi.org/10.5880/Kp.0001), distributed by ISGI (https://isgi.unistra.fr); licence CC BY 4.0.",
        url: "https://isgi.unistra.fr/events_qdays.php",
        files: Files::Kept(include_str!("data/isgi_disturbed_days.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "isgi-ck-days",
        folder: "Literature/Earth/ISGI geomagnetic events",
        name: "Quietest CK-days, 1868-2024 (ISGI)",
        about: "Quiet UT days from the aa index of two antipodal observatories. `ck24` judges the day itself (mean aa under 13 nT), `ck48` the 48 hours centred on it; each is really_quiet (C) or quiet (K) from the summed aa weights, blank when the day does not qualify. Copied once from the ISGI download service on 2026-11-16.",
        credit: "Quietest CK-days from EOST (Strasbourg, France), distributed by ISGI, the International Service of Geomagnetic Indices (https://isgi.unistra.fr); licence CC BY-NC 4.0 (non-commercial use).",
        url: "https://isgi.unistra.fr/events_ckdays.php",
        files: Files::Kept(include_str!("data/isgi_ck_days.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "cluster-ftes-fear",
        folder: "Literature/Earth",
        name: "Cluster flux transfer events, 2002-2003 (Fear et al. 2012)",
        about: "Flux transfer events seen by Cluster at the magnetopause, used to study how season and IMF clock angle control where FTEs are observed. Each event is the FTE time, to the minute, with the lag applied to ACE data and the lagged IMF in GSM (nT); the IMF is left out where ACE had no data. Converted once from the paper's Data Set S1.",
        credit: "Fear, Palmroth and Milan (2012), Seasonal and clock angle control of the location of flux transfer event signatures at the magnetopause, JGR 117, A04202, https://doi.org/10.1029/2011JA017235, Data Set S1; copyright AGU (no open licence): the event times and values are given here with this citation.",
        url: "https://doi.org/10.1029/2011JA017235",
        files: Files::Kept(include_str!("data/fear_2012_cluster_ftes.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "sector-boundaries-svalgaard",
        folder: "Literature/Heliosphere",
        name: "Interplanetary sector boundaries from ground magnetometers, 1947-1978 (Svalgaard)",
        about: "Crossings of interplanetary magnetic sector boundaries inferred by Svalgaard from polar-cap magnetometer data. A boundary needs the polarity to hold for about 4 days on each side, so these are large-scale, mostly recurrent boundaries. The list has a 1-day resolution: each event is the 24 hours centred on the listed day, within which the crossing most probably lies. `polarity_change` says from which polarity to which (away from or toward the Sun); `bartels_rotation` and `bartels_day` give the first day of the new sector. Converted once from the NOAA NCEI file.",
        credit: "Svalgaard's list of solar sector boundary crossings, from the NOAA National Centers for Environmental Information (https://www.ngdc.noaa.gov/stp/space-weather/interplanetary-data/sector_boundaries/station_data/Stanford/sector.bnd); NOAA data, public domain.",
        url: "https://www.ngdc.noaa.gov/stp/space-weather/interplanetary-data/sector_boundaries/station_data/",
        files: Files::Kept(include_str!("data/svalgaard_sector_boundaries.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms-multiple-xlines-fuselier",
        folder: "Literature/Earth",
        name: "MMS magnetopause crossings with multiple X-lines (Fuselier et al. 2026)",
        about: "Dayside magnetopause crossings by MMS, 2017-2019, used to test whether X-lines form at the maximum magnetic shear location and drift with the magnetosheath flow: IMF Bx/B and clock angle, the distance to the predicted X-line (Earth radii), whether the crossing agrees with that prediction, and whether multiple reconnection shows in the magnetosheath boundary layer (MSBL) and in the low-latitude boundary layer (LLBL). Each event is the crossing time, to the minute. Converted once from Table 1 of the paper.",
        credit: "Fuselier et al. (2026), Multiple Reconnection X-Lines at the Earth's Magnetopause in the Presence of Magnetosheath Flow, JGR Space Physics 131, e2025JA034839, https://doi.org/10.1029/2025JA034839, licence CC-BY-4.0 (Table 1).",
        url: "https://doi.org/10.1029/2025JA034839",
        files: Files::Kept(include_str!("data/fuselier_2026_multiple_xlines.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms-foreshock-bubbles-turner",
        folder: "Literature/Earth",
        name: "MMS foreshock bubbles, 2017-2019 (Turner et al. 2020)",
        about: "Ten foreshock bubbles observed by MMS, with ARTEMIS upstream, used to characterise their structure at ion and electron scales. Each event starts at the bubble time, to the minute, and lasts its duration. Attributes join Tables 1 and 2 of the paper: the solar wind Alfvén and fast Mach numbers and beta, the magnetic field (nT) and velocity (km/s) in GSE downstream and upstream of the IMF discontinuity, the discontinuity normal and how it was found, whether the motional electric field points into the discontinuity on either side, the shock or compression at each side, the upstream boundary normal and speed (km/s), the number of magnetic holes in the core, the expansion speed (km/s), size (Earth radii), age (s) and distance travelled since formation (Earth radii). The last event has no multispacecraft analysis (MMS in a string of pearls): its expansion speed, age and distance are not given. Converted once from Tables 1 and 2 of the paper.",
        credit: "Turner, Liu, Wilson et al. (2020), Microscopic, Multipoint Characterization of Foreshock Bubbles With Magnetospheric Multiscale (MMS), JGR Space Physics 125, e2019JA027707, https://doi.org/10.1029/2019JA027707, licence CC-BY-4.0 (Tables 1 and 2).",
        url: "https://doi.org/10.1029/2019JA027707",
        files: Files::Kept(include_str!("data/turner_2020_foreshock_bubbles.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms-magnetic-holes-in-foreshock-bubbles-turner",
        folder: "Literature/Earth",
        name: "Magnetic holes inside MMS foreshock bubbles (Turner et al. 2020)",
        about: "Twenty-five deep magnetic holes (|B| < 1 nT) inside the foreshock bubbles of Turner et al. (2020), found in the bubble cores: these are not solar wind magnetic holes. Each event starts at the hole time, to 0.1 s, and lasts its crossing time (0.05 to 0.9 s). Attributes: the hole velocity in GSE and speed (km/s) from multispacecraft timing, the crossing time (s) and the size estimate (km). Converted once from Table 3 of the paper.",
        credit: "Turner, Liu, Wilson et al. (2020), Microscopic, Multipoint Characterization of Foreshock Bubbles With Magnetospheric Multiscale (MMS), JGR Space Physics 125, e2019JA027707, https://doi.org/10.1029/2019JA027707, licence CC-BY-4.0 (Table 3).",
        url: "https://doi.org/10.1029/2019JA027707",
        files: Files::Kept(include_str!("data/turner_2020_magnetic_holes.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms-foreshock-bubbles-lee",
        folder: "Literature/Earth",
        name: "MMS foreshock bubbles, 2017-2019 (Lee et al. 2023)",
        about: "Twenty-three foreshock bubbles observed by MMS, used to study how they accelerate ions. Each event is the bubble time, to the minute. Attributes, in the paper's order: the bubble number; the depletions inside the bubble as ratios to the ambient values, B_min/B_0, V_X,min/V_X,0 and n_min/n_0; the heating, T_max/T_0; the increased field and density on the compressional boundary, B_max/B_0 and n_max/n_0; the distance from the model bow shock of Merka et al. (2005), D_bs (Earth radii); the solar wind Alfvén Mach number M_A; and the proton flux enhancement during the bubble, (D - B)/B. Nine of these bubbles are also in the Turner et al. (2020) list. Converted once from Table 1 of the paper.",
        credit: "Lee, Sibeck, Omidi et al. (2023), Ion Acceleration by Foreshock Bubbles: Magnetospheric Multiscale Observations, JGR Space Physics 128, e2023JA031770, https://doi.org/10.1029/2023JA031770, Table 1; copyright AGU (no open licence): the event times and values are given here with this citation.",
        url: "https://doi.org/10.1029/2023JA031770",
        files: Files::Kept(include_str!("data/lee_2023_foreshock_bubbles.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "stereo-stealth-cmes-ma",
        folder: "Literature/Sun",
        name: "Stealth CMEs seen by STEREO, 2009 (Ma et al. 2010)",
        about: "Eleven Earth-sided CMEs of 2009 January to August, seen at the limb by one STEREO spacecraft and on the disk by the other, with no low coronal signature on the disk: stealth CMEs, a third of the 34 Earth-sided CMEs of the period. A low coronal signature (LCS) is a sign of the eruption on the solar disk in STEREO/EUVI 171, 195 or 304 A images: a filament eruption, a sigmoid eruption, a flare, an EIT (coronal) wave, a coronal dimming, or a post-eruptive arcade or brightening. Each event is the CME's first appearance in the COR2 field of view (CACTus time). Attributes: the paper's event number; its notes (two asterisks: the CME speed the authors measured by tracking its core is quite reliable, one asterisk: reliable; c: the CACTus record was corrected, mainly its width; a: added to CACTus by the authors); the CACTus position angle, angular width (deg) and speed (km/s) and the STEREO spacecraft seeing it at the limb (mid: not purely at the limb); and the other spacecraft, seeing it on the disk, with its own CACTus entry when it has one; and the CME's character as seen from each side. Converted once from Table 1 of the paper.",
        credit: "Ma, Attrill, Golub and Lin (2010), Statistical Study of Coronal Mass Ejections With and Without Distinct Low Coronal Signatures, ApJ 722, 289-301, https://doi.org/10.1088/0004-637X/722/1/289, no licence stated (Table 1).",
        url: "https://doi.org/10.1088/0004-637X/722/1/289",
        files: Files::Kept(include_str!("data/ma_2010_stealth_cmes.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "stereo-cmes-with-lcs-ma",
        folder: "Literature/Sun",
        name: "CMEs with low coronal signatures seen by STEREO, 2009 (Ma et al. 2010)",
        about: "Twenty-three Earth-sided CMEs of 2009 January to August, seen at the limb by one STEREO spacecraft and on the disk by the other, whose eruption shows on the disk: the counterpart of the stealth CMEs of the same paper. A low coronal signature (LCS) is a sign of the eruption on the solar disk in STEREO/EUVI 171, 195 or 304 A images: a filament eruption, a sigmoid eruption, a flare, an EIT (coronal) wave, a coronal dimming, or a post-eruptive arcade or brightening. Each event is the CME's first appearance in the COR2 field of view (CACTus time). Attributes: the paper's event number; its notes (two asterisks: the CME speed the authors measured by tracking its core is quite reliable, one asterisk: reliable; c: the CACTus record was corrected, mainly its width; a: added to CACTus by the authors); the CACTus position angle, angular width (deg) and speed (km/s) and the STEREO spacecraft seeing it at the limb (mid: not purely at the limb); and the other spacecraft, seeing it on the disk, with its own CACTus entry when it has one; and the low coronal signatures identified. Converted once from Table 2 of the paper.",
        credit: "Ma, Attrill, Golub and Lin (2010), Statistical Study of Coronal Mass Ejections With and Without Distinct Low Coronal Signatures, ApJ 722, 289-301, https://doi.org/10.1088/0004-637X/722/1/289, no licence stated (Table 2).",
        url: "https://doi.org/10.1088/0004-637X/722/1/289",
        files: Files::Kept(include_str!("data/ma_2010_cmes_with_lcs.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "voyager2-hcs-crossings-choi",
        folder: "Literature/Heliosphere",
        name: "Heliospheric current sheet crossings by Voyager 2, 1977-1990 (Choi and Lee 2025)",
        about: "The 188 crossings of the heliospheric current sheet Voyager 2 observed from 1 to 33.6 au, 1977 to 1990, in 48 s magnetic field data, each fitted with a Harris current sheet model. Each event runs from the crossing's start to its end as the table gives them; for five crossings (14, 23, 43, 48, 52) the table's end comes before its start, so the event runs from the earlier to the later time and `end_before_start` says so. Attributes: the paper's sequence number, the crossing time at the sheet's centre, the Euler angles alpha and beta of the sheet normal relative to the local frame (rad), and the amplitude of the magnetic field change (nT). Converted once from the machine-readable Table 2 of the paper.",
        credit: "Choi and Lee (2025), Heliospheric Current Sheet Crossings Observed by Voyager 2 across 1-33.6 au, ApJ 987, 59, https://doi.org/10.3847/1538-4357/adde44, licence CC-BY-4.0 (Table 2).",
        url: "https://doi.org/10.3847/1538-4357/adde44",
        files: Files::Kept(include_str!("data/choi_2025_voyager2_hcs.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "sbcross-sector-boundary-crossings",
        folder: "Literature/Heliosphere",
        name: "Sector boundary crossings at L1, 1995-2025 (SBCross, Heinemann et al. 2026)",
        about: "The SBCross catalog: 1,076 crossings of heliospheric sector boundaries (the heliospheric current sheet, its plasma sheet and surrounding halo) found automatically near Earth from 1995 to 2025, in Wind/3DP 255 eV suprathermal electron pitch angle distributions and the magnetic field azimuth of hourly OMNI data; the successor of Svalgaard's list. Each event runs from the crossing's start to its end, to the hour. Attributes: the polarity change (+,- or -,+); the duration (h) and thickness (km); the average and spread of the solar wind speed (km/s), density and peak density (cm^-3), magnetic field (nT) and plasma beta; the confidence (high: found in both the electron and the field data; moderate: only in the electron data; low: only in the field data); and whether a CME of the Richardson and Cane list is associated (within 12 h), with `cme_position` before, after or overlapping. Read on each run from the static dataset on Zenodo; https://sbcross.helsinki.fi keeps it up to date but did not answer when this list was made.",
        credit: "Heinemann, Temmer, Owens, Kilpua, Farrugia and Price (2026), Detection and Statistics of Large-scale Sector Boundary Crossings in the Solar Wind: The SBCross Catalog, ApJ 1012, 160, https://doi.org/10.3847/1538-4357/aeb926 (licence CC-BY-4.0); dataset https://doi.org/10.5281/zenodo.22078280, licence CC-BY-4.0.",
        url: "https://doi.org/10.5281/zenodo.22078280",
        files: Files::Remote(&[
            "https://zenodo.org/api/records/22078280/files/event_summary_v3_19082026.csv/content",
        ]),
        read: sbcross,
        part: "",
    },
    List {
        key: "wso-hcs-tilt",
        folder: "Literature/Sun",
        name: "Tilt angle of the heliospheric current sheet, WSO, per Carrington rotation",
        about: "The maximum latitude the computed heliospheric current sheet reaches in each Carrington rotation since 1976, from the Wilcox Solar Observatory, read again on each run. Each event is one rotation, from its start to the next rotation's start (the last one lasts a synodic rotation, 27.2753 days); `carrington_rotation` is its number. Two potential field models: the radial model, with a radial boundary condition at the photosphere and a source surface at 3.25 solar radii, gives r_av_deg, r_n_deg and r_s_deg (the average, northern and southern extent, degrees); the classic model, with a line-of-sight boundary condition and a polar field correction, gives l_av_deg, l_n_deg and l_s_deg, which WSO says probably give the better estimate. WSO warns that the last rotation may rest on incomplete data.",
        credit: "Wilcox Solar Observatory, Computed Tilt Angle of the Heliospheric Current Sheet, http://wso.stanford.edu/Tilts.html (no licence stated); cite Hoeksema (1995), The Large-Scale Structure of the Heliospheric Current Sheet During the Ulysses Epoch, Space Science Reviews 72, 137-148, https://doi.org/10.1007/BF00768770.",
        url: "http://wso.stanford.edu/Tilts.html",
        files: Files::Remote(&["http://wso.stanford.edu/Tilts.html"]),
        read: wso_tilts,
        part: "",
    },
    List {
        key: "eth-type-iv-fine-structures-bernold",
        folder: "Literature/Sun",
        name: "Fine structures in type IV radio bursts, ETH Zurich 1977-1978 (Bernold 1980)",
        about: "Twenty-nine intervals of fine structure in six type IV solar radio bursts, 24 December 1977 to 13 October 1978, from the ETH Zurich digital spectrometer (100-1000 MHz in 1 MHz steps): the most typical ones, not every one recorded. Each event is a selected period of the digital recording, UT to the second. Attributes from Table I: the characteristic as printed; its class in Table II (variation in time, variation in frequency, combinations, diffuse); the frequency band recorded (MHz) and the sweeps per second; the whole burst's start, end and band from the analog recording; and the paper's figure showing it. Table II defines the characteristics: broad band fluctuations (>100 MHz, fluctuations in flux, not periodic), quasi periodic pulsations (>50 MHz, 0.5 to 5 s, almost sinusoidal modulation), sudden reductions (>50 MHz, high emission interrupted by steep absorptions), spikes (>20 MHz, <1 s, a band of enhanced emission), zebra pattern (2-5 MHz, slowly varying bands of enhanced flux), tadpoles (<5 MHz, <1 s, emission with absorption at higher frequencies and a dot of emission in it), fibres (>50 MHz, about 0.1 s, emission followed by absorption, drifting), rain (<10 MHz, <1/8 s, single drops of emission over the burst), clouds (>10 MHz, >1 s, fuzzy structures) and spots (<10 MHz, about 1 s, short intense emissions). Transcribed from the scan twice, independently, and checked against the times on the figures: every row matches its figure but the last clouds (78/10/13), which Table I starts at 12:54:35 and Figure 29 shows from 12:59:35; the table's time is kept. Converted once from Tables I and II of the paper.",
        credit: "Bernold, T. (1980), A catalogue of fine structures in type IV solar radio bursts, Astronomy and Astrophysics Supplement Series 42, 43-58, https://ui.adsabs.harvard.edu/abs/1980A%26AS...42...43B (Tables I and II); scan © European Southern Observatory, provided by the NASA Astrophysics Data System, no licence stated.",
        url: "https://ui.adsabs.harvard.edu/abs/1980A%26AS...42...43B",
        files: Files::Kept(include_str!("data/bernold_1980_type4_fine_structures.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "sdo-cme-directions-direcd",
        folder: "Literature/Sun",
        name: "CME directions from coronal dimmings, Solar Cycle 25 (DIRECD, Jain et al. 2026)",
        about: "Thirty-one coronal mass ejections of Solar Cycle 25 (2021-2026) whose 3D direction the DIRECD method infers from the coronal dimming they leave in SDO/AIA 211 Å images. Each event starts and ends with the dimming's impulsive phase. Attributes join Tables 1 and 3 of the paper by event number: the event number, the heliographic latitude and longitude of the associated flare (deg), the height of the best-fit cone (solar radii), the 3D inclination and its projections onto the meridional and equatorial planes with their errors (deg; positive north and west), whether SDO quick-look data were used, the CME speed (km/s), the angular miss distance between source and deflection plane (deg), and the Dst peak time, observed and predicted Dst (nT). Three events have no speed or Dst. The catalog's 33 earlier events, from Jain et al. (2025), are not in these tables. Converted once from Tables 1 and 3 of the paper.",
        credit: "Jain, Podladchikova, Dissauer, Veronig and Razquin (2026), The DIRECD Coronal Mass Ejection Direction Catalog: Three-dimensional Propagation Inferred from Coronal Dimmings, ApJS 286, 64, https://doi.org/10.3847/1538-4365/ae9d52, licence CC-BY-4.0 (Tables 1 and 3).",
        url: "https://doi.org/10.3847/1538-4365/ae9d52",
        files: Files::Kept(include_str!("data/jain_2026_direcd.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms-turbulence-stawarz",
        folder: "Literature/Earth",
        name: "MMS magnetosheath turbulence intervals (Stawarz et al. 2022)",
        about: "Turbulent magnetosheath intervals in MMS burst data, 2015-2019: position (GSE, km), mean flow and fluctuations, field and fluctuations, density, temperatures, betas, the angle between flow and field, the plasma scales (km) and the magnetic correlation length (in ion inertial lengths). Converted once from the paper's supplementary data.",
        credit: "Stawarz et al. (2022), Turbulence-driven magnetic reconnection and the magnetic correlation length: Observations from Magnetospheric Multiscale, Phys. Plasmas 29, 012302, https://doi.org/10.1063/5.0071106, licence CC-BY-4.0 (supplementary material).",
        url: "https://doi.org/10.1063/5.0071106",
        files: Files::Kept(include_str!("data/stawarz_2022_turbulence_intervals.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms-turbulence-reconnection-stawarz",
        folder: "Literature/Earth",
        name: "MMS reconnecting current sheets in magnetosheath turbulence (Stawarz et al. 2022)",
        about: "Reconnection events found in the turbulence intervals of the same paper: each event spans the four spacecraft's crossings, with each spacecraft's crossing time, peak current, magnetic shear, thickness (electron inertial lengths), electron and ion outflow jumps (in Alfven speed) and inflow Alfven speed, then the LMN frame (GSE) and the mean ion flow over 10 ion inertial lengths. Converted once from the paper's supplementary data.",
        credit: "Stawarz et al. (2022), Turbulence-driven magnetic reconnection and the magnetic correlation length: Observations from Magnetospheric Multiscale, Phys. Plasmas 29, 012302, https://doi.org/10.1063/5.0071106, licence CC-BY-4.0 (supplementary material).",
        url: "https://doi.org/10.1063/5.0071106",
        files: Files::Kept(include_str!("data/stawarz_2022_reconnection_events.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms-bow-shock-current-sheets-gingell",
        folder: "Literature/Earth",
        name: "MMS reconnecting current sheets at the bow shock (Gingell et al. 2020)",
        about: "Reconnecting current sheets in the transition region of Earth's bow shock, seen by MMS: each event is the sheet's time, with its width, current, electron and ion outflows, heating (DT, across the sheet; dT, against the surroundings), the LMN field, inflow and outflow plasma, position (GSE, Earth radii), shock Mach number and angle, and three shock normals. Converted once from the paper's supplementary data set.",
        credit: "Gingell et al. (2020), Statistics of Reconnecting Current Sheets in the Transition Region of Earth's Bow Shock, JGR Space Physics 125, e2019JA027119, https://doi.org/10.1029/2019JA027119, Data Set S1; free to read, copyright AGU (no open licence): the event times and values are given here with this citation.",
        url: "https://doi.org/10.1029/2019JA027119",
        files: Files::Kept(include_str!(
            "data/gingell_2020_reconnecting_current_sheets.csv"
        )),
        read: kept_table,
        part: "",
    },
    List {
        key: "mms-magnetosheath-wang",
        folder: "Literature/Earth",
        name: "MMS turbulent magnetosheath intervals (Wang et al. 2021)",
        about: "Magnetosheath intervals in MMS burst data, 2016-2017, behind a quasi-perpendicular or a quasi-parallel shock, with the mean ion speed, ion beta and electron density, used for a survey of collisionless dissipation. Converted once from Table 1 of the paper.",
        credit: "Wang et al. (2021), Statistical Survey of Collisionless Dissipation in the Terrestrial Magnetosheath, JGR Space Physics 126, e2020JA029000, https://doi.org/10.1029/2020JA029000, Table 1; free to read, copyright AGU (no open licence): the event times and values are given here with this citation.",
        url: "https://doi.org/10.1029/2020JA029000",
        files: Files::Kept(include_str!("data/wang_2021_magnetosheath_intervals.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "sirs-chi",
        folder: "Literature/Heliosphere",
        name: "Stream interaction regions near Earth, 1995-2016 (Chi et al. 2018)",
        about: "Every SIR in Wind and ACE data from 1995 to 2016, extending Jian et al. (2006): start, end and the stream interface (middle), the minimum Dst and its time when the SIR drove a storm, and whether it interacted with an ICME, with the ICME's times. Converted once from the paper's supporting Tables S2 and S3.",
        credit: "Chi et al. (2018), Geoeffectiveness of Stream Interaction Regions From 1995 to 2016, Space Weather 16, 1960-1971, https://doi.org/10.1029/2018SW001894, Tables S2 and S3; free to read, copyright AGU (no open licence): the event times and values are given here with this citation.",
        url: "https://doi.org/10.1029/2018SW001894",
        files: Files::Kept(include_str!("data/chi_2018_sirs.csv")),
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
    List {
        key: "icme-multicatalog",
        folder: "Literature/Heliosphere",
        name: "ICME multi-catalogue, 0.25-5 AU (Larrodera & Temmer)",
        about: "About 2000 ICMEs from 1975 to 2022 seen by 13 spacecraft, gathered from 12 published catalogues: the event runs from the ICME's start to the end of its magnetic obstacle, `mo_start` is where the obstacle begins (the same as the start when there is no sheath), `source` names the catalogue the dates come from, with the spacecraft's distance (km) and HCI longitude and latitude. Only the dates come from the source catalogues; the file's 150 columns of plasma averages are left out.",
        credit: "Larrodera, C. & Temmer, M., Interplanetary Coronal Mass ejections multi-catalog, e-cienciaDatos, https://doi.org/10.21950/XGUIYX, licence CC-BY-NC-4.0.",
        url: "https://doi.org/10.21950/XGUIYX",
        files: Files::Remote(&["https://edatos.consorciomadrono.es/api/access/datafile/50182"]),
        read: icme_multicatalog,
        part: "",
    },
    List {
        key: "llamaice",
        folder: LLAMAICE_FOLDER,
        name: "Near-Earth CME candidates, ACE and Wind 1995-2006 (LLAMAICE)",
        about: "Every CME candidate the published near-Earth catalogues list from February 1995 to December 2006, as the LLAMAICE team reviewed it, with the team's `comment` starting with its confidence that this is a CME (Y yes, PY probably yes, M maybe, PN probably not, N no). Where the team set boundaries (`bounds_from` LLAMAICE), the event runs from the first to the last of, in time order, `sheath_start`, `front_mixed_start` (a region mixing sheath and ejecta signatures), `ejecta_start`, `ejecta_end` and `back_mixed_end` (a mixed region behind the ejecta). Where it did not, mostly candidates it doubts (`bounds_from` catalogues), the event spans what the catalogues said. `catalogues` names the catalogues that list it, each with its own list here; `interacts_with` is the LLAMAICE CME it interacts with and `hss` the high-speed stream catalogues it matches.",
        credit: LLAMAICE_CREDIT,
        url: "https://osprei.space/llamaice",
        files: Files::Together(&[
            "https://osprei.space/llamadata/LLAMAICE_1.0.csv",
            LLAMAICE_SOURCES,
        ]),
        read: llamaice,
        part: "",
    },
    List {
        key: "llamaice-richardson-cane",
        folder: LLAMAICE_FOLDER,
        name: "Richardson & Cane ICMEs (LLAMAICE)",
        about: "The Richardson & Cane list of near-Earth ICMEs (Richardson & Cane 2010, https://doi.org/10.1007/s11207-010-9568-6), as collated by LLAMAICE for ACE and Wind, February 1995 to December 2006. The event runs from the sheath start (the ejecta start when the catalogue gives no sheath) to the ejecta end; `llamaice_event` is the LLAMAICE CME it was matched to, so the same CME can be compared across catalogues, and `cme_time` the time of the CME at the Sun when the catalogue gives one.",
        credit: LLAMAICE_CREDIT,
        url: "https://osprei.space/llamaice",
        files: Files::Remote(&[LLAMAICE_SOURCES]),
        read: llamaice_source,
        part: "RC",
    },
    List {
        key: "llamaice-jian",
        folder: LLAMAICE_FOLDER,
        name: "Jian ICMEs (LLAMAICE)",
        about: "Lan Jian's list of ICMEs at ACE and Wind (Jian et al. 2006, https://doi.org/10.1007/s11207-006-0133-2), as collated by LLAMAICE for ACE and Wind, February 1995 to December 2006. The event runs from the sheath start (the ejecta start when the catalogue gives no sheath) to the ejecta end; `llamaice_event` is the LLAMAICE CME it was matched to, so the same CME can be compared across catalogues, and `cme_time` the time of the CME at the Sun when the catalogue gives one.",
        credit: LLAMAICE_CREDIT,
        url: "https://osprei.space/llamaice",
        files: Files::Remote(&[LLAMAICE_SOURCES]),
        read: llamaice_source,
        part: "Jian",
    },
    List {
        key: "llamaice-lepping",
        folder: LLAMAICE_FOLDER,
        name: "Lepping magnetic clouds (LLAMAICE)",
        about: "Lepping's list of Wind magnetic clouds (Lepping et al. 2006, https://doi.org/10.5194/angeo-24-215-2006), as collated by LLAMAICE for ACE and Wind, February 1995 to December 2006. The event runs from the sheath start (the ejecta start when the catalogue gives no sheath) to the ejecta end; `llamaice_event` is the LLAMAICE CME it was matched to, so the same CME can be compared across catalogues, and `cme_time` the time of the CME at the Sun when the catalogue gives one.",
        credit: LLAMAICE_CREDIT,
        url: "https://osprei.space/llamaice",
        files: Files::Remote(&[LLAMAICE_SOURCES]),
        read: llamaice_source,
        part: "Lepping",
    },
    List {
        key: "llamaice-wind",
        folder: LLAMAICE_FOLDER,
        name: "Wind ICME catalogue (LLAMAICE)",
        about: "The Wind ICME catalogue (Nieves-Chinchilla et al. 2018, https://doi.org/10.1007/s11207-018-1247-z), as collated by LLAMAICE for ACE and Wind, February 1995 to December 2006. The event runs from the sheath start (the ejecta start when the catalogue gives no sheath) to the ejecta end; `llamaice_event` is the LLAMAICE CME it was matched to, so the same CME can be compared across catalogues, and `cme_time` the time of the CME at the Sun when the catalogue gives one.",
        credit: LLAMAICE_CREDIT,
        url: "https://osprei.space/llamaice",
        files: Files::Remote(&[LLAMAICE_SOURCES]),
        read: llamaice_source,
        part: "Wind",
    },
    List {
        key: "llamaice-cdaw",
        folder: LLAMAICE_FOLDER,
        name: "CDAW flux rope ICMEs (LLAMAICE)",
        about: "The ICMEs of the CDAW flux rope campaign, as collated by LLAMAICE for ACE and Wind, February 1995 to December 2006. The event runs from the sheath start (the ejecta start when the catalogue gives no sheath) to the ejecta end; `llamaice_event` is the LLAMAICE CME it was matched to, so the same CME can be compared across catalogues, and `cme_time` the time of the CME at the Sun when the catalogue gives one.",
        credit: LLAMAICE_CREDIT,
        url: "https://osprei.space/llamaice",
        files: Files::Remote(&[LLAMAICE_SOURCES]),
        read: llamaice_source,
        part: "CDAW",
    },
    List {
        key: "llamaice-dreams",
        folder: LLAMAICE_FOLDER,
        name: "DREAMS ICMEs (LLAMAICE)",
        about: "The ICMEs of the catalogue LLAMAICE calls DREAMS, as collated by LLAMAICE for ACE and Wind, February 1995 to December 2006. The event runs from the sheath start (the ejecta start when the catalogue gives no sheath) to the ejecta end; `llamaice_event` is the LLAMAICE CME it was matched to, so the same CME can be compared across catalogues, and `cme_time` the time of the CME at the Sun when the catalogue gives one.",
        credit: LLAMAICE_CREDIT,
        url: "https://osprei.space/llamaice",
        files: Files::Remote(&[LLAMAICE_SOURCES]),
        read: llamaice_source,
        part: "DREAMS",
    },
    List {
        key: "intense-storms-ji",
        folder: "Literature/Earth",
        name: "Intense geomagnetic storms 1998-2006 (Ji et al. 2012)",
        about: "The 63 storms with Dst at or below -100 nT from 1998 to 2006 used to compare Dst forecast models: `dst_min_nt` is the storm's lowest Dst and `structure` the interplanetary driver (sMC magnetic cloud, SH sheath, nonMC ICME without a cloud, CIR).",
        credit: "Ji, E.-Y., Moon, Y.-J., Gopalswamy, N. & Lee, D.-H. (2012), J. Geophys. Res. 117(A3), https://doi.org/10.1029/2011JA016872 (table sent by Vincent Génot).",
        url: "https://doi.org/10.1029/2011JA016872",
        files: Files::Kept(include_str!("data/ji_2012_intense_storms.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "solo-mirror-mode-storms",
        folder: "Literature/Heliosphere",
        name: "Mirror mode storms at Solar Orbiter (Dimmock et al. 2022)",
        about: "25 trains of mirror mode structures Solar Orbiter saw from April 2020 to August 2021, each at the time it starts: `mm_type` 1 for short bursts of peaks or dips, 2 for extended trains of magnetic holes; the ion speed, density, temperature, anisotropy and beta are means over the event with their standard deviations, `structure` is where it sat (SW-CS solar wind current sheet, SBC sector boundary crossing, SIR), `pas_quality` the PAS quality factor (lower is better).",
        credit: "Dimmock, A. P. et al. (2022), J. Geophys. Res. 127, e2022JA030754, https://doi.org/10.1029/2022JA030754, licence CC-BY-4.0 (Table 1).",
        url: "https://doi.org/10.1029/2022JA030754",
        files: Files::Kept(include_str!("data/dimmock_2022_mirror_mode_storms.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "psp-hcs-crossings",
        folder: "Literature/Heliosphere/PSP heliospheric current sheet (Fargette)",
        name: "Heliospheric current sheet crossings by PSP below 50 Rs (Fargette et al. 2026)",
        about: "The 39 full crossings of the heliospheric current sheet by Parker Solar Probe in encounters 6 to 21 while below 50 solar radii: `jet` is the reconnection outflow seen (outward or inward), `side_jet` marks a smaller jet also seen at a B_R reversal beside the crossing.",
        credit: "Fargette et al. (2026), ApJ 997, 174, https://doi.org/10.3847/1538-4357/ae2601, licence CC-BY-4.0. (Table 1)",
        url: "https://doi.org/10.3847/1538-4357/ae2601",
        files: Files::Kept(include_str!("data/fargette_2026_hcs_crossings.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "psp-hcs-partial-crossings",
        folder: "Literature/Heliosphere/PSP heliospheric current sheet (Fargette)",
        name: "Partial heliospheric current sheet crossings by PSP (Fargette et al. 2026)",
        about: "29 partial crossings of the heliospheric current sheet by Parker Solar Probe in encounters 6 to 21 while below 50 solar radii.",
        credit: "Fargette et al. (2026), ApJ 997, 174, https://doi.org/10.3847/1538-4357/ae2601, licence CC-BY-4.0. (Table 2)",
        url: "https://doi.org/10.3847/1538-4357/ae2601",
        files: Files::Kept(include_str!("data/fargette_2026_hcs_partial_crossings.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "psp-hcs-side-jets",
        folder: "Literature/Heliosphere/PSP heliospheric current sheet (Fargette)",
        name: "Small ion jets beside heliospheric current sheet crossings (Fargette et al. 2026)",
        about: "4 small ion jets Parker Solar Probe saw at B_R reversals on the side of a heliospheric current sheet crossing; `jet` is outward or inward.",
        credit: "Fargette et al. (2026), ApJ 997, 174, https://doi.org/10.3847/1538-4357/ae2601, licence CC-BY-4.0. (Table 3)",
        url: "https://doi.org/10.3847/1538-4357/ae2601",
        files: Files::Kept(include_str!("data/fargette_2026_hcs_side_jets.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "solo-reconnection-exhausts",
        folder: "Literature/Heliosphere",
        name: "Reconnection exhausts in the solar wind, Solar Orbiter 2020 (Fargette et al. 2023)",
        about: "146 magnetic reconnection exhausts (jets) Solar Orbiter saw between 0.6 and 0.8 AU from 14 July to 13 August 2020, from PAS and MAG, with start and end times refined by eye: `number` is the row in the paper's Table D.1, `found_by` says whether the automated detection found it or only visual inspection did (13 exhausts; the table's note says nine, the text and the bold rows say 13).",
        credit: "Fargette, N., Lavraud, B., Rouillard, A. P. et al. (2023), Clustering of magnetic reconnection exhausts in the solar wind: An automated detection study, A&A 674, A98, https://doi.org/10.1051/0004-6361/202346043, licence CC-BY-4.0 (Table D.1).",
        url: "https://doi.org/10.1051/0004-6361/202346043",
        files: Files::Kept(include_str!("data/fargette_2023_reconnection_exhausts.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "cassini-long-standing-reconnection",
        folder: "Literature/Saturn",
        name: "Long-standing small-scale reconnection at Saturn, Cassini (Guo et al. 2019)",
        about: "17 intervals in which Cassini met several small-scale magnetic reconnection sites in a row in Saturn's magnetodisk, 2005-2010. The paper gives the start to the hour and the duration to about an hour: the stop is the start plus that duration. `reconnection_sites` is how many sites were met, `separation_*_h` the mean, shortest and longest time between them, then the local time (hours) and distance to Saturn (Rs) at start and end, whether accelerated particles were seen, and the `sector` the paper files it under (noon, dusk, night, dawn, or periodical for the ones that repeat).",
        credit: "Guo, R. L., Yao, Z. H., Sergis, N. et al. (2019), Long-standing Small-scale Reconnection Processes at Saturn Revealed by Cassini, ApJL 884, L14, https://doi.org/10.3847/2041-8213/ab4429, licence CC-BY-3.0 (Table 1).",
        url: "https://doi.org/10.3847/2041-8213/ab4429",
        files: Files::Kept(include_str!("data/guo_2019_saturn_reconnection.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "image-substorm-onsets-frey",
        folder: "Literature/Earth",
        name: "Substorm onsets seen by IMAGE-FUV, 2000-2002 (Frey et al. 2004)",
        about: "Northern auroral substorm onsets picked in IMAGE-FUV images, May 2000 to December 2002: the imager used (WIC, or S13 for SI-13), the brightest pixel of the onset surge (image pixel and raw counts), IMAGE's geocentric distance, and the onset's geographic and geomagnetic position and magnetic local time. Converted once from the paper's supplementary table.",
        credit: "Frey, Mende, Angelopoulos & Donovan (2004), Substorm onset observations by IMAGE-FUV, JGR 109, A10304, https://doi.org/10.1029/2004JA010607, supplementary table 1; copyright AGU (no open licence): the onset times and positions are given here with this citation.",
        url: "https://doi.org/10.1029/2004JA010607",
        files: Files::Kept(include_str!("data/frey_2004_image_substorm_onsets.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "venus-bow-shock-persson",
        folder: "Literature/Venus",
        name: "Venus bow shock crossings, Venus Express (Persson et al. 2023)",
        about: "Bow shock crossings of Venus by Venus Express, 2006-2014, picked by hand in ASPERA-4 and MAG data: orbit, inbound or outbound, and position in VSO (Venus radii). Converted once from the Zenodo file.",
        credit: "Persson, Bergman, Signoles, Wolff, Martinez, Lindwall & Futaana (2023), Venusian bow shock crossings manually identified from measurements by the ASPERA-4 and MAG instruments onboard Venus Express, Zenodo, https://doi.org/10.5281/zenodo.7679678, licence CC-BY-4.0.",
        url: "https://doi.org/10.5281/zenodo.7679678",
        files: Files::Kept(include_str!("data/persson_2023_venus_bow_shock.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "venus-ion-composition-boundary-persson",
        folder: "Literature/Venus",
        name: "Venus ion composition boundary crossings, Venus Express (Persson et al. 2023)",
        about: "Crossings of the ion composition boundary of Venus (the induced magnetosphere's inner edge) by Venus Express, 2006-2014, picked by hand in ASPERA-4 and MAG data: orbit, inbound or outbound, and position in VSO (Venus radii). Converted once from the Zenodo file.",
        credit: "Persson, Bergman, Signoles, Wolff, Martinez, Lindwall & Futaana (2023), Venusian bow shock crossings manually identified from measurements by the ASPERA-4 and MAG instruments onboard Venus Express, Zenodo, https://doi.org/10.5281/zenodo.7679678, licence CC-BY-4.0.",
        url: "https://doi.org/10.5281/zenodo.7679678",
        files: Files::Kept(include_str!(
            "data/persson_2023_venus_ion_composition_boundary.csv"
        )),
        read: kept_table,
        part: "",
    },
    List {
        key: "mars-bow-shock-simon-wedlund",
        folder: "Literature/Mars",
        name: "Mars bow shock crossings, MAVEN, predicted (Simon Wedlund et al. 2022)",
        about: "Predicted, not picked by hand: bow shock crossings of Mars by MAVEN, 2014-2024, from a 3D shock model corrected on the magnetic field (predictor-corrector). The authors advise using them as a first indicator of the shock, with caution. `direction` inbound is from the solar wind into the sheath; position in MSO (Mars radii), solar zenith angle, shock angle and solar longitude Ls. Converted once from the Zenodo file.",
        credit: "Simon Wedlund, Volwerk, Beth, Mazelle, Möstl, Halekas, Gruesbeck & Rojas-Castillo (2025), Predicted times, spatial coordinates of bow shock crossings and shock geometry at Mars from the NASA/MAVEN mission, Zenodo, https://doi.org/10.5281/zenodo.15056494, licence CC-BY-4.0; method in Simon Wedlund et al. (2022), JGR Space Physics 127, https://doi.org/10.1029/2021JA029942.",
        url: "https://doi.org/10.5281/zenodo.15056494",
        files: Files::Kept(include_str!("data/simon_wedlund_2022_mars_bow_shock.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "themis-magnetosheath-koller",
        folder: "Literature/Earth/THEMIS magnetosheath and jets (Koller)",
        name: "THEMIS subsolar magnetosheath intervals, 2008-2023 (Koller et al. 2025)",
        about: "Intervals when one of the five THEMIS probes was in the subsolar magnetosheath, 2008-2023, from ESA and FGM data at spin resolution (3 s), selected as in Plaschke et al. (2013). The jet lists of this folder are found inside these intervals. Converted once from the Zenodo file.",
        credit: KOLLER,
        url: "https://doi.org/10.5281/zenodo.13843752",
        files: Files::Kept(include_str!("data/koller_2025_themis_magnetosheath.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "themis-jets-plaschke",
        folder: "Literature/Earth/THEMIS magnetosheath and jets (Koller)",
        name: "THEMIS magnetosheath jets, Plaschke criterion, 2008-2023 (Koller et al. 2025)",
        about: "High-speed jets over 10 s in the THEMIS subsolar magnetosheath intervals, 2008-2023, by the Plaschke et al. (2013) criterion (anti-sunward dynamic pressure against the solar wind's). `inner_time` is a third time the file gives between start and end without naming it. Converted once from the Zenodo file.",
        credit: KOLLER,
        url: "https://doi.org/10.5281/zenodo.13843752",
        files: Files::Kept(include_str!("data/koller_2025_themis_jets_plaschke.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "themis-jets-archer",
        folder: "Literature/Earth/THEMIS magnetosheath and jets (Koller)",
        name: "THEMIS magnetosheath jets, Archer criterion, 2008-2023 (Koller et al. 2025)",
        about: "High-speed jets over 10 s in the THEMIS subsolar magnetosheath intervals, 2008-2023, by the Archer et al. (2013) criterion (dynamic pressure against its 20-minute running mean). `inner_time` is a third time the file gives between start and end without naming it. Converted once from the Zenodo file.",
        credit: KOLLER,
        url: "https://doi.org/10.5281/zenodo.13843752",
        files: Files::Kept(include_str!("data/koller_2025_themis_jets_archer.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "themis-jets-koller",
        folder: "Literature/Earth/THEMIS magnetosheath and jets (Koller)",
        name: "THEMIS magnetosheath jets, Koller criterion, 2008-2023 (Koller et al. 2025)",
        about: "High-speed jets over 10 s in the THEMIS subsolar magnetosheath intervals, 2008-2023, by the Koller et al. (2022) criterion; not used in the 2025 paper. `inner_time` is a third time the file gives between start and end without naming it. Converted once from the Zenodo file.",
        credit: KOLLER,
        url: "https://doi.org/10.5281/zenodo.13843752",
        files: Files::Kept(include_str!("data/koller_2025_themis_jets_koller.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "omni-sir-hss-koller",
        folder: "Literature/Heliosphere",
        name: "Stream interaction regions and high-speed streams at L1, 1995-2023 (Koller et al. 2025)",
        about: "SIRs with their high-speed streams in OMNI data, 1995-2023: from the start of the pressure rise to the end of the fast stream, with the time of peak speed and the list each event comes from (Grandin et al. 2019, then the authors). Limited by OMNI coverage; the authors do not claim completeness. Converted once from the Zenodo file.",
        credit: KOLLER,
        url: "https://doi.org/10.5281/zenodo.13843752",
        files: Files::Kept(include_str!("data/koller_2025_omni_sir_hss.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "psp-switchbacks-martinovic",
        folder: "Literature/Heliosphere",
        name: "Switchbacks at Parker Solar Probe, encounters 1 and 2 (Martinović et al. 2021)",
        about: "1,074 switchbacks picked by eye in Parker Solar Probe encounters 1 and 2: the event is the switchback itself (stable reversed field); `sampled_from` and `sampled_to` bound the quiet and transition regions sampled around it. `quality` runs 0-5; under 2 the authors deem the event unreliable. Converted once from the Zenodo HDF5 file.",
        credit: "Martinović, Klein, Huang, Chandran, Kasper, Lichko, Bowen, Chen, Matteini, Stevens, Case & Bale (2021), Parker Solar Probe E1,2 Switchback Database, Zenodo, https://doi.org/10.5281/zenodo.4579821, licence CC-BY-4.0; see Martinović et al. (2021), ApJ 912, https://doi.org/10.3847/1538-4357/abebe5.",
        url: "https://doi.org/10.5281/zenodo.4579821",
        files: Files::Kept(include_str!("data/martinovic_2021_psp_switchbacks.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "sep-30mev-papaioannou",
        folder: "Literature/Heliosphere",
        name: "SEP events above 30 MeV, 1956-2023 (Papaioannou)",
        about: "Solar energetic particle events with protons above 30 MeV, 1956-2023, by day, with their omnidirectional fluence; `source` says which compilation an event comes from. The authors made no cross-calibration between sources. Each event is its whole day. Converted once from the Zenodo sheet.",
        credit: "Papaioannou (2025), A catalog of E>30 MeV Solar Energetic Particle (SEP) events from 1956 to 2023, Zenodo, https://doi.org/10.5281/zenodo.15083732, licence CC-BY-4.0; compiled from Shea & Smart (1990), GSEP (Rotti et al. 2022) and Papaioannou et al. (2023).",
        url: "https://doi.org/10.5281/zenodo.15083732",
        files: Files::Kept(include_str!("data/papaioannou_2025_sep_30mev.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "gsep-rotti",
        folder: "Literature/Heliosphere",
        name: "GOES solar energetic particle events, 1986-2017 (GSEP, Rotti et al. 2022)",
        about: "Proton events in GOES data, 1986-2017, over the time slice the authors analysed, with the flare (class, position, region) and CME they associate, peak fluxes and fluences above 10 to 100 MeV, and `above_swpc_threshold` (1 for the 244 events over NOAA's 10 pfu at 10 MeV). Converted once from the Dataverse table (version 5).",
        credit: "Rotti, Aydin, Georgoulis & Martens (2022), Integrated Geostationary Solar Energetic Particle Events Catalog: GSEP, ApJS, https://doi.org/10.3847/1538-4365/ac87ac; data at Harvard Dataverse, https://doi.org/10.7910/DVN/DZYLHK, licence CC0-1.0.",
        url: "https://doi.org/10.7910/DVN/DZYLHK",
        files: Files::Kept(include_str!("data/rotti_2022_gsep.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "mercury-icmes-winslow",
        folder: "Literature/Mercury",
        name: "ICMEs at Mercury, MESSENGER (Winslow et al. 2015)",
        about: "ICMEs seen by MESSENGER in orbit around Mercury, 2011-2014: from the shock (or the ejecta when no shock) to the ICME's end, with the ejecta start, the CME launch and coronal speed (`source_uncertain` when the authors marked the source with a question mark), transit speeds, Mercury's distance and field strengths. Converted once from the SPDF spreadsheet.",
        credit: "Winslow et al. (2015), Interplanetary coronal mass ejections from MESSENGER orbital observations at Mercury, JGR Space Physics 120, https://doi.org/10.1002/2015JA021200; spreadsheet at NASA SPDF, https://spdf.gsfc.nasa.gov/pub/data/messenger/ICME_Catalog_at_Mercury.xls (NASA data, no licence stated).",
        url: "https://doi.org/10.1002/2015JA021200",
        files: Files::Kept(include_str!("data/winslow_2015_mercury_icmes.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "rbspb-emic-inglis",
        folder: "Literature/Earth",
        name: "EMIC waves, Van Allen Probe B, 2013-2019 (Inglis et al. 2024)",
        about: "Electromagnetic ion cyclotron waves detected automatically in Van Allen Probe B magnetometer data, 2013 to July 2019: band (H+, He+, O+), mean frequency, L and MLT at start and end. Times are the file's UT columns: its unix columns run 5 h ahead of them. Converted once from the Zenodo file.",
        credit: "Inglis, Murphy & Halford (2024), A database of detected EMIC wave activity using Van Allen Probe B magnetometer data, Zenodo, https://doi.org/10.5281/zenodo.12548567, licence CC-BY-4.0; see Inglis et al. (2024), JGR Space Physics, https://doi.org/10.1029/2024JA032666.",
        url: "https://doi.org/10.5281/zenodo.12548567",
        files: Files::Kept(include_str!("data/inglis_2024_rbspb_emic.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "rbsp-emic-remya",
        folder: "Literature/Earth",
        name: "EMIC waves, Van Allen Probes A and B, 2012-2019 (Remya et al. 2023)",
        about: "Electromagnetic ion cyclotron waves found by an automated algorithm (after Bortnik et al. 2007) in Van Allen Probes A and B data, September 2012 to October 2019: probe, band(s), mean field, frequency range, wave power, mean L, MLT and MLAT, and mean AE and Sym-H. Converted once from the Zenodo file.",
        credit: "Remya (2024), EMIC Wave Event List from Van Allen Probes, Zenodo, https://doi.org/10.5281/zenodo.14195258, licence CC-BY-4.0 (the author asks users to get in touch); see Remya et al. (2023), JGR Space Physics 128, https://doi.org/10.1029/2023JA031712.",
        url: "https://doi.org/10.5281/zenodo.14195258",
        files: Files::Kept(include_str!("data/remya_2024_rbsp_emic.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "type-iii-wind-hfc",
        folder: "Literature/Sun/Type III radio bursts (HELIO HFC)",
        name: "Type III radio bursts, Wind/WAVES RAD2, 1996-2011 (HELIO HFC)",
        about: TYPE_III_ABOUT,
        credit: TYPE_III,
        url: "https://voparis-helio.obspm.fr/hfc-gui/",
        files: Files::Kept(include_str!("data/hfc_type_iii_wind.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "type-iii-stereo-a-hfc",
        folder: "Literature/Sun/Type III radio bursts (HELIO HFC)",
        name: "Type III radio bursts, STEREO-A/WAVES HFR, 2007-2011 (HELIO HFC)",
        about: TYPE_III_ABOUT,
        credit: TYPE_III,
        url: "https://voparis-helio.obspm.fr/hfc-gui/",
        files: Files::Kept(include_str!("data/hfc_type_iii_stereo_a.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "type-iii-stereo-b-hfc",
        folder: "Literature/Sun/Type III radio bursts (HELIO HFC)",
        name: "Type III radio bursts, STEREO-B/WAVES HFR, 2007-2011 (HELIO HFC)",
        about: TYPE_III_ABOUT,
        credit: TYPE_III,
        url: "https://voparis-helio.obspm.fr/hfc-gui/",
        files: Files::Kept(include_str!("data/hfc_type_iii_stereo_b.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "psp-sirs-allen",
        folder: "Literature/Heliosphere/PSP-era SIRs (Allen et al. 2021)",
        name: "Stream interaction regions at Parker Solar Probe, orbits 1-5 (Allen et al. 2021)",
        about: "SIRs and CIRs seen by Parker Solar Probe in orbits 1 to 5, 2018-2020: time of peak pressure (the interface), distance, peak pressure, field and density, speed range, whether STEREO-A and L1 saw it too, whether it is a CIR (seen a rotation earlier), had a shock, or sat near a current sheet crossing, and the ambiguous flag (0 clean). Converted once from the paper's Table 1; the living version the paper announces is no longer online.",
        credit: ALLEN,
        url: "https://doi.org/10.1051/0004-6361/202039833",
        files: Files::Kept(include_str!("data/allen_2021_psp_sirs.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "stereo-a-sirs-allen",
        folder: "Literature/Heliosphere/PSP-era SIRs (Allen et al. 2021)",
        name: "Stream interaction regions at STEREO-A matching PSP's, 2018-2019 (Allen et al. 2021)",
        about: "The SIRs and CIRs STEREO-A saw that match Parker Solar Probe's (`psp_events` gives their numbers in the PSP list), with the same quantities. Converted once from the paper's Table 2.",
        credit: ALLEN,
        url: "https://doi.org/10.1051/0004-6361/202039833",
        files: Files::Kept(include_str!("data/allen_2021_stereo_a_sirs.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "l1-sirs-allen",
        folder: "Literature/Heliosphere/PSP-era SIRs (Allen et al. 2021)",
        name: "Stream interaction regions at L1 matching PSP's, 2018-2019 (Allen et al. 2021)",
        about: "The SIRs and CIRs ACE and Wind saw at L1 that match Parker Solar Probe's (`psp_events` gives their numbers in the PSP list), with the same quantities. Converted once from the paper's Table 3.",
        credit: ALLEN,
        url: "https://doi.org/10.1051/0004-6361/202039833",
        files: Files::Kept(include_str!("data/allen_2021_l1_sirs.csv")),
        read: kept_table,
        part: "",
    },
    List {
        key: "soda-shocks-psp",
        folder: "Literature/Heliosphere/SODA interplanetary shocks",
        name: "Interplanetary shocks at Parker Solar Probe (SODA)",
        about: SODA_ABOUT,
        credit: SODA,
        url: "https://parker.gsfc.nasa.gov/shocks.html",
        files: Files::Remote(&[SODA_FILE]),
        read: soda_shocks,
        part: "PSP",
    },
    List {
        key: "soda-shocks-solar-orbiter",
        folder: "Literature/Heliosphere/SODA interplanetary shocks",
        name: "Interplanetary shocks at Solar Orbiter (SODA)",
        about: SODA_ABOUT,
        credit: SODA,
        url: "https://parker.gsfc.nasa.gov/shocks.html",
        files: Files::Remote(&[SODA_FILE]),
        read: soda_shocks,
        part: "SLO",
    },
    List {
        key: "soda-shocks-stereo-a",
        folder: "Literature/Heliosphere/SODA interplanetary shocks",
        name: "Interplanetary shocks at STEREO-A (SODA)",
        about: SODA_ABOUT,
        credit: SODA,
        url: "https://parker.gsfc.nasa.gov/shocks.html",
        files: Files::Remote(&[SODA_FILE]),
        read: soda_shocks,
        part: "STA",
    },
    List {
        key: "lassos-icmes-helios",
        folder: LASSOS_FOLDER,
        name: "ICMEs at Helios 1 and 2, 1975-1980 (LASSOS)",
        about: LASSOS_ABOUT,
        credit: LASSOS,
        url: "https://science.gsfc.nasa.gov/lassos/ICME_catalogs/helios-catalog.shtml",
        files: Files::Remote(&[
            "https://science.gsfc.nasa.gov/lassos/assets/catalogs/ICMEcatalog_Helios_web.json",
        ]),
        read: lassos_icmes,
        part: "",
    },
    List {
        key: "lassos-icmes-messenger",
        folder: LASSOS_FOLDER,
        name: "ICMEs at MESSENGER, 2007-2015 (LASSOS)",
        about: LASSOS_ABOUT,
        credit: LASSOS,
        url: "https://science.gsfc.nasa.gov/lassos/ICME_catalogs/messenger-catalog.shtml",
        files: Files::Remote(&[
            "https://science.gsfc.nasa.gov/lassos/assets/catalogs/ICMEcatalog_MESSENGER_web.json",
        ]),
        read: lassos_icmes,
        part: "",
    },
    List {
        key: "lassos-icmes-psp",
        folder: LASSOS_FOLDER,
        name: "ICMEs at Parker Solar Probe (LASSOS)",
        about: LASSOS_ABOUT,
        credit: LASSOS,
        url: "https://science.gsfc.nasa.gov/lassos/ICME_catalogs/parker-catalog.shtml",
        files: Files::Remote(&[
            "https://science.gsfc.nasa.gov/lassos/assets/catalogs/ICMEcatalog_Parker_web.json",
        ]),
        read: lassos_icmes,
        part: "",
    },
    List {
        key: "lassos-icmes-solar-orbiter",
        folder: LASSOS_FOLDER,
        name: "ICMEs at Solar Orbiter (LASSOS)",
        about: LASSOS_ABOUT,
        credit: LASSOS,
        url: "https://science.gsfc.nasa.gov/lassos/ICME_catalogs/solo-catalog.shtml",
        files: Files::Remote(&[
            "https://science.gsfc.nasa.gov/lassos/assets/catalogs/ICMEcatalog_Orbiter_web.json",
        ]),
        read: lassos_icmes,
        part: "",
    },
    List {
        key: "lassos-icmes-stereo",
        folder: LASSOS_FOLDER,
        name: "ICMEs at STEREO A and B (LASSOS)",
        about: LASSOS_ABOUT,
        credit: LASSOS,
        url: "https://science.gsfc.nasa.gov/lassos/ICME_catalogs/stereo-catalog.shtml",
        files: Files::Remote(&[
            "https://science.gsfc.nasa.gov/lassos/assets/catalogs/ICMEcatalog_STEREO_web.json",
        ]),
        read: lassos_icmes,
        part: "",
    },
    tums!(
        "Earth",
        "SLAMS",
        "earth-slams",
        "Short large-amplitude magnetic structures (SLAMS)",
        "Short large-amplitude magnetic structures (SLAMS): steepened magnetic pulsations of the quasi-parallel foreshock, several times the ambient field."
    ),
    tums!(
        "Earth",
        "HFA",
        "earth-hfa",
        "Hot flow anomalies",
        "Hot flow anomalies (HFAs): cores of hot, deflected and depleted plasma formed where an interplanetary discontinuity meets the bow shock."
    ),
    tums!(
        "Earth",
        "HFA/EOF",
        "earth-hfa-eof",
        "Edge-of-foreshock hot flow anomalies (HFA/EOF)",
        "The structures TUMS types \"HFA/Edge of Foreshock (EOF)\"."
    ),
    tums!(
        "Earth",
        "SHFA",
        "earth-shfa",
        "Spontaneous hot flow anomalies",
        "Spontaneous hot flow anomalies (SHFAs): structures like hot flow anomalies that form in the foreshock without an interplanetary discontinuity."
    ),
    tums!(
        "Earth",
        "FC",
        "earth-fc",
        "Foreshock cavities and traveling foreshocks",
        "The structures TUMS types \"Foreshock cavity/Traveling foreshock (FC)\"."
    ),
    tums!(
        "Earth",
        "FB",
        "earth-fb",
        "Foreshock bubbles",
        "Foreshock bubbles (FBs): large structures formed upstream of the bow shock where foreshock ions meet an interplanetary discontinuity, with a shock ahead of them."
    ),
    tums!(
        "Earth",
        "Caviton",
        "earth-caviton",
        "Foreshock cavitons",
        "Foreshock cavitons: small depressions of the magnetic field and density in the foreshock, bounded by compressions."
    ),
    tums!(
        "Earth",
        "DH",
        "earth-dh",
        "Foreshock density holes",
        "Foreshock density holes (DH): short drops of the plasma density in the foreshock."
    ),
    tums!(
        "Earth",
        "Unknown",
        "earth-unknown",
        "Unclassified upstream transients",
        "Upstream transients TUMS lists without a type (\"Unknown\")."
    ),
    tums!(
        "Mars",
        "SLAMS",
        "mars-slams",
        "Short large-amplitude magnetic structures (SLAMS)",
        "Short large-amplitude magnetic structures (SLAMS): steepened magnetic pulsations of the quasi-parallel foreshock, several times the ambient field."
    ),
    tums!(
        "Mars",
        "HFA",
        "mars-hfa",
        "Hot flow anomalies",
        "Hot flow anomalies (HFAs): cores of hot, deflected and depleted plasma formed where an interplanetary discontinuity meets the bow shock."
    ),
    tums!(
        "Mars",
        "*",
        "mars-other",
        "Other upstream structures",
        "The structures TUMS lists at Mars too rarely for a list of their own; `type` says which (TUMS's code: FB foreshock bubble, FC foreshock cavity, SHFA spontaneous hot flow anomaly, HFA/EOF, Unknown...)."
    ),
    tums!(
        "Mercury",
        "SLAMS",
        "mercury-slams",
        "Short large-amplitude magnetic structures (SLAMS)",
        "Short large-amplitude magnetic structures (SLAMS): steepened magnetic pulsations of the quasi-parallel foreshock, several times the ambient field."
    ),
    tums!(
        "Mercury",
        "HFA",
        "mercury-hfa",
        "Hot flow anomalies",
        "Hot flow anomalies (HFAs): cores of hot, deflected and depleted plasma formed where an interplanetary discontinuity meets the bow shock."
    ),
    tums!(
        "Venus",
        "FC",
        "venus-fc",
        "Foreshock cavities and traveling foreshocks",
        "The structures TUMS types \"Foreshock cavity/Traveling foreshock (FC)\"."
    ),
    tums!(
        "Venus",
        "HFA",
        "venus-hfa",
        "Hot flow anomalies",
        "Hot flow anomalies (HFAs): cores of hot, deflected and depleted plasma formed where an interplanetary discontinuity meets the bow shock."
    ),
    tums!(
        "Venus",
        "*",
        "venus-other",
        "Other upstream structures",
        "The structures TUMS lists at Venus too rarely for a list of their own; `type` says which (TUMS's code: SLAMS, SHFA spontaneous hot flow anomaly...)."
    ),
    tums!(
        "Saturn",
        "HFA",
        "saturn-hfa",
        "Hot flow anomalies",
        "Hot flow anomalies (HFAs): cores of hot, deflected and depleted plasma formed where an interplanetary discontinuity meets the bow shock."
    ),
    tums!(
        "Jupiter",
        "*",
        "jupiter-other",
        "Upstream structures",
        "The few structures TUMS lists at Jupiter; `type` says which (TUMS's code: SLAMS, HFA hot flow anomaly)."
    ),
];

/// Every list: those read from their own sources, then those VizieR keeps.
fn lists() -> impl Iterator<Item = &'static List> {
    LISTS.iter().chain(&vizier::LISTS)
}

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
pub(super) fn value(text: &str) -> Option<Value> {
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

/// One row of TUMS's export.
#[derive(serde::Deserialize)]
struct TumsRow {
    #[serde(rename = "Date")]
    date: String,
    #[serde(rename = "Type")]
    kind: String,
    #[serde(rename = "Mission")]
    mission: String,
    #[serde(rename = "Planet")]
    planet: String,
    #[serde(rename = "Reference")]
    reference: String,
    #[serde(rename = "Doi")]
    doi: String,
}

impl TumsRow {
    /// The whole row: TUMS gives no key of its own.
    fn id(&self) -> String {
        [
            &self.date,
            &self.kind,
            &self.mission,
            &self.planet,
            &self.reference,
            &self.doi,
        ]
        .map(String::as_str)
        .join("|")
    }

    /// The `part` of the list that takes it: its planet and type's own, else its planet's
    /// other structures.
    fn part<'a>(&self, parts: &[&'a str]) -> Option<&'a str> {
        let own = format!("{}:{}", self.planet, self.kind);
        let other = format!("{}:*", self.planet);
        let find = |wanted: &str| parts.iter().copied().find(|part| *part == wanted);
        find(&own).or_else(|| find(&other))
    }
}

/// TUMS's missions as their teams write them.
const TUMS_MISSIONS: [(&str, &str); 4] = [
    ("Maven", "MAVEN"),
    ("Messenger", "MESSENGER"),
    ("Themis", "THEMIS"),
    ("VEX", "Venus Express"),
];

/// TUMS's export: one structure per row, `Date` (UT, `1988-07-22 14:18:00.00`), `Type`,
/// `Mission`, `Planet`, `Reference` and `Doi`. `part` is `Planet:Type`, or `Planet:*` for the
/// planet's types no list of their own takes. A row no list takes fails the read, so a new
/// type or planet at TUMS shows instead of being dropped; a repeated row counts once.
fn tums(text: &str, part: &str) -> Result<Vec<Item>, String> {
    let parts: Vec<&str> = LISTS
        .iter()
        .filter(|list| list.key.starts_with("tums-"))
        .map(|list| list.part)
        .collect();
    let rows: Vec<TumsRow> = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes())
        .deserialize()
        .collect::<Result<_, _>>()
        .map_err(|error| format!("TUMS export: {error}"))?;
    if let Some(row) = rows.iter().find(|row| row.part(&parts).is_none()) {
        return Err(format!(
            "no TUMS list takes {} at {}: add one",
            row.kind, row.planet
        ));
    }
    let mut seen = std::collections::HashSet::new();
    rows.into_iter()
        .filter(|row| row.part(&parts) == Some(part))
        .filter(|row| seen.insert(row.id()))
        .map(tums_item)
        .collect()
}

fn tums_item(row: TumsRow) -> Result<Item, String> {
    let at = Timestamp::parse(&format!("{}Z", row.date.replacen(' ', "T", 1)))
        .ok_or_else(|| format!("bad TUMS date {}", row.date))?;
    let mission = TUMS_MISSIONS
        .iter()
        .find(|(given, _)| *given == row.mission)
        .map_or(row.mission.as_str(), |(_, name)| name);
    let mut attributes = Map::new();
    keep(
        &mut attributes,
        [
            ("type", row.kind.as_str()),
            ("mission", mission),
            ("reference", &row.reference),
            ("doi", &row.doi),
        ],
    );
    Ok(point(row.id(), at, attributes))
}

/// SBCross's columns, as their attributes are named; the times, `CME` and its position are
/// read apart.
const SBCROSS_COLUMNS: [(&str, &str); 13] = [
    ("Polarity", "polarity"),
    ("Duration (hours)", "duration_h"),
    ("Thickness (km)", "thickness_km"),
    ("Velocity Avg (km/s)", "speed_avg_km_s"),
    ("Velocity Std (km/s)", "speed_std_km_s"),
    ("Density Avg (cm^-3)", "density_avg_cm3"),
    ("Density Std (cm^-3)", "density_std_cm3"),
    ("Peak Density (cm^-3)", "density_peak_cm3"),
    ("B Field Avg (nT)", "b_avg_nt"),
    ("B Field Std (nT)", "b_std_nt"),
    ("Beta Avg", "beta_avg"),
    ("Beta Std", "beta_std"),
    ("Confidence", "confidence"),
];

/// The SBCross catalogue's CSV: a sector boundary crossing per row, from `Start Time` to `End
/// Time` (UT, to the hour). `CME` (y/n) becomes `cme`, and `CME Position` (b, a, o) where the
/// associated CME lies: before, after or overlapping. An empty cell is left out.
fn sbcross(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    let mut reader = csv::ReaderBuilder::new().from_reader(text.as_bytes());
    let headers = reader.headers().map_err(|error| error.to_string())?.clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .ok_or_else(|| format!("SBCross has no column {name}"))
    };
    let (start_at, stop_at) = (column("Start Time")?, column("End Time")?);
    let (cme_at, position_at) = (column("CME")?, column("CME Position")?);
    let columns = SBCROSS_COLUMNS
        .iter()
        .map(|(header, name)| Ok((column(header)?, *name)))
        .collect::<Result<Vec<_>, String>>()?;
    let time = |cell: Option<&str>| {
        let cell = cell.unwrap_or_default();
        Timestamp::parse(&format!("{}:00Z", cell.replace(' ', "T")))
            .ok_or_else(|| format!("SBCross: {cell:?} is not a time"))
    };
    reader
        .records()
        .map(|record| {
            let record = record.map_err(|error| error.to_string())?;
            let (start, stop) = (time(record.get(start_at))?, time(record.get(stop_at))?);
            let mut attributes = Map::new();
            for (at, name) in &columns {
                if let Some(cell) = record.get(*at).and_then(value) {
                    attributes.insert((*name).to_owned(), cell);
                }
            }
            attributes.insert("cme".into(), Value::Bool(record.get(cme_at) == Some("y")));
            let position = match record.get(position_at) {
                Some("b") => Some("before"),
                Some("a") => Some("after"),
                Some("o") => Some("overlap"),
                _ => None,
            };
            if let Some(position) = position {
                attributes.insert("cme_position".into(), Value::from(position));
            }
            Ok(Item {
                id: record.get(start_at).unwrap_or_default().to_owned(),
                start,
                stop,
                attributes,
            })
        })
        .collect()
}

/// One synodic Carrington rotation, 27.2753 days, in microseconds.
const CARRINGTON_ROTATION_MICROS: i128 = 2_356_585_920_000;

/// WSO's HCS tilt page: a line per Carrington rotation, `CR 1642  1976:05:27 15h` then the
/// radial model's average, northern and southern extent and the classic model's (degrees).
/// A rotation lasts until the next one starts, or one synodic rotation when the next row is
/// not the next rotation (the last one).
fn wso_tilts(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    const COLUMNS: [&str; 6] = [
        "r_av_deg", "r_n_deg", "r_s_deg", "l_av_deg", "l_n_deg", "l_s_deg",
    ];
    let rows: Vec<(i64, Timestamp, Map<String, Value>)> = text
        .lines()
        .filter_map(|line| line.strip_prefix("CR "))
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [number, date, hour, values @ ..] = fields.as_slice() else {
                return Err(format!("a WSO tilt row has too few fields: CR {line}"));
            };
            let rotation: i64 = number
                .parse()
                .map_err(|_| format!("CR {line}: not a rotation"))?;
            let at = Timestamp::parse(&format!(
                "{}T{}:00:00Z",
                date.replace(':', "-"),
                hour.trim_end_matches('h')
            ))
            .ok_or_else(|| format!("CR {line}: not a start date"))?;
            if values.len() != COLUMNS.len() {
                return Err(format!("CR {line}: expected {} values", COLUMNS.len()));
            }
            let mut attributes = Map::new();
            attributes.insert("carrington_rotation".into(), Value::from(rotation));
            for (name, value) in COLUMNS.iter().zip(values) {
                let value: f64 = value
                    .parse()
                    .map_err(|_| format!("CR {line}: {value} is not a number"))?;
                attributes.insert((*name).into(), Value::from(value));
            }
            Ok((rotation, at, attributes))
        })
        .collect::<Result<_, String>>()?;
    if rows.is_empty() {
        return Err("no Carrington rotation in the WSO tilt page".to_owned());
    }
    let ends: Vec<Option<Timestamp>> = rows
        .iter()
        .zip(rows.iter().skip(1).map(Some).chain([None]))
        .map(|((rotation, _, _), next)| match next {
            Some((following, at, _)) if *following == rotation + 1 => Some(*at),
            _ => None,
        })
        .collect();
    rows.into_iter()
        .zip(ends)
        .map(|((rotation, start, attributes), end)| {
            let stop = match end {
                Some(end) => end,
                None => {
                    Timestamp::from_unix_micros(start.unix_micros() + CARRINGTON_ROTATION_MICROS)
                        .ok_or_else(|| format!("CR {rotation} ends out of range"))?
                }
            };
            Ok(Item {
                id: format!("CR {rotation}"),
                start,
                stop,
                attributes,
            })
        })
        .collect()
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

/// The ICME multi-catalogue (e-cienciaDatos): a CSV whose events run from `icme_start` to
/// `mo_end`. Of its 160 columns, the ones that place and source the event are kept.
fn icme_multicatalog(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    let mut items = timed_csv(text, "icme_start", "mo_end")?;
    for item in &mut items {
        let row = std::mem::take(&mut item.attributes);
        let cell = |column: &str| row.get(column).and_then(Value::as_str).unwrap_or_default();
        let number = |column: &str| row.get(column).filter(|value| value.is_number()).cloned();
        // Some ICMEs come from two source catalogues, with slightly different ends: both stay.
        item.id = format!(
            "{}|{}|{}",
            item.start.rfc3339().unwrap_or_default(),
            cell("spacecraft"),
            cell("cat_source")
        );
        let sheath = row.get("flg_sheath").and_then(Value::as_f64);
        let fields = [
            (
                "spacecraft",
                Some(icme_spacecraft(cell("spacecraft")).into()),
            ),
            ("source", Some(icme_source(cell("cat_source")).into())),
            (
                "mo_start",
                Timestamp::parse(cell("mo_start"))
                    .and_then(|at| at.rfc3339().ok())
                    .map(Value::from),
            ),
            ("r_km", number("r_heliodist")),
            ("longitude_hci_deg", number("theta")),
            ("latitude_hci_deg", number("lati")),
            (
                "sheath",
                sheath.map(|flag| (if flag > 0.0 { "yes" } else { "no" }).into()),
            ),
        ];
        item.attributes = fields
            .into_iter()
            .filter_map(|(name, value)| Some((name.to_owned(), value?)))
            .collect();
    }
    Ok(items)
}

/// The multi-catalogue spells some spacecraft two ways.
fn icme_spacecraft(name: &str) -> &str {
    match name {
        "SO" | "SolarOrbiter" => "Solar Orbiter",
        "helios" => "Helios",
        "ULYSSES" => "Ulysses",
        other => other,
    }
}

/// The catalogue behind each `cat_N` code, from the multi-catalogue's readme.
fn icme_source(code: &str) -> &str {
    match code {
        "cat_1" => "Möstl et al. (2020), doi:10.3847/1538-4357/abb9a1",
        "cat_2" => "Nieves-Chinchilla et al. (2018), doi:10.1007/s11207-018-1247-z",
        "cat_3" => "STEREO ICME list, https://parker.gsfc.nasa.gov/ICME_catalogs/STEREO/",
        "cat_5" => "Helios ICME list, https://parker.gsfc.nasa.gov/ICME_catalogs/Helios/",
        "cat_6" => "Solar Orbiter ICME list, https://parker.gsfc.nasa.gov/ICME_catalogs/SO/",
        "cat_7" => "Parker Solar Probe ICME list, https://parker.gsfc.nasa.gov/ICME_catalogs/PSP/",
        "cat_8" => "Commission et al. (2014), doi:10.2769/88565",
        "cat_9" => "Regnault et al. (2020), doi:10.1029/2020JA028150",
        "cat_10" => "Richardson & Cane (2010), doi:10.1007/s11207-010-9568-6",
        "cat_11" => "Jian et al. (2018), doi:10.3847/1538-4357/aab189",
        "cat_12" => "Jian et al. (2006), doi:10.1007/s11207-006-0133-2",
        "cat_13" => "Du et al. (2010), doi:10.1007/s11207-009-9505-8",
        other => other,
    }
}

/// A LLAMAICE time, `1995-02-07T18:48` or with seconds; `-` and `None` mark a missing one.
fn llamaice_time(cell: &str) -> Result<Option<Timestamp>, String> {
    let parsed = match cell.trim() {
        "-" | "None" | "" => return Ok(None),
        minutes if minutes.len() == 16 => Timestamp::parse(&format!("{minutes}:00")),
        seconds => Timestamp::parse(seconds),
    };
    parsed.map(Some).ok_or_else(|| format!("bad time {cell:?}"))
}

fn time_value(at: Timestamp) -> Option<Value> {
    at.rfc3339().ok().map(Value::from)
}

/// The two LLAMAICE files, read together: the team's boundaries, then the catalogues'.
const FILE_BREAK: &str = "\n\u{1c}\n";

/// Every LLAMAICE candidate CME, a row of the team's CSV: its number, the CMEs it interacts
/// with, five boundaries in time order (sheath start, start of the sheath/ejecta mixed region,
/// ejecta start and end, end of the trailing mixed region), the high-speed stream catalogues
/// it matches, and a comment saying whether the team believes it is a CME. A candidate the
/// team gave no boundary spans what the catalogues that listed it said.
fn llamaice(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    const BOUNDARIES: [&str; 5] = [
        "sheath_start",
        "front_mixed_start",
        "ejecta_start",
        "ejecta_end",
        "back_mixed_end",
    ];
    let (team, catalogues) = text
        .split_once(FILE_BREAK)
        .ok_or("the catalogues' boundaries are missing")?;
    let listed = llamaice_catalogues(catalogues)?;
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_reader(team.as_bytes());
    let mut items = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|error| error.to_string())?;
        let cell = |at: usize| record.get(at).unwrap_or_default();
        let listing: Vec<_> = listed.iter().filter(|row| row.event == cell(0)).collect();
        let mut attributes = Map::new();
        let mut times = Vec::new();
        for (index, name) in BOUNDARIES.iter().enumerate() {
            if let Some(at) = llamaice_time(cell(2 + index))? {
                times.push(at);
                attributes.extend(time_value(at).map(|value| (name.to_string(), value)));
            }
        }
        let bounds_from = if times.is_empty() {
            times.extend(
                listing
                    .iter()
                    .flat_map(|row| [row.item.start, row.item.stop]),
            );
            "catalogues"
        } else {
            "LLAMAICE"
        };
        let (Some(&start), Some(&stop)) = (times.iter().min(), times.iter().max()) else {
            return Err(format!("no time for LLAMAICE CME {}", cell(0)));
        };
        let mut names: Vec<_> = listing.iter().map(|row| row.catalogue).collect();
        names.sort_unstable();
        names.dedup();
        attributes.insert("bounds_from".to_owned(), bounds_from.into());
        attributes.insert("catalogues".to_owned(), names.join(", ").into());
        keep(
            &mut attributes,
            [
                ("llamaice_event", cell(0)),
                ("interacts_with", cell(1)),
                ("hss", cell(7)),
                ("comment", cell(8)),
            ],
        );
        items.push(Item {
            id: cell(0).to_owned(),
            start,
            stop,
            attributes,
        });
    }
    Ok(items)
}

/// One ICME of a catalogue LLAMAICE collates, with the LLAMAICE CME it was matched to.
struct Listed<'a> {
    event: &'a str,
    catalogue: &'a str,
    item: Item,
}

/// The ICMEs of the catalogues LLAMAICE collates, from the file grouping them by LLAMAICE CME:
/// whitespace-separated rows of the LLAMAICE number, its time and day of year, the catalogue,
/// the sheath start, the ejecta start and end, and the time of the CME at the Sun. An ICME
/// LLAMAICE matched to several of its times is repeated: it stays once.
fn llamaice_catalogues(text: &str) -> Result<Vec<Listed<'_>>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut listed = Vec::new();
    for line in text.lines() {
        let cells: Vec<&str> = line.split_whitespace().collect();
        let [event, _, _, catalogue, sheath, front, end, cme] = cells[..] else {
            if cells.is_empty() {
                continue;
            }
            return Err(format!("not 8 cells: {line:?}"));
        };
        if !seen.insert((catalogue, sheath, front, end)) {
            continue;
        }
        let missing = || format!("no ejecta in {line:?}");
        let ejecta_start = llamaice_time(front)?.ok_or_else(missing)?;
        let ejecta_end = llamaice_time(end)?.ok_or_else(missing)?;
        let id = format!("{sheath}|{front}|{end}");
        let sheath = llamaice_time(sheath)?;
        let mut attributes = Map::new();
        keep(&mut attributes, [("llamaice_event", event)]);
        let times = [
            ("sheath_start", sheath),
            ("ejecta_start", Some(ejecta_start)),
            ("ejecta_end", Some(ejecta_end)),
            ("cme_time", llamaice_time(cme)?),
        ];
        for (name, at) in times {
            attributes.extend(
                at.and_then(time_value)
                    .map(|value| (name.to_owned(), value)),
            );
        }
        let start = sheath.unwrap_or(ejecta_start);
        let item = Item {
            id,
            start,
            stop: ejecta_end.max(start),
            attributes,
        };
        listed.push(Listed {
            event,
            catalogue,
            item,
        });
    }
    Ok(listed)
}

/// The ICMEs of one catalogue LLAMAICE collates, `part`.
fn llamaice_source(text: &str, part: &str) -> Result<Vec<Item>, String> {
    Ok(llamaice_catalogues(text)?
        .into_iter()
        .filter(|row| row.catalogue == part)
        .map(|row| row.item)
        .collect())
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

/// The SODA shock list (NASA GSFC): a JSON array of the shocks the algorithm found at Parker
/// Solar Probe, Solar Orbiter and STEREO-A; `part` picks a spacecraft by its code (PSP, SLO,
/// STA). Its numbers are strings, a blank one is none.
fn soda_shocks(text: &str, part: &str) -> Result<Vec<Item>, String> {
    const KEPT: [(&str, &str); 14] = [
        ("Type", "type"),
        ("Quality", "quality"),
        ("RHS", "rhs"),
        ("B_comp", "b_compression"),
        ("n_comp", "density_compression"),
        ("delta_v", "delta_v_km_s"),
        ("beta_up", "beta_upstream"),
        ("theta_bn", "theta_bn_deg"),
        // `machx` is never below `mmsx` at a fast shock: it is the Alfvén Mach number.
        ("machx", "mach_alfven"),
        ("mmsx", "mach_magnetosonic"),
        ("distance", "distance_rs"),
        ("latitude", "latitude_heeq_deg"),
        ("longitude", "longitude_heeq_deg"),
        ("driver", "driver"),
    ];
    let shocks: Vec<Map<String, Value>> =
        serde_json::from_str(text).map_err(|error| error.to_string())?;
    shocks
        .iter()
        .filter(|shock| shock.get("SC").and_then(Value::as_str) == Some(part))
        .map(|shock| {
            let cell = |field: &str| match shock.get(field) {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Number(number)) => number.to_string(),
                _ => String::new(),
            };
            let id = cell("ID");
            let at = slash_time(&cell("Time")).ok_or_else(|| format!("bad time in {id}"))?;
            let cells: Vec<(&str, String)> = KEPT
                .iter()
                .map(|(field, name)| (*name, cell(field)))
                .collect();
            let mut attributes = Map::new();
            keep(
                &mut attributes,
                cells
                    .iter()
                    .map(|(name, cell)| (*name, cell.trim().trim_end_matches('*'))),
            );
            // The page: a starred Mach number was computed with a separate tool, for a shock
            // without the flow's direction.
            if cell("machx").trim().ends_with('*') {
                attributes.insert("mach_alfven_estimated".to_owned(), Value::from(true));
            }
            Ok(point(id, at, attributes))
        })
        .collect()
}

/// A LASSOS ICME catalogue (NASA GSFC) as pandas writes a table to JSON: `{schema, data}`, a
/// row per ICME. An ICME runs from its start (its shock, when it has one) to its end. A speed
/// of 0 is none: the spacecraft measured no plasma.
fn lassos_icmes(text: &str, _part: &str) -> Result<Vec<Item>, String> {
    const KEPT: [(&str, &str); 9] = [
        ("SC", "spacecraft"),
        ("MOclass", "mo_class"),
        ("meanB", "mo_mean_b_nt"),
        ("meanVsw", "mo_mean_speed_km_s"),
        ("Vexp", "mo_expansion_speed_km_s"),
        ("DiP", "distortion_parameter"),
        ("Rau", "distance_au"),
        ("R_au", "distance_au"),
        ("shock", "shock"),
    ];
    let table: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let rows = table["data"].as_array().ok_or("no data")?;
    rows.iter()
        .map(|row| {
            let time = |field: &str| row[field].as_str().and_then(Timestamp::parse);
            let (Some(start), Some(stop)) = (time("ICME_datestart"), time("ICME_dateend")) else {
                return Err(format!("bad times in {row}"));
            };
            // Some catalogues name several ICMEs "-": the start, on its spacecraft, keys one.
            let begun = row["ICME_datestart"].as_str().unwrap_or_default();
            let id = match row["SC"].as_str() {
                Some(spacecraft) => format!("{spacecraft} {begun}"),
                None => begun.to_owned(),
            };
            let mut attributes = Map::new();
            if let Some(mo) = time("MO_datestart").and_then(|at| at.rfc3339().ok()) {
                attributes.insert("mo_start".to_owned(), Value::from(mo));
            }
            for (field, name) in KEPT {
                let speed = matches!(field, "meanVsw" | "Vexp");
                let none = row[field].is_null() || speed && row[field].as_f64() == Some(0.0);
                if !none {
                    attributes.insert(name.to_owned(), row[field].clone());
                }
            }
            Ok(Item {
                id,
                start,
                stop: stop.max(start),
                attributes,
            })
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
        Files::Mirrored { urls, answered } => {
            mirrored(http, urls, *answered).await.map(|text| vec![text])
        }
        Files::Remote(urls) => remote(http, urls, downloaded).await,
        Files::Together(urls) => {
            let texts = remote(http, urls, downloaded).await?;
            Ok(vec![texts.join(FILE_BREAK)])
        }
        Files::Posted { url, form } => {
            if !downloaded.contains_key(url) {
                downloaded.insert(url, posted(http, url, form).await);
            }
            Ok(vec![downloaded[url].clone()?])
        }
    }
}

async fn posted(
    http: &reqwest::Client,
    url: &str,
    form: &[(&str, &str)],
) -> Result<String, String> {
    let bytes = super::patient::post_form(http, url, form, BUSY_PAUSE).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Each of `urls`, downloaded once per run however many lists read it.
async fn remote(
    http: &reqwest::Client,
    urls: &[&'static str],
    downloaded: &mut HashMap<&'static str, Result<String, String>>,
) -> Result<Vec<String>, String> {
    let mut texts = Vec::with_capacity(urls.len());
    for url in urls {
        if !downloaded.contains_key(url) {
            downloaded.insert(url, download(http, url).await);
        }
        texts.push(downloaded[url].clone()?);
    }
    Ok(texts)
}

/// The first answer from `urls` that `answered` accepts, trying them all again after a pause
/// when none does; the last round's failures when none ever does.
async fn mirrored(
    http: &reqwest::Client,
    urls: &[&str],
    answered: fn(&str) -> Result<(), String>,
) -> Result<String, String> {
    let mut failures = Vec::new();
    for round in 0..MIRROR_ROUNDS {
        if round > 0 {
            tokio::time::sleep(MIRROR_PAUSE).await;
        }
        failures.clear();
        for url in urls {
            match download(http, url).await {
                Ok(text) => match answered(&text) {
                    Ok(()) => return Ok(text),
                    Err(error) => failures.push(format!("{url}: {error}")),
                },
                Err(error) => failures.push(format!("{url}: {error}")),
            }
        }
    }
    Err(failures.join("; "))
}

/// What the lists are, as built: their descriptions, where their files are, and the kept
/// tables themselves. A new list, or a corrected table, changes it.
// simplify: a change to a reader alone (`read`) is not seen; it waits for the weekly run.
fn fingerprint<'a>(lists: impl IntoIterator<Item = &'a List>) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for list in lists {
        let files: Vec<&str> = match &list.files {
            Files::Remote(urls) | Files::Together(urls) => urls.to_vec(),
            Files::Mirrored { urls, .. } => urls.to_vec(),
            Files::Kept(text) => vec![text],
            Files::Linked { index, pattern } => vec![index, pattern],
            Files::Posted { url, form } => std::iter::once(*url)
                .chain(form.iter().flat_map(|(name, value)| [*name, *value]))
                .collect(),
        };
        let described = [
            list.key,
            list.folder,
            list.name,
            list.about,
            list.credit,
            list.url,
            list.part,
        ];
        for part in described.into_iter().chain(files) {
            hash.update(part.as_bytes());
            hash.update([0]);
        }
    }
    format!("{:x}", hash.finalize())
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

    fn built(&self) -> Option<String> {
        Some(fingerprint(lists()))
    }

    async fn fetch(
        &self,
        http: &reqwest::Client,
        _settings: &Value,
        _refreshed: &HashMap<String, Timestamp>,
    ) -> Result<Vec<Fetched>, String> {
        let mut fetched_lists = Vec::new();
        let mut downloaded = HashMap::new();
        for list in lists() {
            let events = match texts(http, &list.files, &mut downloaded).await {
                Ok(texts) => events(list, &texts),
                Err(error) => Err(error),
            };
            fetched_lists.push(fetched(list, events));
        }
        Ok(fetched_lists)
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
    const ICME_MULTI: &str =
        include_str!("../../tests/fixtures/relay/literature_icme_multicatalog.csv");
    const LLAMAICE: &str = include_str!("../../tests/fixtures/relay/literature_llamaice.csv");
    const LLAMAICE_SOURCES: &str =
        include_str!("../../tests/fixtures/relay/literature_llamaice_sources.dat");

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
    fn the_kept_fuselier_table_is_fifteen_crossings_at_their_minute() {
        let items = kept_table(include_str!("data/fuselier_2026_multiple_xlines.csv"), "").unwrap();
        assert_eq!(items.len(), 15);
        assert_eq!(items[0].start, at("2017-12-29T04:01:00Z"));
        assert_eq!(items[0].stop, items[0].start);
        assert_eq!(items[7].attributes["imf_bx_over_b"], json!(-0.2));
        assert_eq!(
            items[1].attributes["multiple_reconnection_in_llbl"],
            json!("no")
        );
        assert!(
            !items[1]
                .attributes
                .contains_key("multiple_reconnection_in_msbl")
        );
        assert!(
            LISTS
                .iter()
                .any(|list| list.key == "mms-multiple-xlines-fuselier")
        );
    }

    #[test]
    fn the_turner_bubbles_join_tables_one_and_two_and_last_their_duration() {
        let items = kept_table(include_str!("data/turner_2020_foreshock_bubbles.csv"), "").unwrap();
        assert_eq!(items.len(), 10);
        assert_eq!(items[0].start, at("2017-12-18T12:56:00Z"));
        assert_eq!(items[0].stop, at("2017-12-18T12:56:52Z"));
        assert_eq!(items[0].attributes["sw_fast_mach"], json!(7.7));
        assert_eq!(items[0].attributes["magnetic_holes_in_core"], json!(6.0));
        assert_eq!(items[8].attributes["age_s"], json!(275.0));
        assert!(!items[9].attributes.contains_key("age_s"));
        assert!(
            LISTS
                .iter()
                .any(|list| list.key == "mms-foreshock-bubbles-turner")
        );
    }

    #[test]
    fn the_ma_cmes_are_tables_one_and_two_as_printed() {
        let stealth = kept_table(include_str!("data/ma_2010_stealth_cmes.csv"), "").unwrap();
        let with_lcs = kept_table(include_str!("data/ma_2010_cmes_with_lcs.csv"), "").unwrap();
        assert_eq!(
            (stealth.len(), with_lcs.len()),
            (11, 23),
            "34 Earth-sided CMEs"
        );
        assert_eq!(stealth[0].start, at("2009-01-07T17:37:00Z"));
        assert_eq!(
            stealth[0].attributes["character"],
            json!("Cup-shaped, blowout")
        );
        assert_eq!(
            stealth[1].attributes["disk_cme_time"],
            json!("2009-01-09T04:07:00Z")
        );
        assert_eq!(with_lcs[22].attributes["lcs"], json!("Filament eruption"));
        assert_eq!(with_lcs[9].attributes["disk_width_deg"], json!(160.0));
    }

    #[test]
    fn the_voyager_crossings_run_forward_even_when_the_table_does_not() {
        let crossings = kept_table(include_str!("data/choi_2025_voyager2_hcs.csv"), "").unwrap();
        assert_eq!(crossings.len(), 188);
        let fourteenth = &crossings[13];
        assert_eq!(fourteenth.start, at("1978-03-27T20:44:52Z"));
        assert_eq!(fourteenth.stop, at("1978-03-27T21:18:11Z"));
        assert_eq!(fourteenth.attributes["end_before_start"], json!("yes"));
        assert_eq!(crossings[187].attributes["seq"], json!(188.0));
    }

    #[test]
    fn a_sbcross_row_is_a_sector_boundary_crossing() {
        let crossings =
            sbcross(include_str!("../../tests/fixtures/relay/sbcross.csv"), "").unwrap();
        assert_eq!(crossings.len(), 5);
        assert_eq!(crossings[0].start, at("1995-01-02T04:00:00Z"));
        assert_eq!(crossings[0].stop, at("1995-01-02T07:00:00Z"));
        let first = &crossings[0].attributes;
        assert_eq!(first["polarity"], json!("+,-"));
        assert_eq!(first["confidence"], json!("low"));
        assert_eq!(first["cme"], json!(false));
        assert!(!first.contains_key("cme_position"), "no CME, no position");
        assert_eq!(first["speed_avg_km_s"], json!(342.8));
        assert_eq!(crossings[1].attributes["cme_position"], json!("overlap"));
        assert_eq!(crossings[2].attributes["cme_position"], json!("before"));
        assert!(
            !crossings[3].attributes.contains_key("thickness_km"),
            "empty: left out"
        );
        assert!(sbcross("Start Time,End Time\nnot a time,x\n", "").is_err());
    }

    #[test]
    fn a_wso_tilt_row_is_its_carrington_rotation() {
        let page = include_str!("../../tests/fixtures/relay/wso_tilts.html");
        let rotations = wso_tilts(page, "").unwrap();
        assert_eq!(rotations.len(), 4);
        assert_eq!(rotations[0].start, at("1976-05-27T15:00:00Z"));
        assert_eq!(
            rotations[0].stop,
            at("1976-06-23T21:00:00Z"),
            "to the next rotation"
        );
        assert_eq!(rotations[0].attributes["carrington_rotation"], json!(1642));
        assert_eq!(rotations[0].attributes["l_s_deg"], json!(-15.5));
        let lasts = |item: &Item| item.stop.unix_micros() - item.start.unix_micros();
        assert_eq!(
            lasts(&rotations[2]),
            2_356_585_920_000,
            "1645 is not the next row"
        );
        let last = &rotations[3];
        assert_eq!(last.attributes["carrington_rotation"], json!(2302));
        assert_eq!(
            lasts(last),
            2_356_585_920_000,
            "the last one lasts a rotation"
        );
        assert!(wso_tilts("<html>no table</html>", "").is_err());
    }

    #[test]
    fn the_bernold_fine_structures_are_table_one_as_printed() {
        let items = kept_table(
            include_str!("data/bernold_1980_type4_fine_structures.csv"),
            "",
        )
        .unwrap();
        assert_eq!(items.len(), 29);
        assert_eq!(items[0].start, at("1977-12-24T13:03:55Z"));
        assert_eq!(items[0].stop, at("1977-12-24T13:05:35Z"));
        assert_eq!(items[0].attributes["characteristic"], json!("rain"));
        assert_eq!(items[0].attributes["sweeps_per_s"], json!(8.0));
        assert_eq!(items[26].attributes["figure"], json!("Fig. 26"));
        assert_eq!(
            items[28].start,
            at("1978-10-13T12:54:35Z"),
            "as Table I prints it"
        );
        let list = LISTS
            .iter()
            .find(|list| list.key == "eth-type-iv-fine-structures-bernold");
        assert!(list.unwrap().about.contains("12:59:35"));
    }

    #[test]
    fn the_direcd_cmes_join_tables_one_and_three_and_last_their_dimming() {
        let items = kept_table(include_str!("data/jain_2026_direcd.csv"), "").unwrap();
        assert_eq!(items.len(), 31);
        assert_eq!(items[0].start, at("2021-09-28T05:23:00Z"));
        assert_eq!(items[0].stop, at("2021-09-28T07:52:00Z"));
        assert_eq!(items[0].attributes["inclination_deg"], json!(8.1));
        assert_eq!(items[0].attributes["dst_observed_nt"], json!(-31.0));
        assert_eq!(items[22].stop, at("2025-05-31T00:51:00Z"), "past midnight");
        assert!(
            !items[8].attributes.contains_key("cme_speed_km_s"),
            "no speed in Table 3"
        );
        assert_eq!(items[26].attributes["quick_look_data"], json!("yes"));
        assert!(
            LISTS
                .iter()
                .any(|list| list.key == "sdo-cme-directions-direcd")
        );
    }

    #[test]
    fn the_turner_magnetic_holes_last_their_crossing_time() {
        let items = kept_table(include_str!("data/turner_2020_magnetic_holes.csv"), "").unwrap();
        assert_eq!(items.len(), 25);
        assert_eq!(items[1].start, at("2019-01-08T11:47:36.3Z"));
        assert_eq!(items[1].stop, at("2019-01-08T11:47:36.35Z"));
        assert_eq!(items[1].attributes["size_km"], json!(8.8));
        let list = LISTS
            .iter()
            .find(|list| list.key == "mms-magnetic-holes-in-foreshock-bubbles-turner");
        assert!(
            list.unwrap()
                .about
                .contains("not solar wind magnetic holes")
        );
    }

    #[test]
    fn the_lee_bubbles_are_twenty_three_points_at_their_minute() {
        let items = kept_table(include_str!("data/lee_2023_foreshock_bubbles.csv"), "").unwrap();
        assert_eq!(items.len(), 23);
        assert_eq!(items[0].start, at("2017-01-30T07:37:00Z"));
        assert_eq!(items[0].stop, items[0].start);
        assert_eq!(items[1].attributes["vx_min_over_vx0"], json!(0.03));
        assert_eq!(
            items[14].attributes["proton_flux_enhancement"],
            json!(-0.06)
        );
        assert_eq!(items[22].start, at("2019-01-25T06:50:00Z"));
        let list = LISTS
            .iter()
            .find(|list| list.key == "mms-foreshock-bubbles-lee");
        assert!(list.unwrap().credit.contains("no open licence"));
    }

    /// Rows of TUMS's export, as it gives them: a row repeated, blank references.
    const TUMS_SAMPLE: &str = r#"Date,Type,Mission,Planet,Reference,Doi
"1988-07-22 14:18:00.00",HFA,MGS,Mars,"Oieroset et al. (2001)",https://doi.org/10.1029/2000GL012289
"2015-01-01 19:55:35.00",SLAMS,Maven,Mars,"Wong Chan et al. (2026)",https://doi.org/10.1029/2025JA034918
"2007-08-09 17:58:18.00",HFA,Themis,Earth," "," "
"2008-10-04 14:30:55.00",SHFA,Themis,Earth," "," "
"2008-10-04 14:30:55.00",SHFA,Themis,Earth," "," "
"2006-09-15 04:18:00.00",FC,VEX,Venus,"Collinson et al. (2020)",https://doi.org/10.1029/2020JA028023
"2009-04-11 02:44:00.00",SLAMS,VEX,Venus," "," "
"1992-02-02 18:44:00.00",SLAMS,Ulysses,Jupiter,"Tsurutani et al. (1993)",https://doi.org/10.1029/93JA02586
"2016-06-15 17:15:00.00",HFA,Juno,Jupiter,"Valek et al. (2009)",https://doi.org/10.1029/2009GL037128
"#;

    fn tums_lists() -> impl Iterator<Item = &'static List> {
        LISTS.iter().filter(|list| list.key.starts_with("tums-"))
    }

    #[test]
    fn a_tums_row_goes_to_the_list_of_its_planet_and_type_with_its_mission_and_reference() {
        let hfa = tums(TUMS_SAMPLE, "Mars:HFA").unwrap();
        assert_eq!(hfa.len(), 1);
        assert_eq!(hfa[0].start, at("1988-07-22T14:18:00Z"));
        assert_eq!(hfa[0].stop, hfa[0].start);
        assert_eq!(hfa[0].attributes["mission"], json!("MGS"));
        assert_eq!(
            hfa[0].attributes["reference"],
            json!("Oieroset et al. (2001)")
        );
        assert_eq!(
            hfa[0].attributes["doi"],
            json!("https://doi.org/10.1029/2000GL012289")
        );
        let earth = tums(TUMS_SAMPLE, "Earth:HFA").unwrap();
        assert_eq!(earth[0].attributes["mission"], json!("THEMIS"));
        assert!(!earth[0].attributes.contains_key("reference"));
    }

    #[test]
    fn a_planets_rare_structures_go_to_its_other_list_and_a_repeated_row_counts_once() {
        let venus = tums(TUMS_SAMPLE, "Venus:*").unwrap();
        assert_eq!(venus.len(), 1);
        assert_eq!(venus[0].attributes["type"], json!("SLAMS"));
        assert_eq!(venus[0].attributes["mission"], json!("Venus Express"));
        assert_eq!(tums(TUMS_SAMPLE, "Jupiter:*").unwrap().len(), 2);
        assert_eq!(tums(TUMS_SAMPLE, "Earth:SHFA").unwrap().len(), 1);
    }

    #[test]
    fn every_tums_row_is_in_exactly_one_list() {
        let read: usize = tums_lists()
            .map(|list| tums(TUMS_SAMPLE, list.part).unwrap().len())
            .sum();
        assert_eq!(read, 8, "nine rows, one of them twice");
    }

    #[test]
    fn a_tums_row_no_list_takes_fails_the_read() {
        let more = format!("{TUMS_SAMPLE}\"2024-01-01 00:00:00.00\",FCB,MMS,Earth,\" \",\" \"\n");
        let error = tums(&more, "Earth:HFA").unwrap_err();
        assert!(error.contains("FCB at Earth"), "{error}");
    }

    #[test]
    fn the_tums_lists_post_for_the_export_and_credit_unam_word_for_word() {
        const SENTENCE: &str = "\"This paper uses data from the Database of Transient Upstream Mesoscale Structures, generated and maintained by the Universidad Nacional Autónoma de México.\"";
        assert_eq!(tums_lists().count(), 19);
        for list in tums_lists() {
            assert!(matches!(
                list.files,
                Files::Posted {
                    url: "https://tums.unam.mx/",
                    ..
                }
            ));
            assert!(list.credit.contains(SENTENCE), "{}", list.key);
            assert!(
                list.credit
                    .contains("cite the paper in each event's Reference")
            );
            assert!(
                list.folder
                    .starts_with("Literature/Upstream structures (TUMS)/")
            );
        }
    }

    #[tokio::test]
    async fn a_posted_file_is_downloaded_once_however_many_lists_read_it() {
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let seen = asked.clone();
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(move |form: axum::Form<Vec<(String, String)>>| {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move { format!("{:?}", form.0) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url: &'static str = format!("http://{}/", listener.local_addr().unwrap()).leak();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let files = Files::Posted {
            url,
            form: &[("export", "CSV")],
        };
        let (http, mut downloaded) = (reqwest::Client::new(), HashMap::new());

        let first = texts(&http, &files, &mut downloaded).await.unwrap();
        let second = texts(&http, &files, &mut downloaded).await.unwrap();

        assert_eq!(first, [r#"[("export", "CSV")]"#]);
        assert_eq!(first, second);
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn the_kept_tables_converted_from_supplements_read_whole() {
        let read = |text| kept_table(text, "").unwrap();
        let turbulence = read(include_str!("data/stawarz_2022_turbulence_intervals.csv"));
        assert_eq!(turbulence.len(), 60);
        assert_eq!(turbulence[0].start, at("2015-10-21T06:55:10Z"));
        assert_eq!(turbulence[0].stop, at("2015-10-21T07:03:45Z"));
        assert_eq!(
            turbulence[0].attributes["correlation_length_di"],
            json!(21.44)
        );

        let reconnection = read(include_str!("data/stawarz_2022_reconnection_events.csv"));
        assert_eq!(reconnection.len(), 256);
        assert_eq!(reconnection[0].start, at("2015-10-21T06:55:09.9969Z"));
        assert_eq!(reconnection[0].stop, at("2015-10-21T06:55:10.6530Z"));
        assert!(
            !reconnection[0].attributes.contains_key("mms1_dui_l_va"),
            "NaN is no value"
        );

        let solar_wind = read(include_str!(
            "data/olshevsky_2021_mms1_solar_wind_lpp_run.csv"
        ));
        assert_eq!(solar_wind.len(), 13164);
        assert_eq!(solar_wind[0].start, at("2015-09-09T20:16:27.302Z"));
        assert_eq!(solar_wind[0].attributes["confidence"], json!(0.959));
        assert_eq!(solar_wind[0].attributes["samples"], json!(24.0));

        let commencements = read(include_str!("data/isgi_sudden_commencements.csv"));
        assert_eq!(commencements.len(), 3836);
        assert_eq!(commencements[0].start, at("1869-01-20T07:54:00Z"));
        assert_eq!(
            commencements[0].attributes["record_collections"],
            json!("N S T")
        );
        let flare_effects = read(include_str!("data/isgi_solar_flare_effects.csv"));
        assert_eq!(flare_effects.len(), 1377);
        assert_eq!(flare_effects[0].attributes["confirmed"], json!("yes"));
        let quiet = read(include_str!("data/isgi_quiet_days.csv"));
        let disturbed = read(include_str!("data/isgi_disturbed_days.csv"));
        assert_eq!((quiet.len(), disturbed.len()), (11370, 5685));
        assert_eq!(quiet[0].start, at("1932-01-04T00:00:00Z"));
        assert_eq!(quiet[0].stop, at("1932-01-05T00:00:00Z"));
        let ck = read(include_str!("data/isgi_ck_days.csv"));
        assert_eq!(ck.len(), 26988);
        assert_eq!(ck[0].attributes["ck24"], json!("really_quiet"));
        assert!(!ck[0].attributes.contains_key("ck48"), "blank is no value");

        let ftes = read(include_str!("data/fear_2012_cluster_ftes.csv"));
        assert_eq!(ftes.len(), 446);
        assert_eq!(ftes[0].start, at("2002-11-10T09:52:00Z"));
        assert_eq!(ftes[0].stop, ftes[0].start);
        assert_eq!(ftes[0].attributes["imf_bz_gsm_nt"], json!(15.78));

        let boundaries = read(include_str!("data/svalgaard_sector_boundaries.csv"));
        assert_eq!(boundaries.len(), 932);
        assert_eq!(boundaries[0].start, at("1947-01-07T12:00:00Z"));
        assert_eq!(boundaries[0].stop, at("1947-01-08T12:00:00Z"));
        assert_eq!(
            boundaries[0].attributes["polarity_change"],
            json!("toward_to_away")
        );

        let sheets = read(include_str!(
            "data/gingell_2020_reconnecting_current_sheets.csv"
        ));
        assert_eq!(sheets.len(), 165);
        assert_eq!(sheets[0].stop, sheets[0].start);
        assert_eq!(sheets[0].attributes["DT_e_par_eV"], json!(-9.58));
        assert_eq!(sheets[0].attributes["dT_e_tot_eV"], json!(-3.19));
        assert!(
            !sheets[0].attributes.contains_key("v_sh_km_per_s"),
            "Inf is no value"
        );

        let sheath = read(include_str!("data/wang_2021_magnetosheath_intervals.csv"));
        assert_eq!(sheath.len(), 50);
        assert_eq!(
            sheath[0].attributes["shock_type"],
            json!("quasi-perpendicular")
        );

        let sirs = read(include_str!("data/chi_2018_sirs.csv"));
        assert_eq!(sirs.len(), 866);
        let met = sirs
            .iter()
            .filter(|sir| sir.attributes["interacted_with_icme"] == json!("yes"));
        assert_eq!(met.count(), 60);
        for key in [
            "mms-turbulence-stawarz",
            "mms-turbulence-reconnection-stawarz",
            "mms-bow-shock-current-sheets-gingell",
            "mms-magnetosheath-wang",
            "sirs-chi",
        ] {
            assert!(LISTS.iter().any(|list| list.key == key), "{key}");
        }
    }

    #[test]
    fn the_fingerprint_follows_a_kept_table() {
        let mut lists = LISTS.clone();
        let built = fingerprint(&lists);
        assert_eq!(fingerprint(&LISTS), built);
        lists[0].files = Files::Kept("start,stop\n");
        assert_ne!(fingerprint(&lists), built);
    }

    #[test]
    fn the_fingerprint_covers_the_vizier_lists_too() {
        let built = Literature.built().unwrap();
        assert_eq!(
            built,
            fingerprint(lists()),
            "a VizieR list removed or changed must be seen"
        );
        let mut vizier = vizier::LISTS.clone();
        vizier[0].part = "start=Elsewhere";
        assert_ne!(fingerprint(LISTS.iter().chain(&vizier)), built);
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
    fn the_lists_vincent_sent_read_whole_from_their_kept_tables() {
        let count = |key: &str| {
            let list = LISTS.iter().find(|list| list.key == key).unwrap();
            let Files::Kept(text) = list.files else {
                panic!("{key} is not kept")
            };
            (list.read)(text, list.part).unwrap()
        };
        let storms = count("intense-storms-ji");
        assert_eq!(storms.len(), 63);
        assert_eq!(storms[0].attributes["dst_min_nt"], json!(-100.0));
        assert_eq!(storms[0].attributes["structure"], json!("nonMC"));
        let mirror = count("solo-mirror-mode-storms");
        assert_eq!(mirror.len(), 25);
        assert_eq!(mirror[0].start, mirror[0].stop);
        assert_eq!(mirror[7].attributes["mm_type"], json!("1, 2"));
        assert_eq!(mirror[24].attributes["speed_km_s"], json!(286.0));
        let crossings = count("psp-hcs-crossings");
        assert_eq!(crossings.len(), 39);
        assert_eq!(crossings[30].stop, at("2024-06-30T00:42:00Z"));
        assert_eq!(crossings[30].attributes["jet"], json!("inward"));
        assert_eq!(count("psp-hcs-partial-crossings").len(), 29);
        assert_eq!(count("psp-hcs-side-jets").len(), 4);
        let exhausts = count("solo-reconnection-exhausts");
        assert_eq!(exhausts.len(), 146);
        let visual: Vec<_> = exhausts
            .iter()
            .filter(|item| item.attributes["found_by"] == json!("visual inspection"))
            .collect();
        assert_eq!(visual.len(), 13);
        assert_eq!(visual[0].start, at("2020-07-14T12:01:35Z"));
        assert_eq!(exhausts[145].stop, at("2020-08-12T09:11:16Z"));
        let saturn = count("cassini-long-standing-reconnection");
        assert_eq!(saturn.len(), 17);
        assert_eq!(saturn[14].start, at("2006-03-02T07:00:00Z"));
        assert_eq!(saturn[14].stop, at("2006-03-07T12:00:00Z"), "125 h later");
        assert_eq!(saturn[14].attributes["reconnection_sites"], json!(19.0));
        assert_eq!(saturn[14].attributes["sector"], json!("periodical"));
    }

    #[test]
    fn an_icme_of_the_multi_catalogue_names_the_catalogue_it_came_from() {
        let items = icme_multicatalog(ICME_MULTI, "").unwrap();
        assert_eq!(items.len(), 3);
        let first = &items[0];
        assert_eq!(first.id, "2022-09-30T04:50:00Z|Wind|cat_1");
        assert_eq!(first.start, at("2022-09-30T04:50:00Z"));
        assert_eq!(first.stop, at("2022-10-01T14:04:00Z"));
        assert_eq!(first.attributes["mo_start"], json!("2022-09-30T08:28:00Z"));
        assert_eq!(first.attributes["spacecraft"], json!("Wind"));
        assert_eq!(first.attributes["sheath"], json!("yes"));
        assert_eq!(first.attributes["r_km"], json!(148950000.0));
        assert_eq!(
            first.attributes["source"],
            json!("Möstl et al. (2020), doi:10.3847/1538-4357/abb9a1")
        );
        assert!(!first.attributes.contains_key("v_aver_downS_48"));
        assert_eq!(items[1].attributes["spacecraft"], json!("Helios"));
        assert_eq!(items[2].attributes["spacecraft"], json!("Solar Orbiter"));
        assert_eq!(items[2].attributes["sheath"], json!("no"));
    }

    #[test]
    fn a_llamaice_cme_runs_from_its_first_to_its_last_boundary() {
        let items = llamaice(&format!("{LLAMAICE}{FILE_BREAK}{LLAMAICE_SOURCES}"), "").unwrap();
        assert_eq!(items.len(), 3);
        let first = &items[0];
        assert_eq!(first.attributes["bounds_from"], json!("LLAMAICE"));
        assert_eq!(
            first.attributes["catalogues"],
            json!("DREAMS, Jian, Lepping, Wind")
        );
        assert_eq!(first.id, "1");
        assert_eq!(first.start, at("1995-02-07T18:48:00Z"));
        assert_eq!(first.stop, at("1995-02-09T01:50:00Z"));
        assert_eq!(first.attributes["llamaice_event"], json!(1.0));
        assert_eq!(
            first.attributes["ejecta_start"],
            json!("1995-02-08T03:21:00Z")
        );
        assert_eq!(
            first.attributes["ejecta_end"],
            json!("1995-02-08T21:00:00Z")
        );
        assert_eq!(first.attributes["interacts_with"], json!(2.0));
        assert!(!first.attributes.contains_key("front_mixed_start"));
        assert!(!first.attributes.contains_key("hss"));
        assert!(
            first.attributes["comment"]
                .as_str()
                .unwrap()
                .starts_with("Y. M2")
        );
        assert_eq!(items[1].attributes["llamaice_event"], json!(3.0));
    }

    #[test]
    fn a_cme_the_llamaice_team_did_not_bound_spans_what_the_catalogues_said() {
        let items = llamaice(&format!("{LLAMAICE}{FILE_BREAK}{LLAMAICE_SOURCES}"), "").unwrap();
        let doubted = &items[2];
        assert_eq!(doubted.id, "16");
        assert_eq!(doubted.start, at("1995-10-24T07:45:00Z"));
        assert_eq!(doubted.stop, at("1995-10-25T08:04:00Z"));
        assert_eq!(doubted.attributes["bounds_from"], json!("catalogues"));
        assert_eq!(doubted.attributes["catalogues"], json!("Jian"));
        assert!(!doubted.attributes.contains_key("ejecta_start"));
        assert!(
            doubted.attributes["comment"]
                .as_str()
                .unwrap()
                .starts_with("PN.")
        );
    }

    #[test]
    fn a_catalogue_llamaice_collates_keeps_each_of_its_icmes_once() {
        let rc = llamaice_source(LLAMAICE_SOURCES, "RC").unwrap();
        assert_eq!(
            rc.len(),
            1,
            "an ICME matched to three LLAMAICE times stays once"
        );
        assert_eq!(rc[0].start, at("2002-03-23T11:37:00Z"));
        assert_eq!(rc[0].stop, at("2002-03-25T20:00:00Z"));
        assert_eq!(rc[0].attributes["llamaice_event"], json!(266.0));
        assert_eq!(
            rc[0].attributes["ejecta_start"],
            json!("2002-03-24T12:00:00Z")
        );
        assert_eq!(rc[0].attributes["cme_time"], json!("2002-03-20T17:06:00Z"));
        let jian = llamaice_source(LLAMAICE_SOURCES, "Jian").unwrap();
        assert_eq!(jian.len(), 2);
        assert_eq!(
            jian[0].start,
            at("1995-02-08T03:10:00Z"),
            "no sheath: the ejecta"
        );
        assert!(!jian[0].attributes.contains_key("sheath_start"));
        assert!(!jian[0].attributes.contains_key("cme_time"));
        assert_eq!(llamaice_source(LLAMAICE_SOURCES, "Wind").unwrap().len(), 2);
        assert!(
            llamaice_source(LLAMAICE_SOURCES, "CDAW")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn soda_shocks_are_moments_of_one_spacecraft_with_their_mach_numbers() {
        const SODA: &str = include_str!("../../tests/fixtures/relay/literature_soda.json");
        let psp = soda_shocks(SODA, "PSP").unwrap();
        assert_eq!(psp.len(), 1);
        assert_eq!(psp[0].id, "PSP_20181107_0750_8f");
        assert_eq!(psp[0].start, at("2018-11-07T07:50:19Z"));
        assert_eq!(psp[0].stop, psp[0].start);
        assert_eq!(psp[0].attributes["type"], json!("SR"));
        assert_eq!(psp[0].attributes["mach_alfven"], json!(0.67));
        assert_eq!(psp[0].attributes["mach_magnetosonic"], json!(0.45));
        assert_eq!(psp[0].attributes["distance_rs"], json!(36.7));
        let solo = soda_shocks(SODA, "SLO").unwrap();
        assert_eq!(solo.len(), 1);
        assert!(
            !solo[0].attributes.contains_key("mach_alfven"),
            "blank is no value"
        );
        let stereo = soda_shocks(SODA, "STA").unwrap();
        assert_eq!(stereo.len(), 2);
        let estimated = &stereo[1].attributes;
        assert!(
            estimated["mach_alfven"].is_number(),
            "the star is not part of the number"
        );
        assert_eq!(estimated["mach_alfven_estimated"], json!(true));
        assert!(!stereo[0].attributes.contains_key("mach_alfven_estimated"));
    }

    #[test]
    fn lassos_icmes_run_from_the_icme_start_to_its_end_and_a_zero_speed_is_none() {
        const LASSOS: &str =
            include_str!("../../tests/fixtures/relay/literature_lassos_messenger.json");
        let icmes = lassos_icmes(LASSOS, "").unwrap();
        assert_eq!(icmes.len(), 3);
        assert_eq!(icmes[0].id, "2007-05-04T13:34:00.000Z");
        assert_eq!(icmes[0].start, at("2007-05-04T13:34:00Z"));
        assert_eq!(icmes[0].stop, at("2007-05-06T00:57:00Z"));
        assert_eq!(
            icmes[0].attributes["mo_start"],
            json!("2007-05-04T20:57:00Z")
        );
        assert_eq!(icmes[0].attributes["mo_class"], json!("Fr"));
        assert_eq!(icmes[0].attributes["distance_au"], json!(0.57));
        assert_eq!(icmes[0].attributes["shock"], json!(false));
        assert!(!icmes[0].attributes.contains_key("mo_mean_speed_km_s"));
        assert!(!icmes[1].attributes.contains_key("mo_class"));

        const PARKER: &str =
            include_str!("../../tests/fixtures/relay/literature_lassos_parker.json");
        let parker = lassos_icmes(PARKER, "").unwrap();
        assert_ne!(parker[0].id, parker[2].id, "two ICMEs both named -");
    }

    #[test]
    fn the_kept_tables_of_the_november_batch_read_whole() {
        let read = |text| kept_table(text, "").unwrap();
        let counts = [
            (include_str!("data/persson_2023_venus_bow_shock.csv"), 5218),
            (
                include_str!("data/persson_2023_venus_ion_composition_boundary.csv"),
                2941,
            ),
            (
                include_str!("data/simon_wedlund_2022_mars_bow_shock.csv"),
                20107,
            ),
            (
                include_str!("data/koller_2025_themis_magnetosheath.csv"),
                38212,
            ),
            (
                include_str!("data/koller_2025_themis_jets_plaschke.csv"),
                10478,
            ),
            (
                include_str!("data/koller_2025_themis_jets_archer.csv"),
                14083,
            ),
            (
                include_str!("data/koller_2025_themis_jets_koller.csv"),
                9296,
            ),
            (include_str!("data/koller_2025_omni_sir_hss.csv"), 1022),
            (
                include_str!("data/martinovic_2021_psp_switchbacks.csv"),
                1074,
            ),
            (include_str!("data/papaioannou_2025_sep_30mev.csv"), 387),
            (include_str!("data/rotti_2022_gsep.csv"), 433),
            (include_str!("data/winslow_2015_mercury_icmes.csv"), 61),
            (include_str!("data/inglis_2024_rbspb_emic.csv"), 2678),
            (include_str!("data/remya_2024_rbsp_emic.csv"), 2480),
            (include_str!("data/hfc_type_iii_wind.csv"), 69117),
            (include_str!("data/hfc_type_iii_stereo_a.csv"), 12322),
            (include_str!("data/hfc_type_iii_stereo_b.csv"), 9406),
            (include_str!("data/allen_2021_psp_sirs.csv"), 11),
            (include_str!("data/allen_2021_stereo_a_sirs.csv"), 11),
            (include_str!("data/allen_2021_l1_sirs.csv"), 12),
            (
                include_str!("data/frey_2004_image_substorm_onsets.csv"),
                2437,
            ),
        ];
        for (text, count) in counts {
            assert_eq!(read(text).len(), count);
        }
        let emic = read(include_str!("data/inglis_2024_rbspb_emic.csv"));
        assert_eq!(
            emic[0].start,
            at("2013-01-14T04:46:24Z"),
            "the UT column, not the unix one 5 h ahead"
        );
        let mercury = read(include_str!("data/winslow_2015_mercury_icmes.csv"));
        assert_eq!(mercury[0].start, at("2011-05-19T11:50:02.005Z"));
        assert_eq!(mercury[0].attributes["source_uncertain"], json!("false"));
        let psp = read(include_str!("data/allen_2021_psp_sirs.csv"));
        assert_eq!(
            psp[1].attributes["max_pressure_time"],
            json!("2018-11-14T23:56:30Z")
        );
        assert!(!psp[0].attributes.contains_key("max_pressure_time"));
        let lists_of_batch = LISTS
            .iter()
            .filter(|list| matches!(list.files, Files::Kept(text) if counts.iter().any(|(kept, _)| *kept == text)))
            .count();
        assert_eq!(lists_of_batch, counts.len(), "every table has its list");
    }

    #[test]
    fn every_list_has_a_unique_key_and_says_whom_to_cite() {
        let mut keys: Vec<_> = lists().map(|list| list.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), lists().count());
        assert!(
            lists().all(|list| list.credit.contains("doi.org") || list.credit.contains("http"))
        );
    }
}

#[cfg(test)]
mod lpp_run_tests {
    use super::*;

    #[test]
    fn our_mms_region_lists_say_first_they_are_our_run() {
        let ours = LISTS
            .iter()
            .filter(|list| list.key.ends_with("-olshevsky-lpp"));
        assert_eq!(ours.clone().count(), 4);
        for list in ours {
            assert!(
                list.about
                    .starts_with("Our version, not the authors' catalogue")
            );
            assert!(list.about.contains("2026-11-16"));
        }
    }
}
