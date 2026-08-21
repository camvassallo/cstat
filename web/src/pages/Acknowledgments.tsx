import { useEffect } from 'react';
import { usePageTitle } from '../components/usePageTitle';
import { setPageSeasons } from '../components/season';

/**
 * Data acknowledgments — the single home for provider credit.
 *
 * Every other surface on the site used to carry its own outbound "source"
 * link (247Sports on the portal and recruit boards, Torvik footnotes on the
 * shot charts, and so on). Those credits are real obligations, but scattering
 * them across the product turned each one into an exit ramp. They live here
 * instead: fully attributed, linked, and easy to find from the footer.
 */

type Source = {
  name: string;
  href: string;
  role: string;
  detail: string;
};

const SOURCES: Source[] = [
  {
    name: 'NatStat',
    href: 'https://natst.at/',
    role: 'Games, box scores, and play-by-play',
    detail:
      'The backbone of the site. Schedules, results, team and player box scores, and the substitution play-by-play that lineup and on/off numbers are reconstructed from.',
  },
  {
    name: 'Bart Torvik',
    href: 'https://barttorvik.com/',
    role: 'Advanced player metrics and shot zones',
    detail:
      "Per-game player efficiency, shot-zone splits, and the box-plus-minus foundation the CAM valuation is built on. Also the source of the head-coach mapping behind the coach ratings.",
  },
  {
    name: '247Sports',
    href: 'https://247sports.com/',
    role: 'Recruiting and transfer portal',
    detail:
      'Composite high-school recruit rankings and transfer-portal entries, commitments, and destinations — the roster inputs for every preseason projection.',
  },
  {
    name: 'Tankathon',
    href: 'https://www.tankathon.com/',
    role: 'NBA draft order and mock draft',
    detail:
      'Draft order and the running mock board, which the draft page joins to each prospect\u2019s college season.',
  },
];

function SourceCard({ s }: { s: Source }) {
  return (
    <li className="bg-gray-900 border border-gray-800 rounded-lg p-4">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <a
          href={s.href}
          target="_blank"
          rel="noopener noreferrer"
          className="text-base font-semibold text-[var(--brand-blue-bright)] hover:underline"
        >
          {s.name}
        </a>
        <span className="text-xs uppercase tracking-wide text-gray-500">{s.role}</span>
      </div>
      <p className="text-sm text-gray-400 mt-2 leading-relaxed">{s.detail}</p>
    </li>
  );
}

export default function Acknowledgments() {
  usePageTitle('Data & Acknowledgments');
  // Season-agnostic page — hide the navbar year picker (empty list is the
  // agreed "hide" signal; see Layout's SeasonSelector), and release the
  // override on unmount.
  useEffect(() => {
    setPageSeasons([]);
    return () => setPageSeasons(null);
  }, []);

  return (
    <div className="max-w-3xl space-y-8">
      <header>
        <h1 className="text-3xl font-bold">Data &amp; Acknowledgments</h1>
        <p className="text-sm text-gray-400 mt-2 leading-relaxed">
          Camalytics is built on top of other people's work. These are the
          providers whose data makes the site possible.
        </p>
      </header>

      <section>
        <h2 className="text-xs font-semibold uppercase tracking-wide text-gray-500 mb-3">
          Data providers
        </h2>
        <ul className="space-y-3">
          {SOURCES.map((s) => (
            <SourceCard key={s.name} s={s} />
          ))}
        </ul>
      </section>

      <section>
        <h2 className="text-xs font-semibold uppercase tracking-wide text-gray-500 mb-3">
          What&apos;s ours
        </h2>
        <p className="text-sm text-gray-400 leading-relaxed">
          Everything derived on top of those feeds is computed here: adjusted
          efficiency and team ratings, the <strong className="text-gray-200">CAM</strong>{' '}
          player valuation and its CAMO / CAMD halves, RAPM and on/off,
          lineup reconstruction, the twelve player archetypes, game
          predictions, preseason team projections, and coach ratings.
        </p>
        <p className="text-sm text-gray-400 leading-relaxed mt-3">
          The full methodology — model architecture, features, backtest results,
          and the caveats that come with each — is documented in the{' '}
          <a
            href="https://github.com/camvassallo/cstat"
            target="_blank"
            rel="noopener noreferrer"
            className="text-[var(--brand-blue-bright)] hover:underline"
          >
            open-source repository
          </a>
          .
        </p>
      </section>

      <section>
        <h2 className="text-xs font-semibold uppercase tracking-wide text-gray-500 mb-3">
          Use and accuracy
        </h2>
        <p className="text-sm text-gray-400 leading-relaxed">
          Ratings, projections, and predictions are statistical estimates, not
          certainties. Preseason projections in particular are compressed toward
          average and should be read as directional. Camalytics is an
          independent project and is not affiliated with the NCAA, any
          conference, or any institution.
        </p>
      </section>
    </div>
  );
}
