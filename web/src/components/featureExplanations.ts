/// Plain-English definitions for the 49 features the Predict model uses,
/// keyed by the raw feature name (e.g. "diff_w_gbpm"). Powers the info
/// tooltips in the contributions panel. Keep each definition to 1–2
/// sentences — the tooltip is a 64-tailwind-unit popover, not a prose box.
///
/// Update alongside `crates/cstat-core/src/inference.rs::FEATURE_META` if
/// you add or rename a feature.
export const FEATURE_EXPLANATIONS: Record<string, string> = {
  // Context
  venue: 'Home court flag (1 if a team is hosting, 0 for neutral site). Captures the typical 2–3 point home-court bump.',
  is_conference_game: '1 if both teams are in the same conference, 0 otherwise. Conference games tend to be tighter and lower-margin.',
  diff_win_pct:
    'Win-percentage gap, season-to-date (home minus away). +0.10 means the home team wins 10 percentage points more often than the away team.',

  // Adjusted efficiency (KenPom-style)
  diff_adj_offense:
    'Adjusted offensive efficiency: points scored per 100 possessions, adjusted for opponent defense. Higher = better offense.',
  diff_adj_defense:
    'Adjusted defensive efficiency: points allowed per 100 possessions, adjusted for opponent offense. Sign is flipped here, so positive = home defense is better.',
  diff_adj_efficiency_margin:
    'AdjO minus AdjD — net points per 100 possessions adjusted for opponent quality. The headline KenPom-style rating.',

  // Four factors (offense)
  diff_effective_fg_pct:
    'Effective FG%: shooting efficiency adjusted for the extra value of 3-pointers. Single best predictor of offensive output.',
  diff_turnover_pct: 'Turnovers per possession on offense. Lower is better — diff is home minus away.',
  diff_off_rebound_pct:
    'Offensive rebound rate — % of available offensive rebounds the team collects. Extra possessions = extra points.',
  diff_ft_rate:
    'Free throw attempts per field goal attempt. Measures how often a team gets to the line — drawing fouls, attacking the rim.',

  // Four factors (defense)
  diff_opp_effective_fg_pct:
    'Opponent eFG% allowed. Sign is flipped, so positive = home defense holds opponents to worse shooting.',
  diff_opp_turnover_pct:
    'How often the defense forces turnovers (per opponent possession). Higher = better.',
  diff_def_rebound_pct:
    'Defensive rebound rate — % of opponent misses the team secures. Stops second-chance points.',
  diff_opp_ft_rate:
    'How often the defense fouls (opponent FTA / FGA). Sign is flipped, so positive = the home defense fouls less.',

  // Pace
  diff_adj_tempo: 'Possessions per 40 minutes, adjusted for opponent pace. Higher = faster game.',

  // Strength of schedule
  diff_sos:
    'Strength of schedule, computed from the season-to-date adjusted efficiencies of each team\'s opponents.',
  diff_w_player_sos:
    'Player-level strength of schedule, minutes-weighted from Bart Torvik. Captures who actually faced the tougher opponents on the floor.',

  // Power ratings
  diff_elo:
    'NatStat ELO rating — head-to-head + margin-of-victory power rating. Updates after every game.',
  diff_point_diff:
    'Average point differential per game (cumulative). Brute-force "how dominant has this team been".',
  diff_pythag_win_pct:
    'Bill James pythagorean win expectation, derived from points scored vs allowed. Strips out close-game luck.',
  diff_road_win_pct:
    'Win rate in road and neutral games. Proxy for "can this team win away from home" — useful for tournament-style settings.',

  // Roster aggregate (minutes-weighted across qualified players ≥5 GP, ≥10 MPG)
  diff_roster_size: 'Number of qualified rotation players (≥5 games, ≥10 minutes per game). Higher = deeper rotation.',
  diff_w_ppg: 'Roster points per game, minutes-weighted across rotation players.',
  diff_w_rpg: 'Roster rebounds per game, minutes-weighted.',
  diff_w_apg: 'Roster assists per game, minutes-weighted.',
  diff_w_spg: 'Roster steals per game, minutes-weighted.',
  diff_w_bpg: 'Roster blocks per game, minutes-weighted.',
  diff_w_topg: 'Roster turnovers per game, minutes-weighted. Lower is better, but the diff is reported home minus away.',
  diff_w_ts_pct:
    'True shooting % across the roster — efficiency including FG, 3P, and FT. The cleanest single shooting metric.',
  diff_w_efg_pct: 'Effective FG% across the roster, minutes-weighted.',
  diff_w_usage:
    'Average usage rate across the roster — what share of possessions players end with a shot, foul, or turnover.',
  diff_w_ortg:
    'Roster offensive rating — points produced per 100 possessions while on the floor, minutes-weighted (Torvik).',
  diff_w_ast_pct:
    'Roster assist rate — % of teammate FGs assisted while a player is on the floor, minutes-weighted.',
  diff_w_tov_pct: 'Roster turnover rate per 100 plays, minutes-weighted. Lower = better ball security.',
  diff_w_stl_pct: 'Roster steal rate — % of opponent possessions ended via steal while on floor.',
  diff_w_blk_pct: 'Roster block rate — % of opponent 2-point attempts blocked while on floor.',
  diff_minutes_stddev:
    'Spread of minutes across the rotation. High = top-heavy team that leans on its starters; low = balanced rotation.',

  // Roster impact (Bart Torvik GBPM cluster — the model\'s top features)
  diff_w_gbpm:
    'Game-Based Plus/Minus from Bart Torvik — a holistic per-100-possession player impact metric, similar to NBA BPM. Minutes-weighted across the roster. The model\'s strongest signal.',
  diff_w_ogbpm: 'Offensive half of GBPM (roster-weighted) — captures shot creation, scoring, and playmaking impact.',
  diff_w_dgbpm: 'Defensive half of GBPM (roster-weighted) — captures rim protection, steals, and defensive rebounding.',

  // Star player (highest-minutes player on each team)
  diff_star_ppg: 'Highest-minutes player\'s points per game.',
  diff_star_gbpm: 'Highest-minutes player\'s GBPM. Captures star-level upside.',
  diff_star_ogbpm: 'Highest-minutes player\'s offensive GBPM.',
  diff_star_dgbpm: 'Highest-minutes player\'s defensive GBPM.',
  diff_star_ortg: 'Highest-minutes player\'s offensive rating.',

  // Recent form (last-5-game rolling, minutes-weighted)
  diff_w_rolling_gs:
    'Last-5-game rolling game score, minutes-weighted across the roster. A "how is this team playing right now" signal.',
  diff_w_rolling_ts: 'Last-5-game rolling true shooting %, minutes-weighted.',
  diff_w_ppg_trend:
    'Recent PPG minus season PPG. Positive = team is heating up; negative = cooling off.',
  diff_w_gs_trend: 'Recent game score minus season game score. Same idea as PPG trend but on the broader impact metric.',
};

