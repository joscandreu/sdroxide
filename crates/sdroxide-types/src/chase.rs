//! FT8/FT4/FT2 **chase**: answer, by itself, a station the log says is still
//! needed once it has been heard steadily for a while.
//!
//! Hunting a band opening by hand is watching the decode list for a call that
//! is new, waiting to see that it is a real, steady signal rather than one lucky
//! decode, and double-clicking it the moment it calls CQ. This is that, written
//! down: [`Chaser`] remembers which periods each station was decoded in, and
//! [`Chaser::pick`] names the best one to call from a slot that just arrived.
//!
//! It lives here, and runs on the *screen*, because the judgement it makes —
//! "not worked yet", "a new country", "a new grid" — is a question for the
//! logbook, and the logbook is the screen's ([`LogIndex`]), exactly as for the
//! audible alerts. The station only ever receives the decision, as
//! [`crate::Command::DigiChaseQso`], which it refuses unless its sequencer is
//! free and which never counts as the operator acting: the transmit watchdog
//! still stops a chase nobody is answering.
//!
//! The options follow what POTACAT's FT8 *Hunt* offers — what to hunt, a
//! minimum signal, a rest after a contact that went nowhere, a never-answer
//! list, an order of preference — and add the one thing that tool leaves out,
//! which is the point of the feature: a station has to have been **heard
//! consistently** before it is called.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::awards::{LogIndex, Novelty};
use crate::digi::{Decode, cq_is_for_us};
use crate::entity::resolve_callsign;
use crate::geo::grid_distance_km;

/// The lowest minimum signal the settings offer, which means "any signal at
/// all": nothing FT8 can decode is weaker than this.
pub const CHASE_ANY_SNR: i16 = -30;

/// How far away a station has to be to count as DX when either continent is
/// unknown. Matches the decode list's own stand-in for "another country".
const DX_FALLBACK_KM: f64 = 3000.0;

/// How long a station's receptions are remembered at most, whatever the
/// window, so a long session does not grow the table without bound.
const HEARD_KEEP_S: i64 = 60 * 60;

/// How long after a station last called us the chase still answers them, for
/// a call that arrived while the sequencer was busy. Longer than this and they
/// have most likely given up and gone elsewhere.
pub const CHASE_CALLER_KEEP_S: i64 = 120;

/// Which stations are worth chasing — the log's judgement, at the strictness
/// the operator asks for. A station already worked **on this band** is never
/// chased, whichever of these is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChaseNeed {
    /// A callsign never worked at all.
    NewCall,
    /// A grid square never worked (or a new country, which is rarer still).
    NewGrid,
    /// A DXCC entity not yet worked on this band.
    NewDxccBand,
    /// A DXCC entity never worked on any band.
    NewDxcc,
    /// Anyone not yet in the log on this band — the widest net.
    ///
    /// Last and `#[serde(other)]`, as [`crate::DecodeSort`] is, so a typo in a
    /// hand-edited config costs this one setting rather than the whole table.
    #[default]
    #[serde(other)]
    NotOnBand,
}

impl ChaseNeed {
    pub const ALL: [ChaseNeed; 5] = [
        ChaseNeed::NotOnBand,
        ChaseNeed::NewCall,
        ChaseNeed::NewGrid,
        ChaseNeed::NewDxccBand,
        ChaseNeed::NewDxcc,
    ];

    /// Whether a station this new is wanted.
    pub fn wants(self, n: Novelty) -> bool {
        if n.dupe {
            return false;
        }
        match self {
            ChaseNeed::NotOnBand => true,
            ChaseNeed::NewCall => n.new_call,
            ChaseNeed::NewGrid => n.new_grid || n.new_dxcc,
            ChaseNeed::NewDxccBand => n.new_dxcc || n.new_dxcc_band,
            ChaseNeed::NewDxcc => n.new_dxcc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ChaseNeed::NotOnBand => "Not worked on this band",
            ChaseNeed::NewCall => "New callsign",
            ChaseNeed::NewGrid => "New grid",
            ChaseNeed::NewDxccBand => "New DXCC on this band",
            ChaseNeed::NewDxcc => "New DXCC",
        }
    }
}

/// Which of a wanted station's messages the chase answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChaseAnswer {
    /// Their CQ, or the RR73 / 73 that ends their contact with somebody else:
    /// calling the moment they sign off (tail-ending) is how a DX station
    /// working a steady run is usually caught.
    CqOrSignoff,
    /// Anything they send. A station mid-exchange with somebody else is called
    /// the way the decode list's REPLY calls one: held silently until they call
    /// CQ or finish, then answered — never transmitted over.
    Any,
    /// Their CQ — and only a CQ open to us: a `CQ DX` from our own entity or a
    /// `CQ NA` from the wrong continent is left alone.
    ///
    /// Last and `#[serde(other)]` for the reason [`ChaseNeed::NotOnBand`] is.
    #[default]
    #[serde(other)]
    Cq,
}

impl ChaseAnswer {
    pub const ALL: [ChaseAnswer; 3] = [ChaseAnswer::Cq, ChaseAnswer::CqOrSignoff, ChaseAnswer::Any];

    pub fn label(self) -> &'static str {
        match self {
            ChaseAnswer::Cq => "CQ only",
            ChaseAnswer::CqOrSignoff => "CQ or sign-off",
            ChaseAnswer::Any => "Any message",
        }
    }
}

/// Which of several stations eligible in the same slot is called first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChaseOrder {
    /// The strongest signal: the one most likely to complete.
    Strongest,
    /// The farthest, by grid.
    Farthest,
    /// The rarest for the log — a watched call, then a new country, a country
    /// new on this band, a new grid, a new call — and the strongest among
    /// equals.
    #[default]
    #[serde(other)]
    Needed,
}

impl ChaseOrder {
    pub const ALL: [ChaseOrder; 3] =
        [ChaseOrder::Needed, ChaseOrder::Strongest, ChaseOrder::Farthest];

    pub fn label(self) -> &'static str {
        match self {
            ChaseOrder::Needed => "Rarest first",
            ChaseOrder::Strongest => "Strongest first",
            ChaseOrder::Farthest => "Farthest first",
        }
    }
}

/// What counts as already worked — a dupe — when the log is asked about a
/// station. A dupe is never chased, and when one calls us it is answered only
/// after everybody new.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ChaseDupe {
    /// Worked on this band in any data mode: an FT4 contact covers FT8, a
    /// phone or CW one does not.
    DataModes,
    /// Worked on this band in any mode at all.
    Band,
    /// Worked on this band in this very mode.
    ///
    /// Last and `#[serde(other)]` for the reason [`ChaseNeed::NotOnBand`] is.
    #[default]
    #[serde(other)]
    BandMode,
}

