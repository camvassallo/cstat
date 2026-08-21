import { useEffect, useState } from 'react';
import { fetchArchetypes, type ArchetypeClassInfo } from '../api/client';
import { classColor, classTagline } from '../components/archetypeColors';
import { SeasonLink } from '../components/SeasonLink';
import { useSeason } from '../components/season';
import { usePageTitle } from '../components/usePageTitle';

interface ClassDef {
  name: string;
  description: string;
  signature: string[];   // "high X" / "low Y" badges
  comparable: string;    // pro / college parallel
}

// Hand-written descriptions paired with the signatures in
// `training/archetypes.py` and `archetypeColors.ts::CLASS_TAGLINES` (single
// source of truth for the one-liners shown on hover tooltips). Keep all three
// in sync — description lengths roughly even so cards line up. Numbers and
// phrasing reflect the combined-cohort cluster centroids; rerun
// `python -m training.archetypes --diagnostics` to see current means.
const CLASS_DEFS: ClassDef[] = [
  {
    name: 'Wizard',
    description:
      'Elite lead-guard creator. Highest AST% in the dataset paired with heavy minutes and positive two-way impact — the offense runs through them and they don\'t cost you on the other end. POY-shortlist floor general.',
    signature: ['highest AST%', 'heavy minutes', 'positive two-way'],
    comparable: 'All-American floor generals',
  },
  {
    name: 'Sorcerer',
    description:
      'High-volume star scorer. Strong offensive impact and heavy minutes with low assist rates — they hunt their own shots rather than create for teammates. The featured wing or guard who carries scoring load on a good team.',
    signature: ['high OGBPM', 'heavy minutes', 'shot hunter'],
    comparable: 'Lottery-pick alphas',
  },
  {
    name: 'Warlock',
    description:
      'Three-point specialist. Over 70% of their shots come from outside — the heaviest 3PA share of any class. Low usage and lowest rim rate; catch-and-shoot role player rather than primary creator.',
    signature: ['heaviest 3PA share', 'lowest rim share', 'low USG%'],
    comparable: 'Microwave shooters / knockdown bombers',
  },
  {
    name: 'Bard',
    description:
      'Mid-major primary scorer. Plays heavy minutes at high usage as the team\'s only real offensive option — modest passing, but the bulk of the shot diet runs through them. Positive offensive impact relative to their tier.',
    signature: ['heavy minutes', 'high USG%', 'positive OGBPM'],
    comparable: 'Mid-major leading scorers',
  },
  {
    name: 'Ranger',
    description:
      'Perimeter spacer. Above-average 3PA share at low usage and rotation minutes — a role-player shooter rather than a two-way starter. Shoots from outside; doesn\'t generate much else.',
    signature: ['high 3PA share', 'low USG%', 'rotation minutes'],
    comparable: 'Bench shooters / role wings',
  },
  {
    name: 'Barbarian',
    description:
      'Interior finisher. Highest rim share of any class paired with the lowest 3PA — they live near the basket. Low usage; a physical big who gets fed at the rim and blocks shots on the other end.',
    signature: ['highest rim share', 'lowest 3PA share', 'high BLK%'],
    comparable: 'Energy bigs / dunker-spot finishers',
  },
  {
    name: 'Paladin',
    description:
      'Defensive anchor. Elite block rate plus the strongest DGBPM of any class — the rim protector. Low offensive usage, but the wall in the paint.',
    signature: ['elite BLK%', 'highest DGBPM', 'rim defense'],
    comparable: 'Defensive bigs / shot-blockers',
  },
  {
    name: 'Monk',
    description:
      'Versatile rotation forward. Balanced between rim and three at moderate usage and rotation minutes — a stretch four or hybrid forward who can step out to shoot or finish inside. Flexible role player, not a primary option.',
    signature: ['balanced shot diet', 'rotation minutes', 'stretch-four flex'],
    comparable: 'Stretch fours / versatile rotation forwards',
  },
  {
    name: 'Cleric',
    description:
      'Low-volume backup big. Plays inside the arc — rim and midrange — at low usage with solid rebounding. Fills paint minutes without dominating any single column.',
    signature: ['interior rebounder', 'low USG%', 'rotation minutes'],
    comparable: 'Backup bigs / glue forwards',
  },
  {
    name: 'Druid',
    description:
      'Elite two-way big. The highest combined offensive and defensive impact in the dataset — owns the glass, finishes through contact at the rim, contributes on both ends. POY-shortlist territory.',
    signature: ['highest OGBPM', 'high DGBPM', 'elite rebounding'],
    comparable: 'POY-shortlist bigs / lottery picks',
  },
  {
    name: 'Rogue',
    description:
      'Disruptive two-way wing. Highest steal rate of any class paired with strong defensive impact — an off-ball event creator on defense. Modest usage on offense but starter minutes on good teams.',
    signature: ['highest STL%', 'high DGBPM', 'off-ball defender'],
    comparable: 'Defensive Swiss-army wings',
  },
  {
    name: 'Fighter',
    description:
      'Low-USG rotation depth. Above-average assist rate at very low usage and rotation minutes — a backup guard who can run an offense in short bursts without standing out anywhere. Steady, unspectacular, plug-and-play.',
    signature: ['rotation minutes', 'low USG%', 'high AST%'],
    comparable: 'Backup point guards / rotation depth',
  },
];

