# 2027 Season Preview — Two-Part Script (Top 10 Offenses / Top 10 Defenses)

Source: campom.org projections, base season 2026. Board: `campom.org/projected?season=2027`
Metric note: Proj O = adjusted offensive efficiency (pts/100, higher better). Proj D = adjusted defensive efficiency (pts/100 ALLOWED, lower better). "camV3" = a player's projected CamPom value; team-level sums are the model's raw roster inputs. `baseline_weight` = how much the projection leans on program history vs. this specific roster (low = roster-driven rebuild, high = program-anchored).

DATA FLAGS (do not say on camera):
- Michigan's coach shows as "Mike Boynton / new HC" — coachdict error, it's Dusty May. Just don't name the coach for Michigan.
- Floor/ceiling bands = midpoint for 2027 (not populated) → don't cite a range.
- Coach CAE grades below are live once `coach_ratings`/`coach_season_cae` are pushed to prod (`./scripts/sync_to_prod.sh --tables coach_ratings,coach_season_cae`). CAE = performance ABOVE the roster-based projection; it's descriptive/display-only (program-level bias, not a validated coaching-skill score) — a fun on-screen number, not the backbone of the take. Flag low-reliability grades (Scheyer 0.39, Diebler 0.24 have thin résumés).

COACH CAE REFERENCE (pull up `campom.org/coaches` on screen):
Kelvin Sampson (Houston) +6.14 | Mark Few (Gonzaga) +4.60 | Tommy Lloyd (Arizona) +4.03 | Grant McCasland (Texas Tech) +3.76 | Rick Pitino (St. John's) +3.52 | Rick Barnes (Tennessee) +3.38 | Tom Izzo (Michigan St.) +3.18 | Todd Golden (Florida) +3.04 | Matt Painter (Purdue) +2.66 | Dan Hurley (UConn) +2.48 | Jon Scheyer (Duke) +2.30* | Brad Underwood (Illinois) +2.23 | Nate Oats (Alabama) +1.89 | Pat Kelsey (Louisville) +0.38 | Jake Diebler (Ohio St.) +0.21* | John Calipari (Arkansas) −0.44   (* = low reliability)

---

## COLD OPEN / METHODOLOGY PRIMER (~90 sec, run once at the top of Part 1)

Talking points:
- Everything here comes off the projection model on campom.org — pull up `campom.org/projected?season=2027`. It takes each team's 2026 roster, subtracts who left, adds transfers (portal) and recruits (247 composite), projects every returning player's CamPom forward a year, and blends it against program baseline.
- The output is a projected AdjEM split into an offense number and a defense number — which is exactly why we can do two separate episodes.
- Vocabulary the audience needs: CamPom = our all-in-one player value; every player also gets a D&D "archetype" (pull up `campom.org/archetypes`). Quick key: **Druid** = do-everything forward, the model's favorite (highest average value). **Sorcerer** = scoring wing/guard. **Wizard** = high-usage lead guard / floor general. **Paladin** = rim-protecting big. **Rogue** = glue/connector. Everything below that = role/specialist tiers (Monk, Warlock, Barbarian, Bard, Ranger, Cleric, Fighter).
- Recurring lens for both episodes: `baseline_weight`. Some of these teams are almost entirely new rosters the model trusts anyway (Louisville, Arkansas, Tennessee at 0.20) vs. program-anchored continuity teams (UConn, Florida, Illinois, Michigan St. at 0.45).

---

# PART 1 — TOP 10 PROJECTED OFFENSES

Countdown 10 → 1.

### 10. UConn Huskies — Proj O 122.9  (also #5 DEFENSE; #6 overall, EM 33.6)
Show: `campom.org/teams/3dfeaa6f-de19-4029-bdaa-0f2bd8ea04c1?season=2027&view=projected`
- The only top-10 offense that's also a top-5 defense — tease that this team comes back in Part 2.
- Backcourt engine returns: **Silas Demary Jr.** (12.8, Wizard) and a projected breakout in **Braylon Mullins** (9.9, Sorcerer, sophomore), plus **Solo Ball** and **Jayden Ross**.
- Loses Alex Karaban and Tarris Reed (both ~13–16 camV3) — but Hurley reloads through the portal (6 arrivals) and a #26 recruit (Colben Landrew).
- `baseline_weight` 0.45 — the model is leaning on the back-to-back-title program identity, not just the roster.

### 9. Purdue Boilermakers — Proj O 123.2  (#14 overall, EM 29.3)
Show: `campom.org/teams/a544a5d0-4cd1-4756-b079-30a9a7905cd0?season=2027&view=projected`
- The "how is this still a top-10 offense?" team. Purdue loses its entire legendary core — **Braden Smith** (17.1, Wizard), **Trey Kaufman-Renn** (12.8), **Fletcher Loyer** (13.2), **Oscar Cluff** (14.1). That's ~58 camV3 of departures, one of the biggest brain-drains in the country.
- Model keeps them here on returners **CJ Cox** (10.5), **Gicarri Harris** (8.2), 7-footer **Daniel Jacobsen** (7.1, Paladin) + Princeton transfer **Caden Pierce** (5.9, Druid) and a strong #45/#62/#67 recruiting trio.
- Talking point: `baseline_weight` 0.38 — Painter's program floor is doing real work here. Great illustration of what the baseline lever is for.

### 8. Tennessee Volunteers — Proj O 123.4  (#22 overall, EM 25.5)
Show: `campom.org/teams/3710f3f1-b8c6-4f75-85b7-66deba7d916a?season=2027&view=projected`
- The biggest roster teardown in the top 10: **9 departures (~71 camV3)** — Ja'Kobi Gillespie, Nate Ament, JP Estrella all gone — and **7 transfers in**.
- New backcourt is portal-built: **Jalen Haralson** (9.6, Wizard), **Terrence Hill** (9.2), **Juke Harris** (8.2), plus Dai Dai Ames. Rick Barnes rebuilding on the fly.
- `baseline_weight` 0.20 — almost pure roster projection, the model is NOT giving Tennessee program credit here. Note the offense-only profile (D is only ~#50) — flag it won't appear in Part 2.

### 7. Arkansas Razorbacks — Proj O 125.1  (#11 overall, EM 30.7)
Show: `campom.org/teams/977af06b-e276-4d26-9f4d-4b3c41c67885?season=2027&view=projected`
- The Calipari freshman-machine episode. Arkansas returns almost nothing (2 returners, ~10 camV3) after losing Darius Acuff Jr. (19.1), Meleek Thomas, Malique Ewin, DJ Wagner, Karter Knox.
- But the recruiting board is elite: **#3 Jordan Smith Jr., #13 Abdou Toure, #16 JJ Andrews, plus Miikka Muurinen (#54)** — pull up `campom.org/projected?season=2027` and show the top_recruits row.
- `baseline_weight` 0.20 — model is trusting freshmen + Billy Richmond (9.4) and transfer Jeremiah Wilkinson (8.3). Talking point: how much do you trust a projection built almost entirely on recruits?
- CAE hook: **Calipari is the ONLY coach in either top-10 with a negative CAE (−0.44)** — historically his teams score slightly BELOW what their roster talent projects. Perfect tension against an elite recruiting class: does the talent finally overwhelm the pattern, or does the model's skepticism hold?

### 6. Florida Gators — Proj O 125.3  (also #8 DEFENSE; #4 overall, EM 34.1)
Show: `campom.org/teams/65cd560f-8b8d-4b8f-8a2c-2738484621b8?season=2027&view=projected`
- The continuity story. Florida returns **7 rotation pieces (~66 camV3, the most in this group)** off the title run.
- Loaded frontcourt: **Alex Condon** (17.5, Druid — a top-5 returning player in the country), **Thomas Haugh** (15.5, Sorcerer), **Rueben Chinyelu** (11.7, Druid). Backcourt **Boogie Fland** (10.1, Rogue) + transfer Denzel Aberdeen back.
- Top-8 on BOTH ends — flag they return in Part 2. `baseline_weight` 0.45. Point out how rare "keep 7 guys" is in the portal era.

### 5. Texas Tech Red Raiders — Proj O 125.4  (#12 overall, EM 30.6)
Show: `campom.org/teams/edb2bfd0-017f-4df2-b9c6-d8dcacb7e929?season=2027&view=projected`
- Single-star episode: **JT Toppin** (17.4, Druid) is the headliner — essentially the #1 or #2 returning player in the sport. Pull up his player page and progression to show the year-over-year projection.
- Everything else is new: loses Christian Anderson (16.5), Donovan Atwell, LeJuan Watts; rebuilds with transfers Dra Gibbs-Lawhorn, Damarion Dennis, Cruz Davis + #29 recruit DaKari Spear.
- `baseline_weight` 0.26 (low) — this is a roster-driven number riding on Toppin. Great "one superstar can carry an offense projection" talking point.

### 4. Ohio State Buckeyes — Proj O 125.5  (#20 overall, EM 26.7)
Show: `campom.org/teams/6265a954-0017-44e5-bec7-cdb2fcc8f527?season=2027&view=projected`
- The most lopsided team in Part 1: #4 offense but **#64 defense** — huge gap, best "elite O, no D" case study of the episode.
- **John Mobley Jr.** (12.0, Sorcerer) returns as the shot-maker; transfer **Justin Pippen** (8.2, Wizard — yes, Scottie's son) runs point; #8 recruit **Anthony Thompson** headlines the class.
- Loses Bruce Thornton (19.9, Wizard). Talking point: the model likes the shot-making, flat-out doesn't trust the defense.

### 3. Illinois Fighting Illini — Proj O 127.2  (#3 overall, EM 34.4)
Show: `campom.org/teams/bd78de3d-6129-4bfa-8203-6ad7e79b5e54?season=2027&view=projected`
- Size + skill. The **Ivisic twins** — **Tomislav** (12.6, Sorcerer) and **Zvonimir** (12.3, Paladin) — plus a big sophomore leap for **David Mirkovic** (14.4, Druid, the team's top projection) and wing **Andrej Stojakovic** (11.0, Druid).
- Underwood keeps 5 (~50 camV3 returning) and adds #17 recruit Quentin Coleman. Loses Kylan Boswell and freshman Keaton Wagler (19.6!).
- `baseline_weight` 0.45 — program + roster both strong. Note it's #3 overall, a genuine title-contender profile.

### 2. Duke Blue Devils — Proj O 127.3  (also #2 DEFENSE; #1 overall, EM 39.2)
Show: `campom.org/teams/d414d5fc-8653-43b8-ac66-ec8f63c74c82?season=2027&view=projected`
- THE headline team of the whole series: **#1 overall, and top-2 on BOTH ends — the only team in the sport that is.** Reload, not rebuild.
- And they lose the single most valuable departing player in the country — **Cameron Boozer** (30.5 camV3, Druid, and the #1 exemplar of the Druid archetype on `campom.org/archetypes`) — plus Isaiah Evans (13.2) and Maliq Brown.
- How is the O still #2? Returners **Patrick Ngongba** (12.2, Paladin), **Cayden Boozer** (10.3, Rogue), **Dame Sarr** (9.1), **Caleb Foster** (8.3); Wisconsin transfer **John Blackwell** (11.6, Sorcerer); and a monster class — **#4 Cameron Williams, #12 Deron Rippey Jr., #22 Bryson Howard, #54 Boumtje**.
- Talking point: this is the projection engine's thesis in one team — Scheyer replaces a 30-point player and the number barely moves.

### 1. Alabama Crimson Tide — Proj O 127.5  (#9 overall, EM 31.9)
Show: `campom.org/teams/b9e807be-99ad-41c2-94bc-6049272e3254?season=2027&view=projected`
- **The #1 projected offense in the country** — and the definitive Nate Oats "system over roster" episode.
- They lose a monster in **Labaron Philon** (19.5, Wizard, 22 ppg) plus Aiden Sherrell and Latrell Wrightsell — yet the O stays #1 on returners **Aden Holloway** (13.2, Sorcerer) and **Amari Allen** (11.8, Sorcerer) + transfers **Drew Fielder** (8.4, Druid) and **Brandon Garrison** (5.5, Paladin).
- The catch: **defense is only #25** — pace-and-space cuts both ways. Contrast directly with Duke (#1 O AND #2 D). `baseline_weight` 0.36.
- Close Part 1: three of the top-5 offenses (Bama, Duke, Illinois) are separated by 0.3 points per 100 — basically a tie at the top.

---

# PART 2 — TOP 10 PROJECTED DEFENSES

Countdown 10 → 1. Reminder for the audience: lower number = better defense.

### 10. Saint John's Red Storm — Proj D 91.9  (#43 offense, #25 overall, EM 24.9)
Show: `campom.org/teams/d88ac8c9-9fc8-4e76-9426-cf44fe70db47?season=2027&view=projected`
- The mirror image of the Ohio State segment: **#10 defense but only #43 offense** — the one pure defense-first team in either episode. Pitino's pressure identity in a number.
- Loses a huge frontcourt — **Zuby Ejiofor** (20.8, Druid, a top Druid exemplar), Bryce Hopkins, Dillon Mitchell. Rebuilds around **Ian Jackson** (6.7, Sorcerer), **Rubén Prey** (8.6, Paladin) and transfer **Tounde Yessoufou** (10.6, Sorcerer).
- `baseline_weight` 0.20 — pure roster read, no program credit, and the model still buys the D. Good "defense travels through system" debate.

### 9. Michigan State Spartans — Proj D 91.6  (#13 overall, EM 29.9)
Show: `campom.org/teams/5a3e7af6-53d5-4398-8c0d-97e0d9ac977e?season=2027&view=projected`
- The Izzo continuity/defense episode: returns **7 players (~48 camV3)**, most in this group.
- **Jeremy Fears Jr.** (14.6, Wizard) is the defensive floor general; athletic **Coen Carr** (10.9, Druid), plus Jordan Scott, Cameron Ward, Kur Teng, Trey Fort.
- `baseline_weight` 0.45 — program + continuity. Classic MSU: veteran, connected, hard to score on.

### 8. Florida Gators — Proj D 91.2  (#6 offense, #4 overall, EM 34.1)
- Callback to Part 1 — **top-8 on both ends.** The frontcourt that drives the offense (Condon, Chinyelu — both Druids) is also the rim protection. Continuity = defense. Full detail already covered in Part 1 #6.

### 7. Louisville Cardinals — Proj D 90.8  (#11 offense, #7 overall, EM 32.1)
Show: `campom.org/teams/eaa03f66-f667-48c2-9f30-ccce38d89e56?season=2027&view=projected`
- The "brand-new team every year" episode. Louisville returns **just 2 players (~7 camV3, lowest in either list)** and imports **6 transfers (~41 camV3).**
- Elite portal defense haul: **Flory Bidunga** (12.6, Paladin — rim protector from Kansas), **Alvaro Folgueiras** (11.9, Rogue), **Jackson Shelstad** (7.6, Wizard, from Oregon), De'Shayne Montgomery, plus **Karter Knox** (from Arkansas — a portal player the model tracks across teams).
- `baseline_weight` 0.20. Pat Kelsey teleports in a top-10 defense. Talking point: the projection follows players through the portal — Knox shows up as an Arkansas departure AND a Louisville arrival.

### 6. Gonzaga Bulldogs — Proj D 90.7  (#15 offense, #10 overall, EM 31.2)
Show: `campom.org/teams/9eea7343-e29b-42cd-8ec7-ccda00c95536?season=2027&view=projected`
- The surprise: Gonzaga is usually an offense-first program, and here the model has them **top-6 defensively.**
- Anchored by **Braden Huff** (13.1, Druid), sophomore **Davis Fogle** (11.0, Druid) and guard **Mario Saint-Supery** (9.7, Rogue). Loses Graham Ike (13.6, Druid).
- Note the transfer-in: **Isiah Harwell** (6.0) — another player who's a departure elsewhere (Houston) and an arrival here. `baseline_weight` 0.38.

### 5. UConn Huskies — Proj D 89.3  (#10 offense, #6 overall, EM 33.6)
- Part 1 callback — **top-10 O and top-5 D.** Demary/Mullins/Ball score; length and Hurley's scheme defend. One of only three teams (with Duke and Florida) that lands on both boards. Detail covered in Part 1 #10.

### 4. Houston Cougars — Proj D 89.0  (#20 offense, #8 overall, EM 32.0)
Show: `campom.org/teams/d783c621-0bb0-477b-8bad-8785e9451e21?season=2027&view=projected`
- The Kelvin Sampson defensive-identity episode — #4 D, only #20 O (Houston lives on the other end).
- Anchor: **Joseph Tugler** (14.0, Paladin) — the projected defensive centerpiece, elite shot-blocker. Pull up his player page/progression.
- Loses a lot of offense (Milos Uzan, Emanuel Sharp, Kingston Flemings) — but Sampson's D projects to survive it. `baseline_weight` 0.30. Great "the system is the star" talking point.
- CAE payoff: **Sampson's +6.14 is the single highest coach grade in your entire field** — the model literally quantifies that his teams outperform their roster year after year. This is the segment to put `campom.org/coaches` on screen.

### 3. Michigan Wolverines — Proj D 88.4  (#5 overall, EM 33.9)
Show: `campom.org/teams/d9469ea9-44c5-4803-8d5a-39cf4028ea3a?season=2027&view=projected`
- (Coach data flag — don't name the coach.) **#5 overall, #3 defense.**
- Guard defense + a projected sophomore leap: **Trey McKenney** (11.7, Sorcerer), **Elliot Cadeau** (11.6, Wizard), **LJ Cason** (8.3, Rogue). Frontcourt rebuilt via portal — **JP Estrella** (9.8, Druid, from Tennessee — track him across the portal again) and 7-footer **Moustapha Thiam** (8.1, Druid).
- Loses a ton (Yaxel Lendeborg 22.3, Aday Mara, Morez Johnson) yet the D projection holds. `baseline_weight` 0.28 — roster-driven.

### 2. Duke Blue Devils — Proj D 88.1  (#2 offense, #1 overall, EM 39.2)
- The big callback: **the only team top-2 in BOTH offense and defense.** Ngongba anchors the rim (Paladin), Cayden Boozer (Rogue) and Sarr defend on the perimeter, and the elite class adds length. Full detail in Part 1 #2 — here just hammer the both-ends dominance and the #1-overall projection.

### 1. Arizona Wildcats — Proj D 87.3  (#13 offense, #2 overall, EM 35.3)
Show: `campom.org/teams/179abcd1-98d5-4dc8-8532-3f8ffee320c5?season=2027&view=projected`
- **The #1 projected defense in the country — and #2 overall behind only Duke.** Big talking point: Tommy Lloyd's reputation is offense, but the 2027 projection flips the script.
- Rim protection returns: **Motiejus Krivas** (11.9, Paladin) back from injury + versatile **Ivan Kharchenkov** (12.5, Rogue).
- Massive roster turnover — loses Brayden Burries (15.8), Jaden Bradley, Koa Peat, Tobe Awaka (~63 camV3 out) — reloaded by the **#2 overall recruit Caleb Holt** and a strong class. `baseline_weight` 0.31.
- Close the series: Arizona #1 D, Alabama #1 O, and Duke the only team elite on both — the three theses of the projection model.

---

## CROSS-CUTTING TALKING POINTS / FEATURE PLUGS (sprinkle throughout or use as an outro)

- **The portal is tracked player-by-player.** Same names appear as a departure on one team and an arrival on another — Karter Knox (Arkansas → Louisville), JP Estrella (Tennessee → Michigan), Isiah Harwell (Houston → Gonzaga), Nikolas Khamenia (Duke → UConn), Caden Pierce (Princeton → Purdue). The projection moves their value with them.
- **`baseline_weight` as a storyline.** Roster-driven rebuilds at 0.20 (Louisville, Arkansas, Tennessee, Saint John's) vs. program-anchored 0.45 (UConn, Florida, Illinois, Michigan St.). It's the single best "how much is this the coach vs. the roster" number on the site.
- **Druid = the model's MVP class.** Highest average CamPom of the 12 archetypes. Notice how many top teams are anchored by one: Florida (Condon), Texas Tech (Toppin), Illinois (Mirkovic), Gonzaga (Huff) — and the guy Duke is replacing (Cameron Boozer). Pull up `campom.org/archetypes` and show the Druid exemplars.
- **The three-team overlap:** Duke, Florida, UConn are the only teams that make BOTH the top-10 offense and top-10 defense lists. That's your "complete team" tier.
- **Offense-only vs. defense-only bookends:** Ohio State (#4 O / #64 D) and Saint John's (#10 D / #43 O) are perfect mirror images — good back-and-forth segment between the two episodes.
- Feature plugs to work in: `campom.org/predict` (simulate any 2027 matchup), `campom.org/players/compare` (put two of these stars head-to-head), any player's `/progression` page (year-over-year CamPom projection), `campom.org/lineups` (best 5-man combos), and `campom.org/portle` (the daily guess-the-player game) as a fun outro CTA.