impl ChaseDupe {
    pub const ALL: [ChaseDupe; 3] = [ChaseDupe::BandMode, ChaseDupe::DataModes, ChaseDupe::Band];

    pub fn label(self) -> &'static str {
        match self {
            ChaseDupe::BandMode => "Same band and mode",
            ChaseDupe::DataModes => "Same band, any data mode",
            ChaseDupe::Band => "Same band, any mode",
        }
    }
}

/// The CQ words that open a contest with an exchange of its own, which the
/// everyday reply a chase sends would only garble: ARRL Field Day, RTTY
/// Roundup, WW Digi and the VHF contests' `CQ TEST`.
const CONTEST_CQ: [&str; 4] = ["TEST", "FD", "RU", "WW"];

/// The default for [`ChaseSettings::max_calls`].
pub const CHASE_MAX_CALLS: u8 = 4;

/// The operator's chase options. A screen's preference, like the alerts: it is
/// the screen that holds the log the judgement is made against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChaseSettings {
    /// Chasing right now. **Never remembered**: an automatic transmitter that
    /// switched itself back on when the program started is an on-air behaviour
    /// nobody consented to that day.
    #[serde(skip)]
    pub enabled: bool,
    /// Which stations are wanted.
    pub need: ChaseNeed,
    /// Which of their messages is answered.
    pub answer: ChaseAnswer,
    /// How many different periods a station must have been decoded in, within
    /// [`window_min`](Self::window_min), before it is called. This is "heard
    /// consistently": one lucky decode of a station at the noise floor is not
    /// a contact waiting to happen. 1 calls on first sight.
    pub min_heard: u8,
    /// The span, in minutes, those receptions are counted over.
    pub window_min: u8,
    /// Leave alone a station weaker than this, in dB.
    /// [`CHASE_ANY_SNR`] means any signal.
    pub min_snr_db: i16,
    /// Only stations on another continent.
    pub dx_only: bool,
    /// Once a chased contact goes nowhere — the unanswered-call count gave up,
    /// or the operator stopped it — leave that station alone on this band for
    /// this many minutes. 0 lets it be called again straight away.
    pub retry_after_min: u16,
    /// Which eligible station goes first.
    pub order: ChaseOrder,
    /// What counts as already worked.
    pub dupe: ChaseDupe,
    /// Give up on a chased station after calling it this many times with no
    /// reply — sooner than the sequencer's own unanswered-call count, which
    /// is set for a contact the operator is minding, so the chase moves on to
    /// the next station while it is still being heard.
    pub max_calls: u8,
    /// Never chase these. A callsign, or a prefix ending in `*` (`VE3*`).
    pub ignore: Vec<String>,
    /// Always worth chasing, whatever [`need`](Self::need) says — though still
    /// not as a dupe on this band. Same pattern rules as `ignore`.
    pub watch: Vec<String>,
}

impl Default for ChaseSettings {
    fn default() -> Self {
        ChaseSettings {
            enabled: false,
            need: ChaseNeed::NotOnBand,
            answer: ChaseAnswer::Cq,
            min_heard: 3,
            window_min: 5,
            min_snr_db: CHASE_ANY_SNR,
            dx_only: false,
            retry_after_min: 30,
            order: ChaseOrder::Needed,
            dupe: ChaseDupe::BandMode,
            max_calls: CHASE_MAX_CALLS,
            ignore: Vec::new(),
            watch: Vec::new(),
        }
    }
}

/// True when `call` matches one of `patterns`: a callsign exactly, or a prefix
/// written with a trailing `*`. Case does not matter.
pub fn chase_list_matches(patterns: &[String], call: &str) -> bool {
    let call = call.trim().to_ascii_uppercase();
    patterns.iter().any(|p| {
        let p = p.trim().to_ascii_uppercase();
        match p.strip_suffix('*') {
            Some(prefix) => !prefix.is_empty() && call.starts_with(prefix),
            None => !p.is_empty() && p == call,
        }
    })
}

/// Split a list typed as free text — commas, spaces or new lines — into its
/// entries.
pub fn parse_chase_list(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Everything about the operator the chase needs to judge a station.
#[derive(Debug, Clone, Copy)]
pub struct ChaseCtx<'a> {
    pub my_call: &'a str,
    pub my_grid: &'a str,
    /// The ADIF band being operated (`"20m"`), what "worked on this band"
    /// means.
    pub band: &'a str,
    /// The ADIF mode being operated (`"FT8"`), for [`ChaseDupe`]. Empty asks
    /// by band alone.
    pub mode: &'a str,
    /// The CQ word of the contest the station is set up for (`"TEST"`), or
    /// empty outside one: the one contest whose stations may be answered.
    pub contest: &'a str,
    /// Unix seconds now — in practice the start of the slot just decoded.
    pub now: i64,
}

/// What the chase makes of one heard station — the answer to "why is it not
/// calling anybody?", one station at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChaseVerdict {
    /// Ready to be called.
    Eligible,
    /// On the never-answer list.
    Ignored,
    /// A directed CQ that is not for us, or not DX when only DX is wanted.
    NotForUs,
    /// Already worked, or not new in the way that is wanted.
    NotNeeded,
    /// Chased lately without a contact; resting.
    Resting,
    /// Weaker than the minimum signal.
    Weak,
    /// Not heard steadily enough yet: decoded in this many periods so far.
    Building(u8),
    /// Heard steadily, but their last message is not one that is answered —
    /// in the middle of a contact with somebody else, say.
    Busy,
}

/// One station the chase has decided to call.
#[derive(Debug, Clone, PartialEq)]
pub struct ChasePick {
    pub call: String,
    pub grid: Option<String>,
    /// Their signal at us: the report we will send.
    pub snr_db: i16,
    pub audio_hz: f32,
    /// Hold until they are free (see [`ChaseAnswer::Any`]).
    pub wait_for_cq: bool,
    /// Periods they were heard in, within the window.
    pub heard: u8,
    pub novelty: Novelty,
}

/// Counts over every station heard within the window, for the one-line
/// "what is the chase doing" readout.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChaseTally {
    pub eligible: usize,
    pub building: usize,
    pub not_needed: usize,
    pub weak: usize,
    pub busy: usize,
    pub resting: usize,
    pub excluded: usize,
    /// The station nearest to being called: the eligible one that would go
    /// first, or else the one heard most often so far, with its count.
    pub next: Option<(String, u8)>,
}