/// Group-level explanations rendered next to each section title.
export const GROUP_EXPLANATIONS: Record<string, string> = {
  Context: 'Game setup — home-court advantage, conference matchup, season-to-date win pct.',
  'Adjusted efficiency':
    'KenPom-style ratings that strip out opponent strength to estimate true offensive and defensive efficiency.',
  'Four factors (offense)':
    'Dean Oliver\'s four most predictive offensive stats: shooting (eFG%), turnovers, offensive rebounds, free throws.',
  'Four factors (defense)':
    'The four factors flipped to the defensive side — same stats, opponent\'s perspective.',
  Pace: 'Possessions per 40 minutes. Tempo affects raw scoring totals but not efficiency-based predictions.',
  'Strength of schedule':
    'Quality of opponents faced — both team-level (from adjusted efficiency) and player-level (minutes-weighted, Torvik).',
  'Power ratings':
    'Single-number season summaries: ELO, point differential, pythagorean win%, road win rate. Different ways to compress a season into one signal.',
  'Roster aggregate':
    'Minutes-weighted box-score and rate stats across the rotation. Captures depth and balance — what the bench plus starters look like together.',
  'Roster impact':
    'Bart Torvik\'s GBPM — a holistic per-100-possession player impact metric, roster-weighted. The model\'s top three features all live here.',
  'Star player':
    'Stats for the highest-minutes player on each team. Captures top-end upside; one star can swing a game even if rosters are similar.',
  'Recent form':
    'Last-5-game rolling averages and trends vs season averages — has the team been getting better or worse?',
};
