import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { fetchArchetypes, type ArchetypeClassInfo } from '../api/client';
import { classColor } from '../components/archetypeColors';

interface ClassDef {
  name: string;
  tagline: string;
  description: string;
  signature: string[];   // "high X" / "low Y" badges
  comparable: string;    // pro / college parallel
}

// Hand-written descriptions paired with the signatures in
// `training/archetypes.py`. Keep these in sync if the cluster taxonomy changes.
const CLASS_DEFS: ClassDef[] = [
  {
    name: 'Wizard',
    tagline: 'Pure floor general.',
    description:
      'The conductor. Elite assist rate paired with low turnovers and heavy minutes — they orchestrate every possession, taking few shots at the rim themselves.',
    signature: ['high AST%', 'low TOV%', 'heavy minutes'],
    comparable: 'Steady starting point guards',
  },
  {
    name: 'Sorcerer',
    tagline: 'Star scorer.',
    description:
      'The team\'s primary creator and finisher. High usage, high impact (OGBPM), big minutes. Everything runs through them.',
    signature: ['highest USG%', 'high OGBPM', 'heavy minutes'],
    comparable: 'Lottery-pick alphas',
  },
  {
    name: 'Warlock',
    tagline: 'Chaos gunner.',
    description:
      'High-variance volume shooter from beyond the arc. Heavy 3PA share with above-average usage and a willingness to live and die from deep.',
    signature: ['heavy 3PA share', 'high USG%', 'low rim share'],
    comparable: 'Microwave 6th men',
  },
  {
    name: 'Bard',
    tagline: 'Pass-first playmaker.',
    description:
      'Distributes more than they hunt. High AST% with modest usage — they\'d rather set up a teammate than take the shot themselves.',
    signature: ['high AST%', 'low USG%', 'positive OGBPM'],
    comparable: 'Backup point guards & connectors',
  },
  {
    name: 'Ranger',
    tagline: '3-and-D wing.',
    description:
      'The complementary perimeter piece. Lives behind the arc and racks up steals on the other end without dominating the ball.',
    signature: ['heavy 3PA share', 'high STL%', 'low USG%'],
    comparable: 'Switchable wings',
  },
  {
    name: 'Barbarian',
    tagline: 'Rim attacker.',
    description:
      'Drives, dunks, and gets fouled. The highest free-throw rate in the dataset — they earn their points by going through people, not around them.',
    signature: ['highest FT Rate', 'high rim share', 'low 3PA share'],
    comparable: 'Bully-ball forwards',
  },
  {
    name: 'Paladin',
    tagline: 'Two-way anchor.',
    description:
      'The rim protector and defensive leader. Elite block rate paired with strong DGBPM and defensive rebounding. The wall in the paint.',
    signature: ['elite BLK%', 'high DGBPM', 'strong DRB%'],
    comparable: 'Defensive bigs / shot-blockers',
  },
  {
    name: 'Monk',
    tagline: 'Efficient role player.',
    description:
      'Doesn\'t make mistakes. Lowest TOV rate in the dataset, modest usage, positive impact. The "play 30 minutes, post a clean line" archetype.',
    signature: ['lowest TOV%', 'modest USG%', 'positive OGBPM'],
    comparable: 'Steady veterans',
  },
  {
    name: 'Cleric',
    tagline: 'Glue connector.',
    description:
      'Holds the rotation together without dominating any column. Defensive rebounds, occasional creation, low usage — the lineup just works better with them on the floor.',
    signature: ['solid DRB%', 'modest DGBPM', 'low USG%'],
    comparable: 'High-IQ role players',
  },
  {
    name: 'Druid',
    tagline: 'Stretch big.',
    description:
      'Plays inside and out. Real three-point volume paired with rebounding and shot-blocking — too quick for traditional bigs, too physical for wings.',
    signature: ['high BLK%', '3PA share', 'strong rebounding'],
    comparable: 'Modern stretch fours',
  },
  {
    name: 'Rogue',
    tagline: 'Event creator.',
    description:
      'Disruptive on defense. Above-average steal AND block rate simultaneously — opportunistic, off-ball, makes things happen without dominating possessions.',
    signature: ['high STL%', 'high BLK%', 'low USG%'],
    comparable: 'Defensive Swiss-army wings',
  },
  {
    name: 'Fighter',
    tagline: 'Balanced two-way.',
    description:
      'No single specialty. Solid contributors across the board without elite production in any one area — the catch-all for players who don\'t fit a sharper mold.',
    signature: ['no specialty', 'positive OGBPM/DGBPM', 'steady minutes'],
    comparable: 'Plug-and-play rotation pieces',
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
          <h2 className="text-xl font-bold" style={{ color }}>
            {def.name}
          </h2>
          {info != null && (
            <span className="text-xs text-gray-400">
              {info.count.toLocaleString()} players
            </span>
          )}
        </div>
        <div className="text-sm text-gray-300 mt-0.5">{def.tagline}</div>
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
                <Link
                  key={ex.player_id}
                  to={`/players/${ex.player_id}`}
                  className="flex items-center justify-between text-xs hover:bg-gray-700/40 rounded px-1.5 py-1 -mx-1.5"
                >
                  <span className="truncate">
                    <span className="font-medium">{ex.name}</span>
                    <span className="text-gray-500"> — {ex.team_name ?? '—'}</span>
                  </span>
                  <span className="text-gray-500 text-[10px] ml-2 shrink-0">
                    {(ex.primary_score * 100).toFixed(0)}%
                  </span>
                </Link>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default function Archetypes() {
  const [classes, setClasses] = useState<ArchetypeClassInfo[]>([]);
  const [season, setSeason] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    fetchArchetypes(5)
      .then((r) => {
        setClasses(r.classes);
        setSeason(r.season);
      })
      .catch((e) => setError(e.message ?? 'Failed to load archetypes'))
      .finally(() => setLoading(false));
  }, []);

  const byName = new Map(classes.map((c) => [c.name, c]));
  const totalPlayers = classes.reduce((s, c) => s + c.count, 0);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-bold">Player Archetypes</h1>
        <p className="text-sm text-gray-400 mt-1 max-w-3xl">
          Every qualified D-I player is clustered into one of twelve D&amp;D-flavored
          classes based on their shot diet, rate stats, impact metrics, and minutes
          share. Clusters come from k-means in standardized feature space; each
          centroid is matched to the archetype it best resembles.
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
        {CLASS_DEFS.map((def) => (
          <ClassCard
            key={def.name}
            def={def}
            info={byName.get(def.name) ?? null}
          />
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
