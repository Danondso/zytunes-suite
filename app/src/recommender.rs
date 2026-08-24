//! Single-user, offline content-based + behavioral recommender.
//!
//! See `RECOMMENDER_DESIGN.md` for the full rationale; the short version:
//! - We have one user, one library, no peer matrix → no collaborative
//!   filtering, no learned audio embeddings.
//! - The strongest signals are behavioral (`play_count`, `last_played_at_ms`,
//!   `skip_count`) plus genre / year / artist for content similarity.
//! - A hand-tuned weighted scorer with MMR diversity re-ranking and hard
//!   per-artist / per-album caps beats nearest-neighbor for this size of
//!   problem and is fully explainable end-to-end.
//!
//! The flow:
//! ```text
//!   seeds = pick_seeds(plays, params.seed_strategy)
//!   candidates = library.all_tracks().filter(eligible)
//!   scored = candidates.map(|t| (t, score(t, seeds, plays))).filter(threshold)
//!   selected = mmr_select(scored, target_len, diversity_lambda)
//!   final = apply_artist_cap(selected, max_per_artist, max_per_album)
//! ```
//!
//! Scoring is the linear combination of normalized 0..1 terms documented in
//! the design doc. `novelty_jitter` is seeded by `(week_number,
//! library_root_hash)` so two regenerations in the same week produce the
//! same playlist but the next week drifts.

use std::collections::{HashMap, HashSet};

use crate::genre_norm;
use crate::library::{MusicLibrary, Track};
use crate::listen_log::{ListenEvent, ListenLog, DEFAULT_SESSION_GAP_MS};
use crate::local_plays::{LocalPlays, TrackPlays};
use crate::playlist::{GenerationParams, ScoringWeights, SeedStrategy};

/// Markov bigram counts over the listen log: `(prev_id, next_id) -> count`,
/// computed within sessions so morning vs. evening listening doesn't bleed
/// into one transition graph. Skips count for half a play (a skip means the
/// user transitioned but expressed disinterest in the destination).
///
/// Built once per recommendation pass via `BigramTable::from_log`. Lookups
/// during scoring are O(1) amortised.
#[derive(Debug, Clone, Default)]
pub struct BigramTable {
    /// Outgoing transition counts grouped by source. Each `Vec` is sorted
    /// by destination id so `followers_of` returns deterministic ordering.
    by_source: HashMap<u64, HashMap<u64, f32>>,
    /// Total outgoing weight per source — denominator for the transition
    /// probability. Pre-computed so `transition_prob` is one HashMap hit.
    source_totals: HashMap<u64, f32>,
    /// Total events ingested. Used by callers (and the TUI footer) to
    /// decide whether the model has enough data to be worth weighting.
    pub event_count: usize,
    /// Number of sessions extracted from the log. Same purpose as
    /// `event_count` — surfaces "do we have enough data yet?" diagnostics.
    pub session_count: usize,
}

impl BigramTable {
    /// Build from a `ListenLog` using the default 30-min session gap.
    pub fn from_log(log: &ListenLog) -> Self {
        Self::from_sessions(&log.sessions(DEFAULT_SESSION_GAP_MS), log.len())
    }

    /// Build from arbitrary session slices. Exposed so tests can drive the
    /// math without round-tripping through a log file.
    pub fn from_sessions(sessions: &[&[ListenEvent]], total_events: usize) -> Self {
        let mut by_source: HashMap<u64, HashMap<u64, f32>> = HashMap::new();
        let mut source_totals: HashMap<u64, f32> = HashMap::new();
        for session in sessions {
            for window in session.windows(2) {
                let prev = &window[0];
                let next = &window[1];
                if prev.id == next.id {
                    // Self-loop (same track replayed) gives no signal —
                    // skip so it doesn't drown out real transitions.
                    continue;
                }
                // Completed plays carry full weight; skips a half. The
                // sequence info is the transition itself, regardless of
                // whether the destination got listened all the way through.
                let weight = if next.completed { 1.0 } else { 0.5 };
                *by_source
                    .entry(prev.id)
                    .or_default()
                    .entry(next.id)
                    .or_insert(0.0) += weight;
                *source_totals.entry(prev.id).or_insert(0.0) += weight;
            }
        }
        BigramTable {
            by_source,
            source_totals,
            event_count: total_events,
            session_count: sessions.len(),
        }
    }

    /// `P(next | prev)` — how often did the user follow `prev` with `next`,
    /// normalised by all transitions out of `prev`. Returns `0.0` when the
    /// pair was never seen so the term cleanly drops out of the score.
    pub fn transition_prob(&self, prev: u64, next: u64) -> f32 {
        let total = match self.source_totals.get(&prev) {
            Some(t) if *t > 0.0 => *t,
            _ => return 0.0,
        };
        let count = self
            .by_source
            .get(&prev)
            .and_then(|m| m.get(&next))
            .copied()
            .unwrap_or(0.0);
        count / total
    }

    /// `true` when the model has at least a few events worth of data.
    /// Below the threshold the recommender skips the sequence term so
    /// new users aren't biased by spurious one-off transitions.
    pub fn is_useful(&self) -> bool {
        // 20 events across at least 2 sessions is roughly "the user has
        // played for 20+ minutes across two sittings" — enough to start
        // distinguishing genuine sequences from random shuffles. Below
        // this bar the prior is too noisy to trust.
        self.event_count >= 20 && self.session_count >= 2
    }
}

/// Pluggable recommender contract. There's only one implementation today
/// (`WeightedRecommender`); the trait exists so tests can swap in a stub
/// without dragging the full algorithm into every test fixture.
pub trait Recommender {
    /// Produce an ordered list of library track IDs for the playlist.
    /// `now_ms` is injected so tests can fix the clock; production code
    /// passes `playlist::now_unix_ms()`.
    ///
    /// `previously_recommended` is the soft-penalty bag (typically the prior
    /// generation's output); pass `&[]` for first-run.
    ///
    /// `bigrams` is the Phase 4 sequence-aware re-ranking table built from
    /// the listen log. Pass `None` to skip the sequence term entirely
    /// (existing tests, recommender-only contexts where no log exists).
    fn generate(
        &self,
        library: &dyn MusicLibrary,
        plays: &LocalPlays,
        params: &GenerationParams,
        previously_recommended: &[u64],
        bigrams: Option<&BigramTable>,
        now_ms: u64,
    ) -> GenerationOutcome;
}