#[derive(Debug, Clone)]
struct Heard {
    /// Start times of the periods the station was decoded in, oldest first,
    /// each once.
    slots: Vec<i64>,
    /// Their most recent decode.
    last: Decode,
    /// The last grid they sent. A CQ carries one and the rest of a contact
    /// does not, and the novelty of a station is the same either way.
    grid: Option<String>,
    /// The contest word of their latest CQ (`"FD"`), or `None` once they call
    /// an ordinary one. What says a station is in a contest even while its
    /// latest message is an exchange or a sign-off.
    contest: Option<String>,
}

/// The memory behind a chase: who has been heard in which periods on this band,
/// and who was chased lately.
#[derive(Debug, Clone, Default)]
pub struct Chaser {
    band: String,
    heard: HashMap<String, Heard>,
    /// When each station was last chased on each band (Unix s).
    tried: HashMap<(String, String), i64>,
    /// When the operator last skipped each station on each band (Unix s). A
    /// rest of the same length as `tried`, but one that even the station
    /// answering us does not lift: the operator said no.
    skipped: HashMap<(String, String), i64>,
}

/// A decoded callsign in the form the chase keys on, or `None` for one it
/// cannot call: a hash it never resolved (`<...>`), or nothing at all.
fn clean_call(c: &str) -> Option<String> {
    let c = c.trim().trim_start_matches('<').trim_end_matches('>').to_ascii_uppercase();
    (!c.is_empty() && !c.contains('.')).then_some(c)
}

/// True for a message carrying a signal report (`-12`, `R+05`): a station
/// answering a call, as opposed to opening one with a grid or bare calls.
fn is_report(d: &Decode) -> bool {
    let Some(t) = d.message.split_whitespace().last() else { return false };
    let t = t.strip_prefix('R').unwrap_or(t);
    t.len() == 3
        && matches!(t.as_bytes()[0], b'+' | b'-')
        && t[1..].bytes().all(|b| b.is_ascii_digit())
}

/// True for a message that closes a contact with somebody else: the moment
/// that station is about to be free.
fn is_signoff(d: &Decode) -> bool {
    matches!(d.message.split_whitespace().last(), Some("RR73" | "73" | "RRR"))
}

/// True when `call` is on another continent from ours. Unknown either side
/// falls back to the distance between the grids, and with neither it is not
/// DX — the safe answer for something that decides on its own to transmit.
fn is_dx(call: &str, grid: Option<&str>, my_call: &str, my_grid: &str) -> bool {
    match (resolve_callsign(call), resolve_callsign(my_call)) {
        (Some(theirs), Some(mine)) => theirs.continent != mine.continent,
        _ => grid.and_then(|g| grid_distance_km(my_grid, g)).is_some_and(|km| km >= DX_FALLBACK_KM),
    }
}