function ClassCard({ def, info }: { def: ClassDef; info: ArchetypeClassInfo | null }) {
  const color = classColor(def.name);
  return (
    <div
      className="bg-gray-800 rounded-lg overflow-hidden border-l-4 flex flex-col"
      style={{ borderLeftColor: color }}
    >
      <div className="p-4 border-b border-gray-700/60">
        <div className="flex items-baseline justify-between gap-3">
          <SeasonLink
            to={`/players?archetypes=${encodeURIComponent(def.name)}`}
            className="text-xl font-bold hover:underline"
            style={{ color }}
            title={`See all ${def.name}s ranked by CAM`}
          >
            {def.name}
          </SeasonLink>
          {info != null && (
            <SeasonLink
              to={`/players?archetypes=${encodeURIComponent(def.name)}`}
              className="text-xs text-gray-400 shrink-0 hover:text-gray-200 hover:underline"
            >
              {info.count.toLocaleString()} players →
            </SeasonLink>
          )}
        </div>
        <div className="text-sm text-gray-300 mt-0.5">{classTagline(def.name)}</div>
      </div>
      <div className="p-4 flex-1 space-y-3">
        <p className="text-sm text-gray-300 leading-relaxed">{def.description}</p>

        <div className="flex flex-wrap gap-1.5">
          {def.signature.map((trait) => (
            <span
              key={trait}
              className="text-xs font-bold uppercase tracking-wide px-2 py-0.5 rounded"
              style={{ background: color + '22', color }}
            >
              {trait}
            </span>
          ))}
        </div>

        <div className="text-xs text-gray-500">
          <span className="text-gray-400">Comparable: </span>
          {def.comparable}
        </div>

        {info && info.exemplars.length > 0 && (
          <div className="pt-2 border-t border-gray-700/60">
            <div className="text-[10px] uppercase tracking-wider text-gray-500 mb-2">
              Top Exemplars
            </div>
            <div className="space-y-1">
              {info.exemplars.map((ex) => (
                <SeasonLink
                  key={ex.player_id}
                  to={`/players/${ex.player_id}`}
                  className="flex items-center justify-between text-xs hover:bg-gray-700/40 rounded px-1.5 py-1 -mx-1.5"
                >
                  <span className="truncate">
                    <span className="font-medium">{ex.name}</span>
                    <span className="text-gray-500"> — {ex.team_name ?? '—'}</span>
                  </span>
                </SeasonLink>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default function Archetypes() {
  const { season: selectedSeason } = useSeason();
  usePageTitle('Player Archetypes');
  const [classes, setClasses] = useState<ArchetypeClassInfo[]>([]);
  // The API echoes back the season it actually served (defends against drift
  // between the URL param and what the server resolves) — keep displaying
  // that one in the page chrome.
  const [season, setSeason] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    // No `setLoading(true)` here — see Rankings.tsx for the rationale.
    fetchArchetypes(5, selectedSeason)
      .then((r) => {
        setClasses(r.classes);
        setSeason(r.season);
      })
      .catch((e) => setError(e.message ?? 'Failed to load archetypes'))
      .finally(() => setLoading(false));
  }, [selectedSeason]);

  const defsByName = new Map(CLASS_DEFS.map((d) => [d.name, d]));
  // API returns classes sorted by mean GBPM desc — render in that order so the
  // page reads from highest two-way impact down to lowest.
  const ordered = classes
    .map((info) => ({ info, def: defsByName.get(info.name) }))
    .filter((x): x is { info: ArchetypeClassInfo; def: ClassDef } => !!x.def);
  const totalPlayers = classes.reduce((s, c) => s + c.count, 0);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-bold">Player Archetypes</h1>
        <p className="text-sm text-gray-400 mt-1 max-w-3xl">
          Every qualified D-I player, clustered into one of twelve classes by shot
          diet, rate stats, impact, and minutes share. Ordered by mean CAM.
        </p>
        {!loading && season != null && (
          <p className="text-xs text-gray-500 mt-2">
            {season - 1}-{String(season).slice(2)} season ·{' '}
            {totalPlayers.toLocaleString()} players · ≥10 GP, ≥10 MPG
          </p>
        )}
      </div>

      {error && <div className="text-red-400 text-sm">{error}</div>}
      {loading && <div className="text-gray-400 text-sm">Loading…</div>}

      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
        {ordered.length > 0
          ? ordered.map(({ def, info }) => (
              <ClassCard key={def.name} def={def} info={info} />
            ))
          : // Fallback: API not loaded yet — render the static defs.
            CLASS_DEFS.map((def) => (
              <ClassCard key={def.name} def={def} info={null} />
            ))}
      </div>

      <div className="bg-gray-800/50 border border-gray-700 rounded-lg p-4 text-xs text-gray-400 leading-relaxed">
        <div className="font-bold text-gray-300 mb-1">How it works</div>
        Features used: shot zone share (rim / mid / three), AST%, TOV%, USG%,
        ORB%, DRB%, STL%, BLK%, FT Rate, OGBPM, DGBPM, minutes share. Values are
        z-standardized, then k-means with k={CLASS_DEFS.length} runs on the qualified
        cohort. Each centroid is matched to a class via Hungarian assignment against
        hand-written signature templates, so the labels are consistent across runs.
        Affinity scores in the badge tooltip are softmax over negative distance to
        each centroid.
      </div>
    </div>
  );
}