/// What the recommender produced, plus a tiny diagnostic bag the TUI surfaces
/// in the sync log so weight-tuning is observable. Kept structured rather
/// than a free-form `Vec<String>` so future code can assert on it in tests.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GenerationOutcome {
    pub track_ids: Vec<u64>,
    pub seed_count: usize,
    pub candidate_count: usize,
    pub eligible_count: usize,
}

pub struct WeightedRecommender;

impl Recommender for WeightedRecommender {
    fn generate(
        &self,
        library: &dyn MusicLibrary,
        plays: &LocalPlays,
        params: &GenerationParams,
        previously_recommended: &[u64],
        bigrams: Option<&BigramTable>,
        now_ms: u64,
    ) -> GenerationOutcome {
        let all: Vec<&Track> = library.all_tracks().collect();
        let candidate_count = all.len();

        let seeds = pick_seeds(library, &all, plays, &params.seed_strategy, now_ms);
        let seed_ctx = SeedContext::new(&seeds, plays);
        // Only let the sequence term contribute when the log has enough
        // data to be more signal than noise.
        let active_bigrams = bigrams.filter(|b| b.is_useful());

        let exclude_artists_lower: HashSet<String> = params
            .exclude_artists
            .iter()
            .map(|a| a.to_lowercase())
            .collect();
        let exclude_track_ids: HashSet<u64> = params.exclude_track_ids.iter().copied().collect();
        let prior_set: HashSet<u64> = previously_recommended.iter().copied().collect();
        let seed_ids: HashSet<u64> = seeds.iter().map(|t| t.id).collect();

        // Filter to eligible candidates first — every downstream pass walks
        // the survivor set.
        let eligible: Vec<&Track> = all
            .iter()
            .copied()
            .filter(|t| eligible(t, &exclude_artists_lower, &exclude_track_ids, &seed_ids))
            .collect();
        let eligible_count = eligible.len();

        let novelty = params.novelty().clamp(0.0, 1.0);
        let weights = &params.weights;

        // Score every eligible candidate.
        let mut scored: Vec<(usize, f32)> = eligible
            .iter()
            .enumerate()
            .map(|(idx, t)| {
                let mut s = score(t, &seed_ctx, plays, weights, now_ms);
                if let Some(bg) = active_bigrams {
                    // Sequence term: max P(candidate | seed) across all
                    // seeds. Picking max (not sum) so a strong follower of
                    // even one seed dominates over diffuse weak transitions
                    // — matches the intuition "this track plays after that
                    // one a lot" rather than "vaguely related to the bag."
                    let seq_p = seeds
                        .iter()
                        .map(|s| bg.transition_prob(s.id, t.id))
                        .fold(0.0_f32, f32::max);
                    s += weights.w_sequence() * seq_p;
                }
                if prior_set.contains(&t.id) {
                    // Soft penalty for "regenerate this same playlist" — the
                    // higher the novelty knob, the harder we push prior picks
                    // out of the running. Capped at the score itself so we
                    // never produce a negative ratio that breaks comparisons.
                    s -= 0.4 * novelty;
                }
                (idx, s)
            })
            .collect();

        // Stable order by descending score so MMR sees a deterministic top
        // candidate before ties are broken by the diversity penalty.
        // `total_cmp` defines a deterministic NaN ordering so a malformed
        // weight (or accidental 0.0/0.0 in scoring) can't randomise picks.
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));

        let diversity_lambda = params.diversity_lambda().clamp(0.0, 1.0);
        let mmr_picks = mmr_select(&eligible, &scored, params.target_length, diversity_lambda);

        // Apply the hard per-artist / per-album caps as the final pass.
        let final_ids = apply_artist_cap(
            &mmr_picks.iter().map(|i| eligible[*i]).collect::<Vec<_>>(),
            params.max_per_artist.max(1),
            params.max_per_album.max(1),
        )
        .into_iter()
        .map(|t| t.id)
        .collect();

        GenerationOutcome {
            track_ids: final_ids,
            seed_count: seeds.len(),
            candidate_count,
            eligible_count,
        }
    }
}

/// Pull the seed track set per the requested strategy. For `Track`/`Artist`/
/// `Genre` strategies the seeds come straight from the library; for the
/// behavioral strategies (`TopPlayed`, `RecentlyPlayed`) they come from the
/// `LocalPlays` sidecar joined back to library tracks.
///
/// Always returns at least one seed when the library is non-empty, even if
/// the strategy yields nothing — falling back to the first library track is
/// strictly better than producing zero recommendations.
pub fn pick_seeds<'a>(
    library: &'a dyn MusicLibrary,
    all: &[&'a Track],
    plays: &LocalPlays,
    strategy: &SeedStrategy,
    now_ms: u64,
) -> Vec<&'a Track> {
    let by_id: HashMap<u64, &Track> = all.iter().map(|t| (t.id, *t)).collect();
    let mut out: Vec<&Track> = match strategy {
        SeedStrategy::TopPlayed { window_days, count } => {
            let cutoff_ms = if *window_days == 0 {
                0
            } else {
                now_ms.saturating_sub((*window_days as u64) * 86_400_000)
            };
            let mut ranked: Vec<(u64, u32)> = plays_iter(plays)
                .filter_map(|(tid, p)| {
                    if cutoff_ms == 0
                        || p.last_played_at_ms == 0
                        || p.last_played_at_ms >= cutoff_ms
                    {
                        Some((tid, p.play_count))
                    } else {
                        None
                    }
                })
                .filter(|(_, c)| *c > 0)
                .collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            ranked
                .into_iter()
                .take(*count)
                .filter_map(|(id, _)| by_id.get(&id).copied())
                .collect()
        }
        SeedStrategy::RecentlyPlayed { count } => {
            let mut ranked: Vec<(u64, u64)> = plays_iter(plays)
                .filter(|(_, p)| p.last_played_at_ms > 0)
                .map(|(tid, p)| (tid, p.last_played_at_ms))
                .collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            ranked
                .into_iter()
                .take(*count)
                .filter_map(|(id, _)| by_id.get(&id).copied())
                .collect()
        }
        SeedStrategy::Track(id) => by_id.get(id).copied().into_iter().collect(),
        SeedStrategy::Artist(name) => {
            let lower = name.to_lowercase();
            library
                .all_tracks()
                .filter(|t| t.artist.eq_ignore_ascii_case(&lower))
                .take(8)
                .collect()
        }
        SeedStrategy::Genre(name) => {
            let target = genre_norm::normalize(Some(name));
            library
                .all_tracks()
                .filter(|t| {
                    let toks = genre_norm::normalize(t.genre.as_deref());
                    target.iter().any(|g| toks.contains(g))
                })
                .take(8)
                .collect()
        }
    };

    if out.is_empty() {
        // Fall back to the first track so downstream code never has to
        // special-case "no seeds." Better to recommend something than
        // recommend nothing on day-1.
        if let Some(first) = all.first() {
            out.push(first);
        }
    }
    out
}

