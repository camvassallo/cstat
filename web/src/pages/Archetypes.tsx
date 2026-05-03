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
      'Elite lead-guard creator. Heaviest minutes of any class, paired with strong assist rates and positive offensive impact — the team runs through them at the perimeter.',
    signature: ['heavy minutes', 'high AST%', 'high OGBPM'],
    comparable: 'All-American floor generals',
  },
  {
    name: 'Sorcerer',
    description:
      'High-volume star scorer. Highest-USG class in the dataset paired with strong offensive impact and heavy minutes — they shoot, attack, and finish in roughly equal measure.',
    signature: ['highest USG%', 'high OGBPM', 'heavy minutes'],
    comparable: 'Lottery-pick alphas',
  },
  {
    name: 'Warlock',
    description:
      'Three-point specialist. Over 70% of their shots come from outside — the heaviest 3PA share of any class. Modest usage, lowest rim rate, boom-or-bust scoring.',
    signature: ['heaviest 3PA share', 'lowest rim share', 'boom-or-bust'],
    comparable: 'Microwave shooters / knockdown bombers',
  },
  {
    name: 'Bard',
    description:
      'Pass-first distributor. High assist rate paired with the lowest usage in the dataset — they\'d rather set up a teammate than score. Modest impact rather than star-level.',
    signature: ['high AST%', 'low USG%', 'modest impact'],
    comparable: 'Backup point guards',
  },
  {
    name: 'Ranger',
    description:
      'Perimeter spacer. Above-average 3PA share and steal rate at low usage — often a role-player shooter or rotation wing rather than a true two-way starter.',
    signature: ['high 3PA share', 'high STL%', 'low USG%'],
    comparable: 'Bench shooters / role wings',
  },
  {
    name: 'Barbarian',
    description:
      'Interior finisher. Highest rim share of any class paired with the lowest 3PA — they live near the basket. Low usage, often a high-block-rate physical big.',
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
      'Disciplined wing star. High-floor scorer who shoots from outside, plays the heaviest minutes outside Wizard, posts strong OGBPM, and doesn\'t turn the ball over.',
    signature: ['high OGBPM', 'heavy minutes', 'high 3PA share'],
    comparable: 'All-Conference scoring wings',
  },
  {
    name: 'Cleric',
    description:
      'Low-volume interior connector. Plays inside the arc — rim and midrange — at low usage. Doesn\'t dominate any category; modest contributor without standing out on either end.',
    signature: ['rim/mid finisher', 'low USG%', 'modest impact'],
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
      'Disruptive two-way wing. Strong DGBPM with above-average steal AND block rates simultaneously — opportunistic, off-ball, plays heavy minutes.',
    signature: ['high STL%', 'high BLK%', 'high DGBPM'],
    comparable: 'Defensive Swiss-army wings',
  },
  {
    name: 'Fighter',
    description:
      'Balanced two-way rotation. Modest positives on creation, defense, and impact across multiple axes without elite production in any one — the plug-and-play rotation wing.',
    signature: ['multi-axis positives', 'rotation minutes', 'no specialty'],
    comparable: 'Rotation wings / utility players',
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
            to={`/players?archetype=${encodeURIComponent(def.name)}`}
            className="text-xl font-bold hover:underline"
            style={{ color }}
            title={`See all ${def.name}s ranked by CamPom`}
          >
            {def.name}
          </SeasonLink>
          {info != null && (
            <SeasonLink
              to={`/players?archetype=${encodeURIComponent(def.name)}`}
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
              className="text-[10px] font-bold uppercase tracking-wide px-2 py-0.5 rounded"
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
          Every qualified D-I player is clustered into one of twelve D&amp;D-flavored
          classes based on their shot diet, rate stats, impact metrics, and minutes
          share. Clusters come from k-means in standardized feature space; each
          centroid is matched to the archetype it best resembles. Cards are ordered
          by mean CamPom (the site's canonical player valuation), highest to lowest;
          exemplars within each class are ranked the same way.
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