impl Chaser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold in one batch of decodes heard on `band`. A change of band forgets
    /// who was heard — a steady signal on 20 m says nothing about 15 m — but
    /// not who was chased, which is kept per band.
    pub fn observe(&mut self, decodes: &[Decode], band: &str) {
        if band != self.band {
            self.band = band.to_string();
            self.heard.clear();
        }
        let mut newest = i64::MIN;
        for d in decodes {
            newest = newest.max(d.slot_utc);
            if d.free_text {
                continue;
            }
            let Some(call) = d.from.as_deref().and_then(clean_call) else { continue };
            let h = self.heard.entry(call).or_insert_with(|| Heard {
                slots: Vec::new(),
                last: d.clone(),
                grid: None,
                contest: None,
            });
            if !h.slots.contains(&d.slot_utc) {
                h.slots.push(d.slot_utc);
                h.slots.sort_unstable();
            }
            if d.slot_utc >= h.last.slot_utc {
                h.last = d.clone();
                if d.is_cq {
                    h.contest = d
                        .cq_to
                        .as_deref()
                        .map(str::to_ascii_uppercase)
                        .filter(|w| CONTEST_CQ.contains(&w.as_str()));
                }
            }
            if let Some(g) = d.grid.as_deref().filter(|g| !g.is_empty()) {
                h.grid = Some(g.to_string());
            }
        }
        if newest != i64::MIN {
            self.heard.retain(|_, h| {
                h.slots.retain(|t| newest - *t <= HEARD_KEEP_S);
                !h.slots.is_empty()
            });
        }
    }

    /// Note that `call` was chased on `band` at `now`, so a contact that goes
    /// nowhere is not started again before
    /// [`ChaseSettings::retry_after_min`] is up.
    pub fn mark_tried(&mut self, call: &str, band: &str, now: i64) {
        self.tried.insert((call.trim().to_ascii_uppercase(), band.to_string()), now);
    }

    /// Note that the operator skipped `call` on `band` at `now`: left alone,
    /// whatever it sends, for [`ChaseSettings::retry_after_min`].
    pub fn mark_skipped(&mut self, call: &str, band: &str, now: i64) {
        let key = (call.trim().to_ascii_uppercase(), band.to_string());
        self.tried.insert(key.clone(), now);
        self.skipped.insert(key, now);
    }

    /// True when `call` was chased (`tried`) or skipped (`skipped`) on this
    /// band less than the rest ago.
    fn resting_in(
        map: &HashMap<(String, String), i64>,
        call: &str,
        s: &ChaseSettings,
        ctx: &ChaseCtx,
    ) -> bool {
        let retry = i64::from(s.retry_after_min) * 60;
        map.get(&(call.to_string(), ctx.band.to_string())).is_some_and(|t| ctx.now - *t < retry)
    }

    /// Forget everything: who was heard and who was chased.
    pub fn clear(&mut self) {
        self.heard.clear();
        self.tried.clear();
        self.skipped.clear();
    }

    /// Periods `call` was decoded in within the window ending at `now`.
    fn heard_count(&self, h: &Heard, s: &ChaseSettings, now: i64) -> u8 {
        let window = i64::from(s.window_min.max(1)) * 60;
        h.slots.iter().filter(|t| now - **t < window && **t <= now).count().min(255) as u8
    }

    /// What the log makes of `call`, with a dupe judged the way the settings
    /// ask: the index answers "worked on this band" in any mode, and the
    /// narrower questions are put to it only when that is yes.
    fn novelty(
        &self,
        call: &str,
        grid: Option<&str>,
        s: &ChaseSettings,
        log: &LogIndex,
        ctx: &ChaseCtx,
    ) -> Novelty {
        let mut n = log.novelty(call, grid, ctx.band);
        if n.dupe && !ctx.mode.is_empty() {
            n.dupe = match s.dupe {
                ChaseDupe::Band => true,
                ChaseDupe::BandMode => log.worked_band_mode(call, ctx.band, ctx.mode),
                ChaseDupe::DataModes => log.worked_band_class(call, ctx.band, ctx.mode),
            };
        }
        n
    }

    /// True when `h` is working a contest other than the one we are set up
    /// for. Its exchange is not ours, and the everyday reply would garble it.
    fn in_other_contest(h: &Heard, ctx: &ChaseCtx) -> bool {
        h.contest.as_deref().is_some_and(|w| !w.eq_ignore_ascii_case(ctx.contest.trim()))
    }

    /// Judge one heard station against the settings and the log.
    fn verdict(
        &self,
        call: &str,
        h: &Heard,
        s: &ChaseSettings,
        log: &LogIndex,
        ctx: &ChaseCtx,
    ) -> (ChaseVerdict, Novelty) {
        let d = &h.last;
        let my_call = ctx.my_call.trim().to_ascii_uppercase();
        let novelty = self.novelty(call, h.grid.as_deref(), s, log, ctx);
        if call == my_call || chase_list_matches(&s.ignore, call) {
            return (ChaseVerdict::Ignored, novelty);
        }
        if (d.is_cq && !cq_is_for_us(d, ctx.my_call, ctx.my_grid))
            || Self::in_other_contest(h, ctx)
            || (s.dx_only && !is_dx(call, h.grid.as_deref(), ctx.my_call, ctx.my_grid))
        {
            return (ChaseVerdict::NotForUs, novelty);
        }
        let watched = chase_list_matches(&s.watch, call) && !novelty.dupe;
        if !watched && !s.need.wants(novelty) {
            return (ChaseVerdict::NotNeeded, novelty);
        }
        if Self::resting_in(&self.tried, call, s, ctx) {
            return (ChaseVerdict::Resting, novelty);
        }
        if s.min_snr_db > CHASE_ANY_SNR && d.snr_db < s.min_snr_db {
            return (ChaseVerdict::Weak, novelty);
        }
        let n = self.heard_count(h, s, ctx.now);
        if n < s.min_heard.max(1) {
            return (ChaseVerdict::Building(n), novelty);
        }
        let to_me = d.to.as_deref().and_then(clean_call).is_some_and(|t| t == my_call);
        let answerable = match s.answer {
            ChaseAnswer::Cq => d.is_cq,
            ChaseAnswer::CqOrSignoff => d.is_cq || (is_signoff(d) && !to_me),
            ChaseAnswer::Any => true,
        };
        if !answerable {
            return (ChaseVerdict::Busy, novelty);
        }
        (ChaseVerdict::Eligible, novelty)
    }

    /// Ordering key: larger goes first.
    fn rank(
        &self,
        call: &str,
        h: &Heard,
        n: Novelty,
        s: &ChaseSettings,
        ctx: &ChaseCtx,
    ) -> (i64, i64) {
        let snr = i64::from(h.last.snr_db);
        let rarity = if chase_list_matches(&s.watch, call) {
            5
        } else if n.new_dxcc {
            4
        } else if n.new_dxcc_band {
            3
        } else if n.new_grid {
            2
        } else if n.new_call {
            1
        } else {
            0
        };
        match s.order {
            ChaseOrder::Needed => (rarity, snr),
            ChaseOrder::Strongest => (snr, rarity),
            ChaseOrder::Farthest => {
                let km =
                    h.grid.as_deref().and_then(|g| grid_distance_km(ctx.my_grid, g)).unwrap_or(0.0);
                (km as i64, snr)
            }
        }
    }

    /// The station to call now, from the slot just decoded, or `None`.
    ///
    /// Only stations decoded in `batch` are candidates: answering somebody
    /// means transmitting in the period after theirs, so the moment to call is
    /// right after hearing them, and a station last heard five minutes ago may
    /// have gone. Call [`observe`](Self::observe) with the same batch first.
    pub fn pick(
        &self,
        batch: &[Decode],
        s: &ChaseSettings,
        log: &LogIndex,
        ctx: &ChaseCtx,
    ) -> Option<ChasePick> {
        let mut best: Option<((i64, i64), ChasePick)> = None;
        for d in batch {
            let Some(call) = d.from.as_deref().and_then(clean_call) else { continue };
            let Some(h) = self.heard.get(&call) else { continue };
            // Their latest message is the one being answered; an older decode
            // that arrived late in a streamed batch does not speak for them.
            if h.last.slot_utc != d.slot_utc || h.last.message != d.message {
                continue;
            }
            let (verdict, novelty) = self.verdict(&call, h, s, log, ctx);
            if verdict != ChaseVerdict::Eligible {
                continue;
            }
            let my_call = ctx.my_call.trim().to_ascii_uppercase();
            let to_me = d.to.as_deref().and_then(clean_call).is_some_and(|t| t == my_call);
            let free = d.is_cq || to_me || is_signoff(d);
            let key = self.rank(&call, h, novelty, s, ctx);
            if best.as_ref().is_none_or(|(k, _)| key > *k) {
                best = Some((
                    key,
                    ChasePick {
                        call: call.clone(),
                        grid: h.grid.clone(),
                        snr_db: d.snr_db,
                        audio_hz: d.audio_hz,
                        wait_for_cq: !free,
                        heard: self.heard_count(h, s, ctx.now),
                        novelty,
                    },
                ));
            }
        }
        best.map(|(_, p)| p)
    }

    /// A station calling **us** to answer now, or `None`.
    ///
    /// Somebody calling us is the contact the chase is for, already half made,
    /// so none of the chase's judgement of a station applies — not the need,
    /// not the signal, not how often they were heard, not even a dupe on the
    /// band: they asked. Only the operator's never-answer list and the rest
    /// after a chase that went nowhere hold — and that rest not for a station
    /// sending us a report, which is answering a call of ours: late, or after
    /// the chase moved on to somebody else, but a contact all the same. A
    /// station the operator skipped stays skipped. Their latest message must be
    /// addressed to us, sent within [`CHASE_CALLER_KEEP_S`], and not a
    /// sign-off; a station that has since called CQ or gone to somebody else
    /// has moved on. `busy_with` — the station the sequencer has in hand — is
    /// left to the sequencer.
    ///
    /// Who goes first: a station already in the log on this band only after
    /// every one that is not — answering it costs a contact that adds nothing
    /// — then the newest call, then the order setting (strongest, rarest or
    /// farthest) between calls from the same period.
    pub fn pick_caller(
        &self,
        s: &ChaseSettings,
        log: &LogIndex,
        ctx: &ChaseCtx,
        busy_with: Option<&str>,
    ) -> Option<ChasePick> {
        let my_call = ctx.my_call.trim().to_ascii_uppercase();
        if my_call.is_empty() {
            return None;
        }
        let busy = busy_with.and_then(clean_call);
        self.heard
            .iter()
            .filter(|(call, h)| {
                let d = &h.last;
                d.to.as_deref().and_then(clean_call).is_some_and(|t| t == my_call)
                    && !d.is_cq
                    && !is_signoff(d)
                    && ctx.now - d.slot_utc <= CHASE_CALLER_KEEP_S
                    && **call != my_call
                    && busy.as_deref() != Some(call.as_str())
                    && !chase_list_matches(&s.ignore, call)
                    && !Self::in_other_contest(h, ctx)
                    && !Self::resting_in(&self.skipped, call, s, ctx)
                    && (is_report(d) || !Self::resting_in(&self.tried, call, s, ctx))
            })
            .map(|(call, h)| (call, h, self.novelty(call, h.grid.as_deref(), s, log, ctx)))
            .max_by_key(|(call, h, n)| {
                (
                    !n.dupe,
                    h.last.slot_utc,
                    self.rank(call, h, *n, s, ctx),
                    std::cmp::Reverse((*call).clone()),
                )
            })
            .map(|(call, h, novelty)| ChasePick {
                call: call.clone(),
                grid: h.grid.clone(),
                snr_db: h.last.snr_db,
                audio_hz: h.last.audio_hz,
                wait_for_cq: false,
                heard: self.heard_count(h, s, ctx.now),
                novelty,
            })
    }

    /// Counts over every station heard within the window — what the readout
    /// shows while nothing is being called.
    pub fn tally(&self, s: &ChaseSettings, log: &LogIndex, ctx: &ChaseCtx) -> ChaseTally {
        let mut t = ChaseTally::default();
        let window = i64::from(s.window_min.max(1)) * 60;
        let mut best: Option<((i64, i64), String, u8)> = None;
        let mut most: Option<(u8, i16, String)> = None;
        for (call, h) in &self.heard {
            if ctx.now - h.last.slot_utc >= window {
                continue;
            }
            let (verdict, novelty) = self.verdict(call, h, s, log, ctx);
            match verdict {
                ChaseVerdict::Eligible => {
                    t.eligible += 1;
                    let key = self.rank(call, h, novelty, s, ctx);
                    if best.as_ref().is_none_or(|(k, _, _)| key > *k) {
                        best = Some((key, call.clone(), self.heard_count(h, s, ctx.now)));
                    }
                }
                ChaseVerdict::Building(n) => {
                    t.building += 1;
                    let cand = (n, h.last.snr_db, call.clone());
                    if most.as_ref().is_none_or(|m| (cand.0, cand.1) > (m.0, m.1)) {
                        most = Some(cand);
                    }
                }
                ChaseVerdict::NotNeeded => t.not_needed += 1,
                ChaseVerdict::Weak => t.weak += 1,
                ChaseVerdict::Busy => t.busy += 1,
                ChaseVerdict::Resting => t.resting += 1,
                ChaseVerdict::Ignored | ChaseVerdict::NotForUs => t.excluded += 1,
            }
        }
        t.next = best.map(|(_, c, n)| (c, n)).or(most.map(|(n, _, c)| (c, n)));
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QsoRecord;

    const ME: &str = "EA4ABC";
    const GRID: &str = "IN80";

    fn ctx(now: i64) -> ChaseCtx<'static> {
        ChaseCtx { my_call: ME, my_grid: GRID, band: "20m", mode: "FT8", contest: "", now }
    }

    fn dec(slot: i64, msg: &str, snr: i16) -> Decode {
        let toks: Vec<&str> = msg.split_whitespace().collect();
        let is_cq = toks.first() == Some(&"CQ");
        let (to, from, cq_to) = if is_cq {
            if toks.len() == 4 {
                (None, Some(toks[2].to_string()), Some(toks[1].to_string()))
            } else {
                (None, toks.get(1).map(|s| s.to_string()), None)
            }
        } else {
            (toks.first().map(|s| s.to_string()), toks.get(1).map(|s| s.to_string()), None)
        };
        let grid = toks
            .last()
            .filter(|t| {
                let b = t.as_bytes();
                **t != "RR73"
                    && b.len() == 4
                    && b[0].is_ascii_uppercase()
                    && b[1].is_ascii_uppercase()
                    && b[2].is_ascii_digit()
                    && b[3].is_ascii_digit()
            })
            .map(|s| s.to_string());
        Decode {
            slot_utc: slot,
            snr_db: snr,
            dt: 0.1,
            audio_hz: 1200.0,
            message: msg.to_string(),
            to,
            from,
            grid,
            is_cq,
            cq_to,
            free_text: false,
            rr73_to: None,
        }
    }

    fn log_of(calls: &[(&str, &str)]) -> LogIndex {
        let recs: Vec<QsoRecord> = calls
            .iter()
            .map(|(c, b)| QsoRecord {
                call: c.to_string(),
                band: b.to_string(),
                mode: "FT8".into(),
                ..Default::default()
            })
            .collect();
        LogIndex::build(&recs)
    }

    /// Feed `msg` from the same station in `n` periods, a minute apart (every
    /// other 15 s slot is 30 s; a minute keeps the arithmetic obvious).
    fn hear(ch: &mut Chaser, msg: &str, snr: i16, slots: &[i64]) -> Vec<Decode> {
        let mut last = Vec::new();
        for &t in slots {
            last = vec![dec(t, msg, snr)];
            ch.observe(&last, "20m");
        }
        last
    }

    #[test]
    fn a_station_is_called_only_once_it_has_been_heard_steadily() {
        let s = ChaseSettings::default(); // three periods in five minutes
        let log = LogIndex::default();
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[0, 30]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(30)), None, "twice is not yet steady");
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[60]);
        let p = ch.pick(&b, &s, &log, &ctx(60)).expect("three times is");
        assert_eq!(p.call, "JA1XYZ");
        assert_eq!(p.grid.as_deref(), Some("PM95"));
        assert_eq!(p.heard, 3);
        assert!(!p.wait_for_cq);
    }

    #[test]
    fn receptions_outside_the_window_do_not_count() {
        let s = ChaseSettings { window_min: 2, ..Default::default() };
        let log = LogIndex::default();
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[0, 300, 330]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(330)), None, "the first was five minutes ago");
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[360]);
        assert!(ch.pick(&b, &s, &log, &ctx(360)).is_some());
    }

    #[test]
    fn a_station_worked_on_this_band_is_never_chased() {
        let s = ChaseSettings::default();
        let log = log_of(&[("JA1XYZ", "20m")]);
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[0, 30, 60]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(60)), None);
        // Watching a call does not make a dupe worth calling either.
        let s = ChaseSettings { watch: vec!["JA1XYZ".into()], ..Default::default() };
        assert_eq!(ch.pick(&b, &s, &log, &ctx(60)), None);
        // Worked on another band, it is wanted here.
        let log = log_of(&[("JA1XYZ", "40m")]);
        assert!(ch.pick(&b, &ChaseSettings::default(), &log, &ctx(60)).is_some());
    }

    #[test]
    fn the_need_setting_narrows_who_is_wanted() {
        // Japan worked already, on 20 m, by another station.
        let log = log_of(&[("JA2AAA", "20m")]);
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[0, 30, 60]);
        let want = |need| {
            let s = ChaseSettings { need, ..Default::default() };
            ch.pick(&b, &s, &log, &ctx(60)).is_some()
        };
        assert!(want(ChaseNeed::NotOnBand));
        assert!(want(ChaseNeed::NewCall));
        assert!(want(ChaseNeed::NewGrid), "the log has no grid for JA2AAA");
        assert!(!want(ChaseNeed::NewDxccBand), "Japan is in the log on 20 m");
        assert!(!want(ChaseNeed::NewDxcc));
    }

    #[test]
    fn the_watch_list_overrides_the_need() {
        let log = log_of(&[("JA2AAA", "20m")]);
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[0, 30, 60]);
        let s = ChaseSettings {
            need: ChaseNeed::NewDxcc,
            watch: vec!["JA1*".into()],
            ..Default::default()
        };
        assert!(ch.pick(&b, &s, &log, &ctx(60)).is_some());
    }

    #[test]
    fn ignored_and_weak_stations_are_left_alone() {
        let log = LogIndex::default();
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -21, &[0, 30, 60]);
        let s = ChaseSettings { ignore: vec!["ja*".into()], ..Default::default() };
        assert_eq!(ch.pick(&b, &s, &log, &ctx(60)), None);
        let s = ChaseSettings { min_snr_db: -18, ..Default::default() };
        assert_eq!(ch.pick(&b, &s, &log, &ctx(60)), None);
        let s = ChaseSettings { min_snr_db: -22, ..Default::default() };
        assert!(ch.pick(&b, &s, &log, &ctx(60)).is_some());
    }

    #[test]
    fn a_directed_cq_for_somebody_else_is_not_answered() {
        let log = LogIndex::default();
        let s = ChaseSettings::default();
        let mut ch = Chaser::new();
        // A Spanish station calling CQ DX: we are in Spain too.
        let b = hear(&mut ch, "CQ DX EA8ZZZ IL18", -5, &[0, 30, 60]);
        // EA8 is the Canaries, its own entity — so that one is for us.
        assert!(ch.pick(&b, &s, &log, &ctx(60)).is_some());
        let b = hear(&mut ch, "CQ DX EA1ZZZ IN73", -5, &[0, 30, 60]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(60)), None);
        let b = hear(&mut ch, "CQ NA W1AW FN31", -5, &[0, 30, 60]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(60)), None);
    }

    #[test]
    fn dx_only_wants_another_continent() {
        let log = LogIndex::default();
        let s = ChaseSettings { dx_only: true, ..Default::default() };
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ DL1ABC JO62", -5, &[0, 30, 60]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(60)), None, "Germany is in Europe");
        let b = hear(&mut ch, "CQ W1AW FN31", -5, &[0, 30, 60]);
        assert!(ch.pick(&b, &s, &log, &ctx(60)).is_some());
    }

    #[test]
    fn what_is_answered_follows_the_answer_setting() {
        let log = LogIndex::default();
        let mut ch = Chaser::new();
        hear(&mut ch, "CQ JA1XYZ PM95", -12, &[0, 30]);
        // Now in a contact with somebody else, then signing off.
        let busy = hear(&mut ch, "W1AW JA1XYZ -05", -12, &[60]);
        let pick = |ch: &Chaser, b: &[Decode], answer, now| {
            let s = ChaseSettings { answer, ..Default::default() };
            ch.pick(b, &s, &log, &ctx(now))
        };
        assert_eq!(pick(&ch, &busy, ChaseAnswer::Cq, 60), None);
        assert_eq!(pick(&ch, &busy, ChaseAnswer::CqOrSignoff, 60), None);
        let held = pick(&ch, &busy, ChaseAnswer::Any, 60).expect("any message");
        assert!(held.wait_for_cq, "called mid-exchange, so held until they are free");

        let bye = hear(&mut ch, "W1AW JA1XYZ RR73", -12, &[90]);
        assert_eq!(pick(&ch, &bye, ChaseAnswer::Cq, 90), None);
        let tail = pick(&ch, &bye, ChaseAnswer::CqOrSignoff, 90).expect("tail-ending");
        assert!(!tail.wait_for_cq, "they are free the moment they sign off");
    }

    #[test]
    fn a_station_chased_lately_rests() {
        let log = LogIndex::default();
        let s = ChaseSettings::default();
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[0, 30, 60]);
        assert!(ch.pick(&b, &s, &log, &ctx(60)).is_some());
        ch.mark_tried("JA1XYZ", "20m", 60);
        // Ten minutes on, heard as steadily as before: still resting.
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[540, 570, 600]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(600)), None);
        assert_eq!(ch.tally(&s, &log, &ctx(600)).resting, 1);
        // Half an hour after the try, fair game again.
        let b = hear(&mut ch, "CQ JA1XYZ PM95", -12, &[1800, 1830, 1860]);
        assert!(ch.pick(&b, &s, &log, &ctx(1860)).is_some());
        // And on another band the rest never applied.
        ch.mark_tried("JA1XYZ", "20m", 1860);
        let b = vec![dec(1890, "CQ JA1XYZ PM95", -12)];
        let s1 = ChaseSettings { min_heard: 1, ..Default::default() };
        ch.observe(&b, "15m");
        let c = ChaseCtx { band: "15m", ..ctx(1890) };
        assert!(ch.pick(&b, &s1, &log, &c).is_some());
    }

    #[test]
    fn the_order_setting_decides_between_eligible_stations() {
        // Japan already worked; Brazil is new.
        let log = log_of(&[("JA2AAA", "20m")]);
        let mut ch = Chaser::new();
        for t in [0, 30, 60] {
            ch.observe(&[dec(t, "CQ JA1XYZ PM95", -2), dec(t, "CQ PY2ABC GG66", -15)], "20m");
        }
        let batch = vec![dec(60, "CQ JA1XYZ PM95", -2), dec(60, "CQ PY2ABC GG66", -15)];
        let first = |order| {
            let s = ChaseSettings { order, ..Default::default() };
            ch.pick(&batch, &s, &log, &ctx(60)).map(|p| p.call)
        };
        assert_eq!(first(ChaseOrder::Needed).as_deref(), Some("PY2ABC"));
        assert_eq!(first(ChaseOrder::Strongest).as_deref(), Some("JA1XYZ"));
        assert_eq!(first(ChaseOrder::Farthest).as_deref(), Some("JA1XYZ"));
    }

    #[test]
    fn changing_band_forgets_who_was_heard() {
        let log = LogIndex::default();
        let s = ChaseSettings::default();
        let mut ch = Chaser::new();
        hear(&mut ch, "CQ JA1XYZ PM95", -12, &[0, 30]);
        let b = vec![dec(60, "CQ JA1XYZ PM95", -12)];
        ch.observe(&b, "15m");
        let c = ChaseCtx { band: "15m", ..ctx(60) };
        assert_eq!(ch.pick(&b, &s, &log, &c), None);
    }

    #[test]
    fn the_tally_says_why_nothing_is_called() {
        let log = log_of(&[("W1AW", "20m")]);
        let s = ChaseSettings::default();
        let mut ch = Chaser::new();
        for t in [0, 30, 60] {
            ch.observe(&[dec(t, "CQ W1AW FN31", -3)], "20m");
        }
        ch.observe(&[dec(60, "CQ JA1XYZ PM95", -12), dec(30, "CQ VK2ABC QF56", -18)], "20m");
        let t = ch.tally(&s, &log, &ctx(60));
        assert_eq!(t.not_needed, 1);
        assert_eq!(t.building, 2);
        assert_eq!(t.eligible, 0);
        assert!(t.next.is_some());
    }

    #[test]
    fn lists_parse_and_match() {
        let l = parse_chase_list("ve3*, k1abc\n  DL*");
        assert_eq!(l, vec!["VE3*", "K1ABC", "DL*"]);
        assert!(chase_list_matches(&l, "ve3xyz"));
        assert!(chase_list_matches(&l, "K1ABC"));
        assert!(!chase_list_matches(&l, "K1ABCD"));
        assert!(!chase_list_matches(&["*".into()], "K1ABC"), "a bare * matches nothing");
    }

    #[test]
    fn an_unresolved_hash_is_never_called() {
        let log = LogIndex::default();
        let s = ChaseSettings { min_heard: 1, answer: ChaseAnswer::Any, ..Default::default() };
        let mut ch = Chaser::new();
        let mut d = dec(0, "W1AW <...> RR73", -5);
        d.from = Some("<...>".into());
        ch.observe(std::slice::from_ref(&d), "20m");
        assert_eq!(ch.pick(&[d], &s, &log, &ctx(0)), None);
    }

    #[test]
    fn a_station_calling_us_is_answered_whatever_the_filters() {
        // Heard once, weak, already worked on the band, not wanted by the need.
        let log = log_of(&[("W9XYZ", "20m")]);
        let s = ChaseSettings {
            need: ChaseNeed::NewDxcc,
            min_snr_db: -5,
            dx_only: true,
            ..Default::default()
        };
        let mut ch = Chaser::new();
        hear(&mut ch, "EA4ABC W9XYZ EN52", -20, &[0]);
        assert_eq!(ch.pick(&[dec(0, "EA4ABC W9XYZ EN52", -20)], &s, &log, &ctx(0)), None);
        let p = ch.pick_caller(&s, &log, &ctx(0), None).expect("answered");
        assert_eq!(p.call, "W9XYZ");
        assert_eq!(p.grid.as_deref(), Some("EN52"));
        assert!(!p.wait_for_cq);
    }

    #[test]
    fn the_ignore_list_and_the_rest_still_hold_for_callers() {
        let log = LogIndex::default();
        let mut ch = Chaser::new();
        hear(&mut ch, "EA4ABC W9XYZ EN52", -10, &[0]);
        let s = ChaseSettings { ignore: vec!["W9*".into()], ..Default::default() };
        assert_eq!(ch.pick_caller(&s, &log, &ctx(0), None), None);
        let s = ChaseSettings::default(); // 30 min rest
        ch.mark_tried("W9XYZ", "20m", -60);
        assert_eq!(ch.pick_caller(&s, &log, &ctx(0), None), None);
        let s = ChaseSettings { retry_after_min: 0, ..Default::default() };
        assert!(ch.pick_caller(&s, &log, &ctx(0), None).is_some());
    }

    #[test]
    fn a_caller_is_answered_for_two_minutes_unless_they_move_on() {
        let log = LogIndex::default();
        let s = ChaseSettings::default();
        let mut ch = Chaser::new();
        hear(&mut ch, "EA4ABC W9XYZ EN52", -10, &[0]);
        assert!(ch.pick_caller(&s, &log, &ctx(CHASE_CALLER_KEEP_S), None).is_some());
        assert_eq!(ch.pick_caller(&s, &log, &ctx(CHASE_CALLER_KEEP_S + 15), None), None);
        // Gone to somebody else since: no longer calling us.
        hear(&mut ch, "K1ABC W9XYZ EN52", -10, &[30]);
        assert_eq!(ch.pick_caller(&s, &log, &ctx(30), None), None);
        // A sign-off to us is the end of a contact, not a call.
        hear(&mut ch, "EA4ABC VK2ABC RR73", -10, &[60]);
        assert_eq!(ch.pick_caller(&s, &log, &ctx(60), None), None);
    }

    #[test]
    fn the_station_in_hand_is_left_to_the_sequencer() {
        let log = LogIndex::default();
        let s = ChaseSettings::default();
        let mut ch = Chaser::new();
        hear(&mut ch, "EA4ABC W9XYZ EN52", -10, &[0]);
        assert_eq!(ch.pick_caller(&s, &log, &ctx(0), Some("w9xyz")), None);
    }

    #[test]
    fn the_newest_caller_goes_first_then_the_strongest() {
        let log = LogIndex::default();
        let s = ChaseSettings::default();
        let mut ch = Chaser::new();
        ch.observe(&[dec(0, "EA4ABC W9XYZ EN52", -2), dec(30, "EA4ABC K1ABC FN42", -15)], "20m");
        ch.observe(&[dec(30, "EA4ABC N0AA EM48", -9)], "20m");
        assert_eq!(
            ch.pick_caller(&s, &log, &ctx(30), None).map(|p| p.call).as_deref(),
            Some("N0AA")
        );
    }

    #[test]
    fn a_station_answering_our_call_late_is_answered_despite_the_rest() {
        let log = LogIndex::default();
        let s = ChaseSettings::default();
        let mut ch = Chaser::new();
        // Chased, and left for somebody else; now they come back with a report.
        ch.mark_tried("JA1XYZ", "20m", 0);
        hear(&mut ch, "EA4ABC JA1XYZ -15", -15, &[60]);
        assert_eq!(
            ch.pick_caller(&s, &log, &ctx(60), None).map(|p| p.call).as_deref(),
            Some("JA1XYZ")
        );
        // A fresh call from a station resting after a try is still left alone.
        hear(&mut ch, "EA4ABC JA1XYZ PM95", -15, &[90]);
        assert_eq!(ch.pick_caller(&s, &log, &ctx(90), None), None);
        // And one the operator skipped stays skipped, report or not.
        ch.mark_skipped("VK2ABC", "20m", 0);
        hear(&mut ch, "EA4ABC VK2ABC R-09", -9, &[120]);
        assert_eq!(ch.pick_caller(&s, &log, &ctx(120), None), None);
    }

    #[test]
    fn reports_are_told_from_other_messages() {
        for (m, r) in [
            ("EA4ABC W9XYZ -12", true),
            ("EA4ABC W9XYZ R+05", true),
            ("EA4ABC W9XYZ EN52", false),
            ("EA4ABC W9XYZ RR73", false),
            ("EA4ABC W9XYZ", false),
        ] {
            assert_eq!(is_report(&dec(0, m, -10)), r, "{m}");
        }
    }

    #[test]
    fn a_dupe_calling_us_goes_after_a_new_station_however_strong() {
        let log = log_of(&[("W9XYZ", "20m")]);
        let s = ChaseSettings { order: ChaseOrder::Strongest, ..Default::default() };
        let mut ch = Chaser::new();
        ch.observe(&[dec(30, "EA4ABC W9XYZ EN52", 5), dec(30, "EA4ABC K1ABC FN42", -18)], "20m");
        let first = ch.pick_caller(&s, &log, &ctx(30), None).map(|p| p.call);
        assert_eq!(first.as_deref(), Some("K1ABC"));
        // Still answered when nobody new is calling.
        ch.mark_tried("K1ABC", "20m", 30);
        let next = ch.pick_caller(&s, &log, &ctx(30), None).map(|p| p.call);
        assert_eq!(next.as_deref(), Some("W9XYZ"));
    }

    #[test]
    fn callers_from_the_same_period_follow_the_order_setting() {
        let log = LogIndex::default();
        let mut ch = Chaser::new();
        ch.observe(&[dec(30, "EA4ABC VK2ABC QF56", -15), dec(30, "EA4ABC F4ABC JN18", 3)], "20m");
        let pick = |order| {
            let s = ChaseSettings { order, ..Default::default() };
            ch.pick_caller(&s, &log, &ctx(30), None).map(|p| p.call)
        };
        assert_eq!(pick(ChaseOrder::Strongest).as_deref(), Some("F4ABC"));
        assert_eq!(pick(ChaseOrder::Farthest).as_deref(), Some("VK2ABC"));
    }

    fn log_in_mode(call: &str, band: &str, mode: &str) -> LogIndex {
        LogIndex::build(&[QsoRecord {
            call: call.into(),
            band: band.into(),
            mode: mode.into(),
            ..Default::default()
        }])
    }

    #[test]
    fn a_dupe_is_judged_by_band_and_mode_as_asked() {
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ W9XYZ EN52", -5, &[0, 30, 60]);
        let with = |dupe, log: &LogIndex| {
            let s = ChaseSettings { dupe, ..Default::default() };
            ch.pick(&b, &s, log, &ctx(60)).is_some()
        };
        // Worked on 20 m SSB: still wanted on FT8 unless any mode counts.
        let ssb = log_in_mode("W9XYZ", "20m", "SSB");
        assert!(with(ChaseDupe::BandMode, &ssb));
        assert!(with(ChaseDupe::DataModes, &ssb));
        assert!(!with(ChaseDupe::Band, &ssb));
        // Worked on 20 m FT4: another data mode.
        let ft4 = log_in_mode("W9XYZ", "20m", "FT4");
        assert!(with(ChaseDupe::BandMode, &ft4));
        assert!(!with(ChaseDupe::DataModes, &ft4));
        // Worked on 20 m FT8: a dupe whichever way it is asked.
        let ft8 = log_in_mode("W9XYZ", "20m", "FT8");
        for d in ChaseDupe::ALL {
            assert!(!with(d, &ft8), "{d:?}");
        }
    }

    #[test]
    fn a_station_in_another_contest_is_left_alone() {
        let log = LogIndex::default();
        let s = ChaseSettings { answer: ChaseAnswer::Any, ..Default::default() };
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ FD W9XYZ EN52", -5, &[0, 30, 60]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(60)), None);
        // Nor tail-ended, nor answered when it calls us mid-contest.
        let b = hear(&mut ch, "K1ABC W9XYZ RR73", -5, &[90]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(90)), None);
        hear(&mut ch, "EA4ABC W9XYZ R 2A EMA", -5, &[120]);
        assert_eq!(ch.pick_caller(&s, &log, &ctx(120), None), None);
        // An ordinary CQ afterwards: the contest is over for them.
        let b = hear(&mut ch, "CQ W9XYZ EN52", -5, &[150]);
        assert!(ch.pick(&b, &s, &log, &ctx(150)).is_some());
    }

    #[test]
    fn the_contest_we_are_set_up_for_is_answered() {
        let log = LogIndex::default();
        let s = ChaseSettings::default();
        let mut ch = Chaser::new();
        let b = hear(&mut ch, "CQ TEST DL1ABC JO62", -5, &[0, 30, 60]);
        assert_eq!(ch.pick(&b, &s, &log, &ctx(60)), None);
        let euvhf = ChaseCtx { contest: "TEST", ..ctx(60) };
        assert!(ch.pick(&b, &s, &log, &euvhf).is_some());
        // POTA and the like are not contests: answered as ever.
        let b = hear(&mut ch, "CQ POTA K1ABC FN42", -5, &[0, 30, 60]);
        assert!(ch.pick(&b, &s, &log, &ctx(60)).is_some());
    }
}