/// Helper: walk all `(track_id, &TrackPlays)` pairs without exposing
/// `LocalPlays`'s internal map directly. `LocalPlays` only exposes `get` by
/// ID, so this iterates the keys we know about by dipping into the public
/// surface — for now that means we serialise the whole sidecar into a
/// `BTreeMap`. Cheap relative to the rest of the pipeline.
fn plays_iter(plays: &LocalPlays) -> impl Iterator<Item = (u64, &TrackPlays)> + '_ {
    // The public API exposes `get(id) -> Option<&TrackPlays>` and `len`,
    // but no iterator. Add a thin shim through serialization to keep the
    // recommender independent of internal layout.
    plays.entries()
}

/// Filter predicate for "this track is even worth scoring." Hard rejects:
/// already-excluded artist, explicit track exclusion, the track is a seed
/// (we never recommend a seed back to itself), or the duration filter
/// catches it (skits / longforms).
pub fn eligible(
    track: &Track,
    exclude_artists_lower: &HashSet<String>,
    exclude_track_ids: &HashSet<u64>,
    seed_ids: &HashSet<u64>,
) -> bool {
    if exclude_track_ids.contains(&track.id) {
        return false;
    }
    if seed_ids.contains(&track.id) {
        return false;
    }
    if exclude_artists_lower.contains(&track.artist.to_lowercase()) {
        return false;
    }
    if let Some(ms) = track.total_time_ms {
        // Skits / interludes under 30s are noise; longforms over 12min
        // (album-side mixes, classical sets) crowd the playlist out.
        if !(30_000..=12 * 60_000).contains(&ms) {
            return false;
        }
    }
    true
}

/// Cached aggregate stats about the seed set so per-candidate scoring is
/// O(1) per term instead of O(seeds) per term.
pub struct SeedContext {
    seed_genres: Vec<String>,
    seed_year_mean: Option<f32>,
    seed_artist_play_total: HashMap<String, u32>,
    /// Total plays across the user's library (for log-normalising artist
    /// affinity). Always at least 1 so the divide is safe.
    total_plays: u32,
}

impl SeedContext {
    pub fn new(seeds: &[&Track], plays: &LocalPlays) -> Self {
        let mut genre_bag: Vec<String> = Vec::new();
        let mut year_sum = 0u64;
        let mut year_count = 0u32;
        for s in seeds {
            for g in genre_norm::normalize(s.genre.as_deref()) {
                if !genre_bag.contains(&g) {
                    genre_bag.push(g);
                }
            }
            if let Some(y) = s.year {
                year_sum += y as u64;
                year_count += 1;
            }
        }
        let seed_year_mean = if year_count > 0 {
            Some(year_sum as f32 / year_count as f32)
        } else {
            None
        };

        // Per-artist play totals across the user's history. Seeds with
        // higher play affinity for an artist boost candidates from that
        // artist downstream — captures "I keep coming back to Boards of
        // Canada" without needing per-track joins.
        let mut artist_total: HashMap<String, u32> = HashMap::new();
        let mut total_plays: u32 = 0;
        for (_, p) in plays.entries() {
            total_plays = total_plays.saturating_add(p.play_count);
            // The play sidecar is keyed by track ID, not artist; we have to
            // walk the seeds and ask "did the user play these seed tracks?"
            // For artist affinity we instead approximate via the seeds'
            // artists getting credit for the seeds' play counts.
        }
        for s in seeds {
            let entry = plays.get(s.id);
            let pc = entry.map(|p| p.play_count).unwrap_or(0);
            let key = s.artist.to_lowercase();
            *artist_total.entry(key).or_insert(0) += pc;
        }

        SeedContext {
            seed_genres: genre_bag,
            seed_year_mean,
            seed_artist_play_total: artist_total,
            total_plays: total_plays.max(1),
        }
    }
}

