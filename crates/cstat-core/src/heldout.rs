//! Walk-forward game models for STORED projections of past games (#468).
//!
//! The served point-in-time bundle (`pit_*_model.onnx`) is fitted on every
//! season in `pit_model_meta.json`. That is right for a live game and wrong for
//! a stored projection of a game inside that range, because the model scoring
//! it has already seen the result: on 2026 the stored rows graded 59.0% ATS
//! against 54.4% for a model that had not.
//!
//! `training/train_heldout_game_models.py` fits, for each trained-on season S
//! after the first, the same margin and total models on seasons strictly
//! before S, into `MODEL_DIR/heldout/`. [`ModelRouter`] decides which model
//! scores a season's stored projections:
//!
//! - a season the full fit never saw (the one being played) → the full fit,
//!   exactly as before;
//! - a trained-on season with a bundle → that bundle;
//! - the first trained-on season, which has nothing earlier to train on → no
//!   stored projection at all ([`SeasonModel::NoEarlierSeason`]);
//! - a trained-on season whose bundle is missing or was fitted for a different
//!   full fit → an error naming why. Never a fallback to the full fit, which
//!   would quietly put the leak back.
//!
//! Only the `game-projections` sweep reads these. [`crate::inference::Predictor`]
//! does not load them, so the API's boot is unaffected and the deploy image
//! does not need them: prod's nightly sweeps the season being played, which no
//! full fit has seen.
//!
//! Only margin and total are bundled. A stored win probability is derived from
//! the margin (`projection::margin_to_win_prob`), so the sweep never runs a win
//! classifier.

use crate::inference::{FEATURE_NAMES, NUM_FEATURES, TOTAL_FEATURE_NAMES, TOTAL_NUM_FEATURES};
use ort::session::Session;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

/// Subdirectory of `MODEL_DIR` holding the held-out bundles.
pub const HELDOUT_DIR: &str = "heldout";
/// The held-out set's meta, inside [`HELDOUT_DIR`].
pub const HELDOUT_META: &str = "heldout_meta.json";
/// The full fit's meta, whose `seasons` define which seasons are trained-on.
pub const FULL_FIT_META: &str = "pit_model_meta.json";
/// Full-fit artifacts whose digests a held-out set is stamped against.
/// Mirrored in `train_heldout_game_models.py::FULL_FIT_ARTIFACTS`.
pub const FULL_FIT_ARTIFACTS: [&str; 2] = ["pit_margin_model.onnx", "pit_total_model.onnx"];
/// The only protocol this loader accepts: train strictly before the season.
pub const PROTOCOL: &str = "walk_forward";