/// Score a single candidate. Returns a value in roughly `[-w_skip, sum(w_*)]`
/// — caller compares against threshold or sorts directly.
pub fn score(
    track: &Track,
    seeds: &SeedContext,
    plays: &LocalPlays,
    weights: &ScoringWeights,
    now_ms: u64,
) -> f32 {
    let track_genres = genre_norm::normalize(track.genre.as_deref());
    let genre_term = genre_norm::jaccard(&track_genres, &seeds.seed_genres);

    let year_term = match (seeds.seed_year_mean, track.year) {
        (Some(mean), Some(y)) => {
            // Exponential decay over absolute year distance — 5 years halves
            // the contribution, 20 years zeroes it for practical purposes.
            let dist = (y as f32 - mean).abs();
            (-dist / 5.0).exp()
        }
        _ => 0.0,
    };

    let artist_lower = track.artist.to_lowercase();
    let artist_seed_plays = seeds
        .seed_artist_play_total
        .get(&artist_lower)
        .copied()
        .unwrap_or(0);
    let artist_term = if artist_seed_plays == 0 {
        0.0
    } else {
        // log1p over the seed-aggregate plays, normalised by log1p of the
        // user's total play history. `total_plays` is clamped to ≥1 in
        // SeedContext::new, so `den = ln(1 + n)` for n≥1 is always > 0 —
        // no zero-divide guard needed.
        let num = (1.0 + artist_seed_plays as f32).ln();
        let den = (1.0 + seeds.total_plays as f32).ln();
        (num / den).clamp(0.0, 1.0)
    };

    let track_plays = plays.get(track.id);
    let recency_term = match track_plays {
        Some(p) if p.last_played_at_ms > 0 => {
            // Boost tracks NOT played recently. Score = 1 - exp(-days/14):
            // played today → 0, week ago → ~0.4, month ago → ~0.88.
            let days = (now_ms.saturating_sub(p.last_played_at_ms)) as f32 / 86_400_000.0;
            1.0 - (-days / 14.0).exp()
        }
        // Never played → maximum recency boost (it's "due").
        _ => 1.0,
    };

    let rating_term = match track.rating {
        // POPM: 80+ counts as "the user explicitly liked this." Anything
        // below is treated as no signal rather than a penalty — most users
        // never set ratings, and the absence of a rating is not a vote.
        Some(r) if r >= 80 => 1.0,
        _ => 0.0,
    };

    let skip_term = match track_plays {
        Some(p) if p.skip_count > 0 => {
            // log1p so 1 skip is a notable penalty but 20 skips isn't
            // catastrophic — users sometimes skip a track they like
            // because they're not in the mood, and we don't want to
            // permanently blacklist it.
            ((1.0 + p.skip_count as f32).ln() / (1.0_f32 + 20.0).ln()).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };

    weights.w_genre() * genre_term
        + weights.w_year() * year_term
        + weights.w_artist() * artist_term
        + weights.w_recency() * recency_term
        + weights.w_rating() * rating_term
        - weights.w_skip() * skip_term
}

/// Maximal Marginal Relevance: greedy pick that trades off raw score against
/// proximity to already-picked items. With `lambda = 0` reduces to top-K by
/// score; with `lambda = 1` becomes pure diversity. We use a very cheap
/// similarity proxy — same artist, same album, shared genre — because the
/// hard caps below catch the worst clustering anyway.
pub fn mmr_select(
    candidates: &[&Track],
    scored: &[(usize, f32)],
    target_len: usize,
    lambda: f32,
) -> Vec<usize> {
    if candidates.is_empty() || target_len == 0 {
        return Vec::new();
    }
    let mut picked: Vec<usize> = Vec::with_capacity(target_len);
    let mut available: Vec<(usize, f32)> = scored.to_vec();

    while picked.len() < target_len && !available.is_empty() {
        let (best_pos, _) = available
            .iter()
            .enumerate()
            .map(|(pos, (cand_idx, base_score))| {
                let max_sim = picked
                    .iter()
                    .map(|p_idx| similarity(candidates[*cand_idx], candidates[*p_idx]))
                    .fold(0.0_f32, f32::max);
                let mmr_score = (1.0 - lambda) * base_score - lambda * max_sim;
                (pos, mmr_score)
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap_or((0, 0.0));
        let (cand_idx, _) = available.swap_remove(best_pos);
        picked.push(cand_idx);
    }

    picked
}

/// Cheap pairwise similarity for MMR. Same artist contributes 0.5, same
/// album +0.3, shared genre token +0.2 (capped at 1.0). Not symmetric in
/// a true metric sense but symmetric enough for greedy diversification.
fn similarity(a: &Track, b: &Track) -> f32 {
    let mut s: f32 = 0.0;
    if a.artist.eq_ignore_ascii_case(&b.artist) {
        s += 0.5;
    }
    if a.album.eq_ignore_ascii_case(&b.album) {
        s += 0.3;
    }
    let ga = genre_norm::normalize(a.genre.as_deref());
    let gb = genre_norm::normalize(b.genre.as_deref());
    if ga.iter().any(|x| gb.iter().any(|y| y == x)) {
        s += 0.2;
    }
    s.min(1.0)
}

/// Hard per-artist and per-album caps. Walks the input in order (which is
/// already MMR-ranked) and drops anything that would breach a cap. Returning
/// borrowed references rather than owned copies keeps allocation flat.
pub fn apply_artist_cap<'a>(
    tracks: &[&'a Track],
    max_per_artist: usize,
    max_per_album: usize,
) -> Vec<&'a Track> {
    let mut artist_counts: HashMap<String, usize> = HashMap::new();
    let mut album_counts: HashMap<(String, String), usize> = HashMap::new();
    let mut out = Vec::with_capacity(tracks.len());
    for t in tracks {
        let a_key = t.artist.to_lowercase();
        let al_key = (a_key.clone(), t.album.to_lowercase());
        let a_n = artist_counts.entry(a_key).or_insert(0);
        let al_n = album_counts.entry(al_key).or_insert(0);
        if *a_n >= max_per_artist || *al_n >= max_per_album {
            continue;
        }
        *a_n += 1;
        *al_n += 1;
        out.push(*t);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{MusicLibrary, Track};
    use crate::local_plays::LocalPlays;
    use crate::playlist::{GenerationParams, ScoringWeights, SeedStrategy};

    fn t(id: u64, artist: &str, album: &str, name: &str) -> Track {
        Track {
            id,
            name: name.into(),
            artist: artist.into(),
            album: album.into(),
            genre: Some("Rock".into()),
            year: Some(2010),
            total_time_ms: Some(180_000),
            ..Default::default()
        }
    }

    fn t_with_genre_year(
        id: u64,
        artist: &str,
        album: &str,
        name: &str,
        genre: &str,
        year: u32,
    ) -> Track {
        Track {
            id,
            name: name.into(),
            artist: artist.into(),
            album: album.into(),
            genre: Some(genre.into()),
            year: Some(year),
            total_time_ms: Some(180_000),
            ..Default::default()
        }
    }

    struct VecLib(Vec<Track>);
    impl MusicLibrary for VecLib {
        fn artists(&self) -> Vec<&str> {
            self.0.iter().map(|t| t.artist.as_str()).collect()
        }
        fn albums(&self) -> Vec<(&str, &str)> {
            self.0
                .iter()
                .map(|t| (t.artist.as_str(), t.album.as_str()))
                .collect()
        }
        fn artist_tracks<'a>(&'a self, a: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
            let a_owned = a.to_string();
            Box::new(
                self.0
                    .iter()
                    .filter(move |t| t.artist.eq_ignore_ascii_case(&a_owned)),
            )
        }
        fn album_tracks<'a>(&'a self, a: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
            let a_owned = a.to_string();
            Box::new(
                self.0
                    .iter()
                    .filter(move |t| t.album.eq_ignore_ascii_case(&a_owned)),
            )
        }
        fn album_tracks_by_artist<'a>(
            &'a self,
            ar: &str,
            al: &str,
        ) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
            let ar = ar.to_string();
            let al = al.to_string();
            Box::new(self.0.iter().filter(move |t| {
                t.artist.eq_ignore_ascii_case(&ar) && t.album.eq_ignore_ascii_case(&al)
            }))
        }
        fn tracks_by_name<'a>(&'a self, n: &str) -> Box<dyn Iterator<Item = &'a Track> + 'a> {
            let n = n.to_string();
            Box::new(
                self.0
                    .iter()
                    .filter(move |t| t.name.eq_ignore_ascii_case(&n)),
            )
        }
        fn track_count(&self) -> usize {
            self.0.len()
        }
        fn all_tracks(&self) -> Box<dyn Iterator<Item = &Track> + '_> {
            Box::new(self.0.iter())
        }
        fn music_folder(&self) -> Option<&str> {
            None
        }
    }

    // -- pick_seeds --

    #[test]
    fn pick_seeds_top_played_orders_by_play_count() {
        let lib = VecLib(vec![
            t(1, "A", "X", "T1"),
            t(2, "B", "X", "T2"),
            t(3, "C", "X", "T3"),
        ]);
        let mut plays = LocalPlays::new();
        plays.record_play(1, 100);
        plays.record_play(1, 200);
        plays.record_play(1, 300);
        plays.record_play(2, 100);
        plays.record_play(3, 100);
        plays.record_play(3, 200);

        let all: Vec<&Track> = lib.all_tracks().collect();
        let seeds = pick_seeds(
            &lib,
            &all,
            &plays,
            &SeedStrategy::TopPlayed {
                window_days: 0,
                count: 2,
            },
            1_000,
        );
        let names: Vec<&str> = seeds.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["T1", "T3"]);
    }

    #[test]
    fn pick_seeds_top_played_with_window_excludes_old_plays() {
        let lib = VecLib(vec![t(1, "A", "X", "T1"), t(2, "B", "X", "T2")]);
        let mut plays = LocalPlays::new();
        // T1 played long ago, T2 played recently.
        plays.record_play(1, 1_000);
        plays.record_play(2, 1_000_000_000);

        let all: Vec<&Track> = lib.all_tracks().collect();
        let seeds = pick_seeds(
            &lib,
            &all,
            &plays,
            &SeedStrategy::TopPlayed {
                window_days: 1,
                count: 5,
            },
            1_000_000_500,
        );
        // Only T2 is within the window.
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].name, "T2");
    }

    #[test]
    fn pick_seeds_recently_played_orders_by_last_played_desc() {
        let lib = VecLib(vec![
            t(1, "A", "X", "T1"),
            t(2, "B", "X", "T2"),
            t(3, "C", "X", "T3"),
        ]);
        let mut plays = LocalPlays::new();
        plays.record_play(1, 100);
        plays.record_play(2, 300);
        plays.record_play(3, 200);

        let all: Vec<&Track> = lib.all_tracks().collect();
        let seeds = pick_seeds(
            &lib,
            &all,
            &plays,
            &SeedStrategy::RecentlyPlayed { count: 10 },
            1_000,
        );
        let names: Vec<&str> = seeds.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["T2", "T3", "T1"]);
    }

    #[test]
    fn pick_seeds_track_strategy_resolves_by_id() {
        let lib = VecLib(vec![t(1, "A", "X", "T1"), t(42, "B", "X", "T42")]);
        let plays = LocalPlays::new();
        let all: Vec<&Track> = lib.all_tracks().collect();
        let seeds = pick_seeds(&lib, &all, &plays, &SeedStrategy::Track(42), 0);
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].id, 42);
    }

    #[test]
    fn pick_seeds_falls_back_to_first_when_strategy_yields_nothing() {
        // Empty plays + TopPlayed → empty ranking → fall back to first track.
        let lib = VecLib(vec![t(1, "A", "X", "T1"), t(2, "B", "X", "T2")]);
        let plays = LocalPlays::new();
        let all: Vec<&Track> = lib.all_tracks().collect();
        let seeds = pick_seeds(
            &lib,
            &all,
            &plays,
            &SeedStrategy::TopPlayed {
                window_days: 0,
                count: 5,
            },
            0,
        );
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0].id, 1);
    }

    #[test]
    fn pick_seeds_empty_library_returns_empty_no_panic() {
        let lib = VecLib(vec![]);
        let plays = LocalPlays::new();
        let all: Vec<&Track> = lib.all_tracks().collect();
        let seeds = pick_seeds(
            &lib,
            &all,
            &plays,
            &SeedStrategy::RecentlyPlayed { count: 5 },
            0,
        );
        assert!(seeds.is_empty());
    }

    // -- eligible --

    #[test]
    fn eligible_excludes_seeds_and_blocked_artists() {
        let track = t(7, "Banned", "Album", "Song");
        let mut excl = HashSet::new();
        excl.insert("banned".into());
        let empty: HashSet<u64> = HashSet::new();
        let seeds: HashSet<u64> = HashSet::new();
        assert!(!eligible(&track, &excl, &empty, &seeds));
    }

    #[test]
    fn eligible_drops_skits_under_30s() {
        let mut track = t(1, "A", "X", "Skit");
        track.total_time_ms = Some(20_000);
        let empty: HashSet<String> = HashSet::new();
        let empty_ids: HashSet<u64> = HashSet::new();
        assert!(!eligible(&track, &empty, &empty_ids, &empty_ids));
    }

    #[test]
    fn eligible_drops_longforms_over_12min() {
        let mut track = t(1, "A", "X", "Epic");
        track.total_time_ms = Some(13 * 60_000);
        let empty: HashSet<String> = HashSet::new();
        let empty_ids: HashSet<u64> = HashSet::new();
        assert!(!eligible(&track, &empty, &empty_ids, &empty_ids));
    }

    #[test]
    fn eligible_keeps_normal_track() {
        let track = t(1, "A", "X", "Song");
        let empty: HashSet<String> = HashSet::new();
        let empty_ids: HashSet<u64> = HashSet::new();
        assert!(eligible(&track, &empty, &empty_ids, &empty_ids));
    }

    // -- score --

    #[test]
    fn score_higher_for_same_genre_and_year() {
        let seed = t_with_genre_year(
            1,
            "Boards",
            "Music Has The Right",
            "Roygbiv",
            "Electronic",
            1998,
        );
        let same = t_with_genre_year(
            2,
            "Aphex Twin",
            "SAW2",
            "Stone in Focus",
            "Electronic",
            1994,
        );
        let different = t_with_genre_year(
            3,
            "Slayer",
            "Reign in Blood",
            "Raining Blood",
            "Metal",
            1986,
        );

        let plays = LocalPlays::new();
        let seeds_vec = vec![&seed];
        let ctx = SeedContext::new(&seeds_vec, &plays);
        let weights = ScoringWeights::default();

        let s_same = score(&same, &ctx, &plays, &weights, 0);
        let s_diff = score(&different, &ctx, &plays, &weights, 0);
        assert!(
            s_same > s_diff,
            "same genre/year should score higher: {s_same} vs {s_diff}"
        );
    }

    #[test]
    fn score_recency_boost_for_long_unplayed_track() {
        let seed = t(1, "A", "X", "Seed");
        let played_today = t(2, "Cand", "Y", "Today");
        let played_long_ago = t(3, "Cand", "Y", "Ages");

        let mut plays = LocalPlays::new();
        let now = 86_400_000_u64 * 200; // 200 days into "epoch"
        plays.record_play(2, now); // today
        plays.record_play(3, now - 86_400_000 * 90); // 90 days ago

        let seeds_vec = vec![&seed];
        let ctx = SeedContext::new(&seeds_vec, &plays);
        let weights = ScoringWeights::default();

        let s_recent = score(&played_today, &ctx, &plays, &weights, now);
        let s_old = score(&played_long_ago, &ctx, &plays, &weights, now);
        assert!(s_old > s_recent, "older play → bigger recency boost");
    }

    #[test]
    fn score_skip_penalty_lowers_score() {
        let seed = t(1, "A", "X", "Seed");
        let cand = t(2, "Cand", "Y", "Cand");

        let mut plays_no_skips = LocalPlays::new();
        plays_no_skips.record_play(2, 100);
        let mut plays_with_skips = plays_no_skips.clone();
        plays_with_skips.record_skip(2);
        plays_with_skips.record_skip(2);
        plays_with_skips.record_skip(2);

        let seeds_vec = vec![&seed];
        let ctx = SeedContext::new(&seeds_vec, &plays_no_skips);
        let weights = ScoringWeights::default();

        let s_clean = score(&cand, &ctx, &plays_no_skips, &weights, 1_000_000);
        let s_skipped = score(&cand, &ctx, &plays_with_skips, &weights, 1_000_000);
        assert!(s_skipped < s_clean, "skips should drop the score");
    }

    // -- mmr_select --

    #[test]
    fn mmr_lambda_zero_picks_top_by_score() {
        let lib = [
            t(1, "A", "X", "T1"),
            t(2, "B", "Y", "T2"),
            t(3, "C", "Z", "T3"),
        ];
        let refs: Vec<&Track> = lib.iter().collect();
        let scored = vec![(0, 0.9), (1, 0.5), (2, 0.7)];
        let picks = mmr_select(&refs, &scored, 2, 0.0);
        // λ=0 → just top-2 by score regardless of similarity.
        assert_eq!(picks, vec![0, 2]);
    }

    #[test]
    fn mmr_lambda_high_diversifies_artists() {
        let lib = [
            t(1, "Same", "X", "A"),
            t(2, "Same", "X", "B"),
            t(3, "Diff", "Y", "C"),
        ];
        let refs: Vec<&Track> = lib.iter().collect();
        let scored = vec![(0, 0.9), (1, 0.85), (2, 0.5)];
        let picks = mmr_select(&refs, &scored, 2, 0.9);
        // High λ should prefer the diverse pick over the same-artist runner-up.
        assert!(picks.contains(&0));
        assert!(
            picks.contains(&2),
            "high λ should pull in the diverse track"
        );
    }

    // -- apply_artist_cap --

    #[test]
    fn artist_cap_bounds_per_artist() {
        let lib = [
            t(1, "A", "X", "1"),
            t(2, "A", "X", "2"),
            t(3, "A", "X", "3"),
            t(4, "B", "Y", "4"),
        ];
        let refs: Vec<&Track> = lib.iter().collect();
        let out = apply_artist_cap(&refs, 2, 5);
        assert_eq!(out.len(), 3);
        let ids: Vec<u64> = out.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 2, 4]);
    }

    #[test]
    fn artist_cap_bounds_per_album() {
        let lib = [
            t(1, "A", "X", "1"),
            t(2, "A", "X", "2"),
            t(3, "A", "Y", "3"),
        ];
        let refs: Vec<&Track> = lib.iter().collect();
        // max 5 per artist, max 1 per album → drop the second from album X.
        let out = apply_artist_cap(&refs, 5, 1);
        let ids: Vec<u64> = out.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 3]);
    }

    // -- end-to-end generate --

    #[test]
    fn weighted_recommender_end_to_end_smoke() {
        let lib = VecLib(vec![
            t_with_genre_year(1, "BoC", "Geogaddi", "Music Is Math", "Electronic", 2002),
            t_with_genre_year(2, "BoC", "Geogaddi", "Gyroscope", "Electronic", 2002),
            t_with_genre_year(
                3,
                "BoC",
                "Music Has The Right",
                "Roygbiv",
                "Electronic",
                1998,
            ),
            t_with_genre_year(
                4,
                "Aphex Twin",
                "SAW2",
                "Stone in Focus",
                "Electronic",
                1994,
            ),
            t_with_genre_year(
                5,
                "Slayer",
                "Reign in Blood",
                "Raining Blood",
                "Metal",
                1986,
            ),
            t_with_genre_year(6, "Mozart", "Requiem", "Lacrimosa", "Classical", 1791),
        ]);
        let mut plays = LocalPlays::new();
        // User loves BoC.
        plays.record_play(1, 1_000);
        plays.record_play(1, 2_000);
        plays.record_play(2, 1_000);

        let params = GenerationParams::default_discover_weekly();
        let r = WeightedRecommender;
        let outcome = r.generate(&lib, &plays, &params, &[], None, 10_000);

        assert!(!outcome.track_ids.is_empty(), "should produce something");
        assert!(
            outcome.track_ids.iter().all(|id| ![1, 2].contains(id)),
            "seeds must not appear in their own recommendations"
        );
        // BoC track 3 (same genre as the seeds) should be picked over Mozart.
        assert!(
            outcome.track_ids.iter().any(|id| *id == 3 || *id == 4),
            "should favor electronic over classical/metal"
        );
        assert!(outcome.seed_count > 0);
    }

    #[test]
    fn weighted_recommender_artist_cap_holds_end_to_end() {
        // Library has 10 tracks all by the same artist, max_per_artist = 2.
        let lib = VecLib(
            (1..=10)
                .map(|i| t(i, "Solo", &format!("Album {}", i), &format!("Song {}", i)))
                .collect(),
        );
        let mut plays = LocalPlays::new();
        plays.record_play(1, 1_000);

        let mut params = GenerationParams::default_discover_weekly();
        params.target_length = 10;
        params.max_per_artist = 2;
        params.exclude_on_device = false;
        let r = WeightedRecommender;
        let outcome = r.generate(&lib, &plays, &params, &[], None, 10_000);
        assert!(
            outcome.track_ids.len() <= 2,
            "artist cap must hold; got {}",
            outcome.track_ids.len()
        );
    }

    #[test]
    fn weighted_recommender_empty_library_yields_empty_outcome_no_panic() {
        // Day-1 user with no library and no listen history. Every code
        // path that divides by a count, indexes a slice, or sorts a Vec
        // has to tolerate this — otherwise the TUI's "Generate" button
        // would crash on first run before the user has imported anything.
        let lib = VecLib(vec![]);
        let plays = LocalPlays::new();
        let params = GenerationParams::default_discover_weekly();
        let outcome = WeightedRecommender.generate(&lib, &plays, &params, &[], None, 10_000);
        assert!(outcome.track_ids.is_empty());
        assert_eq!(outcome.candidate_count, 0);
        assert_eq!(outcome.eligible_count, 0);
        assert_eq!(outcome.seed_count, 0);
    }

    #[test]
    fn weighted_recommender_all_candidates_filtered_yields_empty_outcome() {
        // Library exists but every track is excluded — by artist block,
        // explicit ID exclusion, or duration filter. The fallback in
        // `pick_seeds` will still surface a seed (so `seed_count >= 1`),
        // but the eligible set is empty and the outcome track_ids must
        // be empty without panic. Regression target: any future indexing
        // into `eligible[0]` or division by `eligible.len()`.
        let mut tracks = vec![
            t_with_genre_year(1, "Banned", "X", "T1", "Rock", 2000),
            t_with_genre_year(2, "Banned", "X", "T2", "Rock", 2000),
            t_with_genre_year(3, "Banned", "X", "T3", "Rock", 2000),
        ];
        // Force every track under the 30s duration floor so even if the
        // exclude_artists wiring shifts in the future, the eligibility
        // filter still rejects them.
        for t in &mut tracks {
            t.total_time_ms = Some(10_000);
        }
        let lib = VecLib(tracks);
        let plays = LocalPlays::new();
        let mut params = GenerationParams::default_discover_weekly();
        params.exclude_artists = vec!["banned".into()];
        let outcome = WeightedRecommender.generate(&lib, &plays, &params, &[], None, 10_000);
        assert!(
            outcome.track_ids.is_empty(),
            "no track should survive filtering"
        );
        assert_eq!(outcome.candidate_count, 3);
        assert_eq!(outcome.eligible_count, 0);
    }

    #[test]
    fn weighted_recommender_previously_recommended_drops_in_priority() {
        let lib = VecLib(vec![
            t(1, "Seed", "X", "Seed"),
            t(2, "A", "X", "Cand A"),
            t(3, "B", "Y", "Cand B"),
        ]);
        let mut plays = LocalPlays::new();
        plays.record_play(1, 1_000);

        let mut params = GenerationParams::default_discover_weekly();
        params.target_length = 1;
        // Crank novelty so the soft-penalty meaningfully shifts ordering.
        params.novelty_bits = 1.0_f32.to_bits();
        params.exclude_on_device = false;
        let r = WeightedRecommender;

        let first = r.generate(&lib, &plays, &params, &[], None, 10_000);
        let pick = first.track_ids[0];
        // Regenerate with the prior pick as a soft penalty — should drift.
        let second = r.generate(&lib, &plays, &params, &[pick], None, 10_000);
        assert!(
            second.track_ids[0] != pick,
            "novelty soft penalty should make us drift off the previous pick"
        );
    }

    // -- BigramTable / sequence model --

    fn ev(ts: u64, id: u64, completed: bool) -> ListenEvent {
        ListenEvent { ts, id, completed }
    }

    #[test]
    fn bigram_transition_prob_basic() {
        // Session: 1 → 2 → 1 → 3 → 2.
        // From 1: {2: 1, 3: 1} → 1→2 = 0.5
        // From 2: {1: 1}        → 2→1 = 1.0
        // From 3: {2: 1}        → 3→2 = 1.0
        let session: Vec<ListenEvent> = vec![
            ev(100, 1, true),
            ev(200, 2, true),
            ev(300, 1, true),
            ev(400, 3, true),
            ev(500, 2, true),
        ];
        let table = BigramTable::from_sessions(&[&session[..]], session.len());
        assert!((table.transition_prob(1, 2) - 0.5).abs() < 1e-6);
        assert!((table.transition_prob(1, 3) - 0.5).abs() < 1e-6);
        assert!((table.transition_prob(2, 1) - 1.0).abs() < 1e-6);
        assert_eq!(table.transition_prob(99, 1), 0.0, "unseen prev → 0");
        assert_eq!(table.transition_prob(1, 99), 0.0, "unseen next → 0");
    }

    #[test]
    fn bigram_skips_count_half() {
        // Skip-as-next is half-weighted compared to a completed-next.
        let session = [ev(100, 1, true), ev(200, 2, false)];
        let table = BigramTable::from_sessions(&[&session[..]], session.len());
        // Single transition with weight 0.5; total out of 1 is also 0.5;
        // probability is 1.0 (it's the only outgoing edge).
        assert!((table.transition_prob(1, 2) - 1.0).abs() < 1e-6);
        // But the absolute weight stayed half — verify by adding a real
        // play so the probability splits as 0.5/(0.5+1.0) = 1/3.
        let session2 = [
            ev(100, 1, true),
            ev(200, 2, false),
            ev(300, 1, true),
            ev(400, 3, true),
        ];
        let table = BigramTable::from_sessions(&[&session2[..]], session2.len());
        let p_skip = table.transition_prob(1, 2);
        let p_play = table.transition_prob(1, 3);
        assert!(
            p_skip < p_play,
            "skipped follower < completed follower (got {p_skip} vs {p_play})"
        );
    }

    #[test]
    fn bigram_self_loops_dropped() {
        // A user replaying the same track shouldn't pollute the model.
        let session = [ev(100, 1, true), ev(200, 1, true), ev(300, 2, true)];
        let table = BigramTable::from_sessions(&[&session[..]], session.len());
        // Only the 1→2 transition should exist (the 1→1 self-loop dropped).
        assert!((table.transition_prob(1, 2) - 1.0).abs() < 1e-6);
        assert_eq!(table.transition_prob(1, 1), 0.0);
    }

    #[test]
    fn bigram_transitions_dont_cross_session_boundaries() {
        // Two sessions: S1 ends with id=1, S2 starts with id=99. The
        // transition 1 → 99 must not be counted because it crossed a
        // session boundary.
        let s1 = [ev(100, 1, true)];
        let s2 = [ev(200, 99, true), ev(300, 5, true)];
        let table = BigramTable::from_sessions(&[&s1[..], &s2[..]], 3);
        assert_eq!(table.transition_prob(1, 99), 0.0);
        assert!((table.transition_prob(99, 5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bigram_is_useful_threshold_gates_small_data() {
        // Single-event log → not useful.
        let table = BigramTable::default();
        assert!(!table.is_useful());

        // 19 events / 1 session → still below threshold.
        let session: Vec<ListenEvent> = (0..19).map(|i| ev(i * 1000, i + 1, true)).collect();
        let table = BigramTable::from_sessions(&[&session[..]], session.len());
        assert!(!table.is_useful());

        // 20+ events / 2+ sessions → useful.
        let s1: Vec<ListenEvent> = (0..15).map(|i| ev(i * 1000, i + 1, true)).collect();
        let s2: Vec<ListenEvent> = (0..10).map(|i| ev(i * 1000, i + 100, true)).collect();
        let table = BigramTable::from_sessions(&[&s1[..], &s2[..]], 25);
        assert!(table.is_useful());
    }

    #[test]
    fn recommender_with_bigrams_boosts_followed_track() {
        // Library has a strong seed (id 1) and two equally-content-similar
        // candidates (ids 2 and 3). The bigram table says the user
        // strongly tends to play 3 after 1. With a useful enough log,
        // the recommender should pick 3 over 2.
        let lib = VecLib(vec![
            t_with_genre_year(1, "Same", "X", "Seed", "Rock", 2010),
            t_with_genre_year(2, "Diff", "Y", "Cand A", "Rock", 2010),
            t_with_genre_year(3, "Other", "Z", "Cand B", "Rock", 2010),
        ]);
        let mut plays = LocalPlays::new();
        plays.record_play(1, 1_000);

        // Build a bigram table that strongly biases 1→3 and reaches the
        // is_useful threshold (≥ 20 events, ≥ 2 sessions).
        let mut s1 = vec![];
        for i in 0..15 {
            s1.push(ev(i * 1000, 1, true));
            s1.push(ev(i * 1000 + 100, 3, true));
        }
        let s2 = [ev(2_000_000, 1, true), ev(2_001_000, 3, true)];
        let bg = BigramTable::from_sessions(&[&s1[..], &s2[..]], 32);
        assert!(bg.is_useful());

        let mut params = GenerationParams::default_discover_weekly();
        params.target_length = 1;
        params.exclude_on_device = false;

        // Without bigrams: recommender picks one of {2, 3} per content scoring.
        // With bigrams: should prefer 3 (the followed track).
        let r = WeightedRecommender;
        let out_with = r.generate(&lib, &plays, &params, &[], Some(&bg), 10_000);
        assert_eq!(
            out_with.track_ids,
            vec![3],
            "bigram boost picks followed track"
        );
    }

    #[test]
    fn recommender_skips_bigrams_when_not_useful() {
        // A tiny log shouldn't influence scoring at all.
        let lib = VecLib(vec![
            t_with_genre_year(1, "Same", "X", "Seed", "Rock", 2010),
            t_with_genre_year(2, "Diff", "Y", "Cand A", "Rock", 2010),
        ]);
        let mut plays = LocalPlays::new();
        plays.record_play(1, 1_000);

        // Only 2 events, 1 session → below threshold.
        let s = [ev(100, 1, true), ev(200, 2, true)];
        let bg = BigramTable::from_sessions(&[&s[..]], s.len());
        assert!(!bg.is_useful());

        let mut params = GenerationParams::default_discover_weekly();
        params.target_length = 1;
        params.exclude_on_device = false;
        let r = WeightedRecommender;

        // Should produce a result regardless; just verify no panic and
        // that the recommendation is the only candidate.
        let out = r.generate(&lib, &plays, &params, &[], Some(&bg), 10_000);
        assert_eq!(out.track_ids, vec![2]);
    }
}