#[derive(Debug, Deserialize)]
struct FullFitMeta {
    seasons: Vec<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeldOutMeta {
    pub protocol: String,
    pub full_fit: FullFitStamp,
    pub features: Vec<String>,
    pub total_features: Vec<String>,
    #[serde(default)]
    pub unscorable_seasons: Vec<i32>,
    pub bundles: Vec<BundleMeta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FullFitStamp {
    pub seasons: Vec<i32>,
    /// Artifact file name → SHA-256 hex.
    pub artifacts: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleMeta {
    pub season: i32,
    pub train_seasons: Vec<i32>,
    /// `"margin"` / `"total"` → file and digest.
    pub artifacts: BTreeMap<String, ArtifactMeta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactMeta {
    pub file: String,
    pub sha256: String,
}

/// One season's held-out margin and total models.
pub struct HeldOutBundle {
    pub season: i32,
    pub train_seasons: Vec<i32>,
    margin: Mutex<Session>,
    total: Mutex<Session>,
}

impl HeldOutBundle {
    pub fn predict_margin(&self, diff: &[f32; NUM_FEATURES]) -> Result<f32, ort::Error> {
        use ort::value::TensorRef;
        let input = TensorRef::from_array_view(([1_usize, NUM_FEATURES], diff.as_slice()))?;
        let mut session = self.margin.lock().unwrap();
        let outputs = session.run(ort::inputs![input])?;
        let (_, preds) = outputs[0].try_extract_tensor::<f32>()?;
        Ok(preds[0])
    }

    pub fn predict_total(
        &self,
        diff_and_sum: &[f32; TOTAL_NUM_FEATURES],
    ) -> Result<f32, ort::Error> {
        use ort::value::TensorRef;
        let input =
            TensorRef::from_array_view(([1_usize, TOTAL_NUM_FEATURES], diff_and_sum.as_slice()))?;
        let mut session = self.total.lock().unwrap();
        let outputs = session.run(ort::inputs![input])?;
        let (_, preds) = outputs[0].try_extract_tensor::<f32>()?;
        Ok(preds[0])
    }
}

/// Which model scores one season's stored projections.
pub enum SeasonModel<'a> {
    /// The season is not in the full fit's training data: serve it.
    FullFit,
    /// A trained-on season: the bundle fitted on the seasons before it.
    HeldOut(&'a HeldOutBundle),
    /// The first trained-on season. Nothing earlier exists to train on, so it
    /// gets no stored projection.
    NoEarlierSeason,
}

/// The routing decision without the bundle reference, so it can be tested
/// without loading a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    FullFit,
    HeldOut,
    NoEarlierSeason,
    /// A trained-on season with no usable bundle, and why.
    Refused(String),
}

/// Decide how `season` is scored. `has_bundle` reports whether a validated
/// bundle exists; `set_problem` is why the whole held-out set is unusable, if
/// it is.
pub fn route_season(
    season: i32,
    full_fit_seasons: &[i32],
    has_bundle: impl Fn(i32) -> bool,
    set_problem: Option<&str>,
) -> Route {
    if !full_fit_seasons.contains(&season) {
        return Route::FullFit;
    }
    if !full_fit_seasons.iter().any(|&s| s < season) {
        return Route::NoEarlierSeason;
    }
    if let Some(problem) = set_problem {
        return Route::Refused(problem.to_string());
    }
    if has_bundle(season) {
        Route::HeldOut
    } else {
        Route::Refused(format!(
            "no held-out bundle for {season} in {HELDOUT_DIR}/{HELDOUT_META}"
        ))
    }
}

/// Check a held-out meta against the compiled feature contract and the full
/// fit it claims to stand in for. Pure: digests are passed in.
pub fn validate_meta(
    meta: &HeldOutMeta,
    full_fit_seasons: &[i32],
    full_fit_digests: &BTreeMap<String, String>,
) -> Result<(), String> {
    if meta.protocol != PROTOCOL {
        return Err(format!(
            "protocol is {:?}, expected {PROTOCOL:?}",
            meta.protocol
        ));
    }
    if meta
        .features
        .iter()
        .map(String::as_str)
        .ne(FEATURE_NAMES.iter().copied())
    {
        return Err("margin feature list differs from the compiled contract".into());
    }
    if meta
        .total_features
        .iter()
        .map(String::as_str)
        .ne(TOTAL_FEATURE_NAMES.iter().copied())
    {
        return Err("total feature list differs from the compiled contract".into());
    }
    let mut stamped = meta.full_fit.seasons.clone();
    stamped.sort_unstable();
    let mut current = full_fit_seasons.to_vec();
    current.sort_unstable();
    if stamped != current {
        return Err(format!(
            "fitted for a full fit on {stamped:?}, but {FULL_FIT_META} lists {current:?}; \
             re-run train_heldout_game_models.py"
        ));
    }
    if &meta.full_fit.artifacts != full_fit_digests {
        return Err(format!(
            "stamped for different full-fit artifacts than the ones in MODEL_DIR \
             ({}); the game models were retrained without re-running \
             train_heldout_game_models.py",
            FULL_FIT_ARTIFACTS.join(", ")
        ));
    }
    for b in &meta.bundles {
        if !current.contains(&b.season) {
            return Err(format!(
                "bundle for {} is not a trained-on season",
                b.season
            ));
        }
        match b.train_seasons.iter().max() {
            None => return Err(format!("bundle for {} has no training seasons", b.season)),
            Some(&latest) if latest >= b.season => {
                return Err(format!(
                    "bundle for {} was trained on {latest}, which is not before it",
                    b.season
                ));
            }
            Some(_) => {}
        }
        for head in ["margin", "total"] {
            if !b.artifacts.contains_key(head) {
                return Err(format!("bundle for {} has no {head} artifact", b.season));
            }
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

fn load_session(path: &Path) -> Result<Session, String> {
    let load = || -> Result<Session, ort::Error> {
        Session::builder()?
            .with_intra_threads(1)?
            .commit_from_file(path)
    };
    load().map_err(|e| format!("loading {}: {e}", path.display()))
}

/// Loads and validates the whole set; any failure makes every trained-on
/// season refuse, with this message.
fn load_set(model_dir: &Path, full_fit_seasons: &[i32]) -> Result<Vec<HeldOutBundle>, String> {
    let dir = model_dir.join(HELDOUT_DIR);
    let meta_path = dir.join(HELDOUT_META);
    let raw = std::fs::read_to_string(&meta_path)
        .map_err(|e| format!("no held-out set ({}: {e})", meta_path.display()))?;
    let meta: HeldOutMeta =
        serde_json::from_str(&raw).map_err(|e| format!("parsing {}: {e}", meta_path.display()))?;

    let mut digests = BTreeMap::new();
    for name in FULL_FIT_ARTIFACTS {
        digests.insert(name.to_string(), sha256_file(&model_dir.join(name))?);
    }
    validate_meta(&meta, full_fit_seasons, &digests)?;

    let mut bundles = Vec::with_capacity(meta.bundles.len());
    for b in &meta.bundles {
        let session = |head: &str| -> Result<Session, String> {
            let a = &b.artifacts[head];
            let path = dir.join(&a.file);
            let actual = sha256_file(&path)?;
            if actual != a.sha256 {
                return Err(format!(
                    "{} does not match the digest in {HELDOUT_META}",
                    path.display()
                ));
            }
            load_session(&path)
        };
        bundles.push(HeldOutBundle {
            season: b.season,
            train_seasons: b.train_seasons.clone(),
            margin: Mutex::new(session("margin")?),
            total: Mutex::new(session("total")?),
        });
    }
    Ok(bundles)
}

/// Picks the model for each season's stored projections. See the module doc.
pub struct ModelRouter {
    full_fit_seasons: Vec<i32>,
    bundles: BTreeMap<i32, HeldOutBundle>,
    /// Why the held-out set is unusable, when it is. Only consulted for a
    /// trained-on season, so a deploy without the set still sweeps the season
    /// being played.
    set_problem: Option<String>,
}

impl ModelRouter {
    /// Read the full fit's seasons and the held-out set from `model_dir`.
    ///
    /// Errors only when the full fit's own meta is unreadable: without it
    /// there is no way to tell a trained-on season from a live one, and
    /// guessing "live" is the leak this module exists to stop. A missing or
    /// stale held-out set is not an error here; it surfaces from
    /// [`ModelRouter::route`] for the seasons that need it.
    pub fn load(model_dir: &Path) -> Result<Self, String> {
        let meta_path = model_dir.join(FULL_FIT_META);
        let raw = std::fs::read_to_string(&meta_path)
            .map_err(|e| format!("reading {}: {e}", meta_path.display()))?;
        let full: FullFitMeta = serde_json::from_str(&raw)
            .map_err(|e| format!("parsing {}: {e}", meta_path.display()))?;
        let (bundles, set_problem) = match load_set(model_dir, &full.seasons) {
            Ok(v) => (v.into_iter().map(|b| (b.season, b)).collect(), None),
            Err(e) => (BTreeMap::new(), Some(e)),
        };
        Ok(Self {
            full_fit_seasons: full.seasons,
            bundles,
            set_problem,
        })
    }

    pub fn full_fit_seasons(&self) -> &[i32] {
        &self.full_fit_seasons
    }

    /// Why the held-out set could not be used, if it could not.
    pub fn set_problem(&self) -> Option<&str> {
        self.set_problem.as_deref()
    }

    /// The model for `season`'s stored projections, or why there is none.
    pub fn route(&self, season: i32) -> Result<SeasonModel<'_>, String> {
        match route_season(
            season,
            &self.full_fit_seasons,
            |s| self.bundles.contains_key(&s),
            self.set_problem.as_deref(),
        ) {
            Route::FullFit => Ok(SeasonModel::FullFit),
            Route::NoEarlierSeason => Ok(SeasonModel::NoEarlierSeason),
            Route::HeldOut => Ok(SeasonModel::HeldOut(&self.bundles[&season])),
            Route::Refused(why) => Err(format!(
                "{season} is in the game models' training data, so its stored projections \
                 need a model that has not seen it: {why}"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEASONS: [i32; 4] = [2015, 2016, 2017, 2018];

    #[test]
    fn untrained_season_is_served_by_the_full_fit() {
        assert_eq!(
            route_season(2027, &SEASONS, |_| false, Some("x")),
            Route::FullFit
        );
    }

    #[test]
    fn first_trained_season_gets_nothing() {
        assert_eq!(
            route_season(2015, &SEASONS, |_| true, None),
            Route::NoEarlierSeason
        );
    }

    #[test]
    fn trained_season_uses_its_bundle_or_refuses() {
        assert_eq!(route_season(2017, &SEASONS, |_| true, None), Route::HeldOut);
        assert!(matches!(
            route_season(2017, &SEASONS, |_| false, None),
            Route::Refused(_)
        ));
        // A broken set refuses even where a bundle exists, rather than
        // falling back to the full fit.
        assert!(matches!(
            route_season(2017, &SEASONS, |_| true, Some("stale")),
            Route::Refused(_)
        ));
    }

    fn meta(bundles: Vec<(i32, Vec<i32>)>) -> (HeldOutMeta, BTreeMap<String, String>) {
        let digests: BTreeMap<String, String> = FULL_FIT_ARTIFACTS
            .iter()
            .map(|n| (n.to_string(), format!("digest-{n}")))
            .collect();
        let art = |s: i32| -> BTreeMap<String, ArtifactMeta> {
            ["margin", "total"]
                .iter()
                .map(|h| {
                    (
                        h.to_string(),
                        ArtifactMeta {
                            file: format!("pit_{s}_{h}_model.onnx"),
                            sha256: String::new(),
                        },
                    )
                })
                .collect()
        };
        let m = HeldOutMeta {
            protocol: PROTOCOL.into(),
            full_fit: FullFitStamp {
                seasons: SEASONS.to_vec(),
                artifacts: digests.clone(),
            },
            features: FEATURE_NAMES.iter().map(|s| s.to_string()).collect(),
            total_features: TOTAL_FEATURE_NAMES.iter().map(|s| s.to_string()).collect(),
            unscorable_seasons: vec![2015],
            bundles: bundles
                .into_iter()
                .map(|(season, train_seasons)| BundleMeta {
                    season,
                    train_seasons,
                    artifacts: art(season),
                })
                .collect(),
        };
        (m, digests)
    }

    #[test]
    fn walk_forward_meta_validates() {
        let (m, d) = meta(vec![(2016, vec![2015]), (2017, vec![2015, 2016])]);
        validate_meta(&m, &SEASONS, &d).unwrap();
    }

    #[test]
    fn a_bundle_that_saw_its_season_is_rejected() {
        let (m, d) = meta(vec![(2017, vec![2015, 2016, 2017])]);
        assert!(
            validate_meta(&m, &SEASONS, &d)
                .unwrap_err()
                .contains("not before")
        );
        // Trained on a LATER season (LOSO, not walk-forward) is also refused.
        let (m, d) = meta(vec![(2016, vec![2015, 2018])]);
        assert!(validate_meta(&m, &SEASONS, &d).is_err());
    }

    #[test]
    fn a_set_for_another_full_fit_is_rejected() {
        let (m, mut d) = meta(vec![(2016, vec![2015])]);
        d.insert("pit_margin_model.onnx".into(), "retrained".into());
        assert!(
            validate_meta(&m, &SEASONS, &d)
                .unwrap_err()
                .contains("retrained without")
        );
        let (m, d) = meta(vec![(2016, vec![2015])]);
        assert!(validate_meta(&m, &[2015, 2016, 2017, 2018, 2019], &d).is_err());
    }

    /// The committed held-out set must load against the committed full fit.
    /// This is what fails CI when the game models are retrained and
    /// `train_heldout_game_models.py` is not re-run: the digests no longer
    /// match, and every trained-on season would refuse to sweep.
    #[test]
    fn committed_heldout_set_matches_the_committed_full_fit() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../training/models");
        let router = ModelRouter::load(&dir).expect("read pit_model_meta.json");
        assert_eq!(router.set_problem(), None, "held-out set unusable");

        let seasons = router.full_fit_seasons().to_vec();
        let first = *seasons.iter().min().unwrap();
        assert!(matches!(
            router.route(first),
            Ok(SeasonModel::NoEarlierSeason)
        ));
        assert!(matches!(
            router.route(seasons.iter().max().unwrap() + 1),
            Ok(SeasonModel::FullFit)
        ));
        for &season in seasons.iter().filter(|&&s| s != first) {
            let Ok(SeasonModel::HeldOut(b)) = router.route(season) else {
                panic!("{season} has no held-out bundle");
            };
            assert!(b.train_seasons.iter().all(|&s| s < season));
            let m = b.predict_margin(&[0.0; NUM_FEATURES]).unwrap();
            let t = b.predict_total(&[0.0; TOTAL_NUM_FEATURES]).unwrap();
            assert!(
                m.abs() < 20.0 && (50.0..=250.0).contains(&t),
                "{season}: {m} / {t}"
            );
        }
    }

    #[test]
    fn feature_contract_drift_is_rejected() {
        let (mut m, d) = meta(vec![(2016, vec![2015])]);
        m.features.swap(2, 3);
        assert!(validate_meta(&m, &SEASONS, &d).is_err());
    }
}
