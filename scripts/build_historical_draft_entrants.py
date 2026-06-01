#!/usr/bin/env python3
"""Build data/draft/{year}_early_entrants.json for historical NBA drafts.

Source: Tankathon past-drafts pages (https://www.tankathon.com/past-drafts/{year}),
captured 2026-06-01. Each drafted player whose pre-draft team was a US college
did NOT return to that roster the next season, so they belong in that base
season's "gone" departure list. The roster projection matches entrants to the
base-season roster by normalized name + team, so:
  - International / G-League picks are dropped (no cstat roster to leave).
  - Names use the player's college form; the matcher folds punctuation and
    Jr/Sr/II/III suffixes, so "V.J. Edgecombe" matches "VJ Edgecombe", etc.
  - Players who were drafted but never logged a cstat game (e.g. Shaedon
    Sharpe) simply fail to match and are harmlessly skipped.

`{year}_early_entrants.json` feeds the projection for TARGET season `year + 1`
(base season `year`). 2026 (the live-forecast file) is maintained separately.

Re-run after capturing a new year:  python3 scripts/build_historical_draft_entrants.py
"""
import json
import re
from pathlib import Path

OUT_DIR = Path(__file__).resolve().parent.parent / "data" / "draft"

# Raw "pick | name | team" (or "pick. name | team") blocks, verbatim from the
# Tankathon fetch. The parser strips the leading enumerator and the team field;
# any team containing "NON-COLLEGE" (or a parenthetical NON-COLLEGE tag) is
# dropped.
RAW = {
    2025: """
1 | Cooper Flagg | Duke
2 | Dylan Harper | Rutgers
3 | V.J. Edgecombe | Baylor
4 | Kon Knueppel | Duke
5 | Ace Bailey | Rutgers
6 | Tre Johnson | Texas
7 | Jeremiah Fears | Oklahoma
8 | Egor Demin | BYU
9 | Collin Murray-Boyles | South Carolina
10 | Khaman Maluach | Duke
11 | Cedric Coward | Washington State
12 | Noa Essengue | NON-COLLEGE
13 | Derik Queen | Maryland
14 | Carter Bryant | Arizona
15 | Thomas Sorber | Georgetown
16 | Hansen Yang | NON-COLLEGE
17 | Joan Beringer | NON-COLLEGE
18 | Walter Clayton Jr. | Florida
19 | Nolan Traore | NON-COLLEGE
20 | Kasparas Jakucionis | Illinois
21 | Will Riley | Illinois
22 | Drake Powell | North Carolina
23 | Asa Newell | Georgia
24 | Nique Clifford | Colorado State
25 | Jase Richardson | Michigan State
26 | Ben Saraf | NON-COLLEGE
27 | Danny Wolf | Michigan
28 | Hugo Gonzalez | NON-COLLEGE
29 | Liam McNeeley | UConn
30 | Yanic Konan Niederhauser | Penn State
31 | Rasheer Fleming | Saint Joseph's
32 | Noah Penda | NON-COLLEGE
33 | Sion James | Duke
34 | Ryan Kalkbrenner | Creighton
35 | Johni Broome | Auburn
36 | Adou Thiero | Arkansas
37 | Chaz Lanier | Tennessee
38 | Kam Jones | Marquette
39 | Alijah Martin | Florida
40 | Micah Peavy | Georgetown
41 | Koby Brea | Kentucky
42 | Maxime Raynaud | Stanford
43 | Jamir Watkins | Florida State
44 | Brooks Barnhizer | Northwestern
45 | Rocco Zikarsky | NON-COLLEGE
46 | Amari Williams | Kentucky
47 | Bogoljub Markovic | NON-COLLEGE
48 | Javon Small | West Virginia
49 | Tyrese Proctor | Duke
50 | Kobe Sanders | Nevada
51 | Mohamed Diawara | NON-COLLEGE
52 | Alex Toohey | NON-COLLEGE
53 | John Tonje | Wisconsin
54 | Taelon Peter | Liberty
55 | Lachlan Olbrich | NON-COLLEGE
56 | Will Richard | Florida
57 | Max Shulga | VCU
58 | Saliou Niang | NON-COLLEGE
59 | Jahmai Mashack | Tennessee
""",
    2024: """
1. Zaccharie Risacher | NON-COLLEGE
2. Alexandre Sarr | NON-COLLEGE
3. Reed Sheppard | Kentucky
4. Stephon Castle | UConn
5. Ron Holland | NON-COLLEGE
6. Tidjane Salaun | NON-COLLEGE
7. Donovan Clingan | UConn
8. Rob Dillingham | Kentucky
9. Zach Edey | Purdue
10. Cody Williams | Colorado
11. Matas Buzelis | NON-COLLEGE
12. Nikola Topic | NON-COLLEGE
13. Devin Carter | Providence
14. Carlton Carrington | Pittsburgh
15. Kel'el Ware | Indiana
16. Jared McCain | Duke
17. Dalton Knecht | Tennessee
18. Tristan da Silva | Colorado
19. Ja'Kobe Walter | Baylor
20. Jaylon Tyson | California
21. Yves Missi | Baylor
22. DaRon Holmes II | Dayton
23. AJ Johnson | NON-COLLEGE
24. Kyshawn George | Miami
25. Pacome Dadiet | NON-COLLEGE
26. Dillon Jones | Weber State
27. Terrence Shannon Jr. | Illinois
28. Ryan Dunn | Virginia
29. Isaiah Collier | USC
30. Baylor Scheierman | Creighton
31. Jonathan Mogbo | San Francisco
32. Kyle Filipowski | Duke
33. Tyler Smith | NON-COLLEGE
34. Tyler Kolek | Marquette
35. Johnny Furphy | Kansas
36. Juan Nunez | NON-COLLEGE
37. Bobi Klintman | NON-COLLEGE
38. Ajay Mitchell | UC Santa Barbara
39. Jaylen Wells | Washington State
40. Oso Ighodaro | Marquette
41. Adem Bona | UCLA
42. KJ Simpson | Colorado
43. Nikola Djurisic | NON-COLLEGE
44. Pelle Larsson | Arizona
45. Jamal Shead | Houston
46. Cam Christie | Minnesota
47. Antonio Reeves | Kentucky
48. Harrison Ingram | North Carolina
49. Tristen Newton | UConn
50. Enrique Freeman | Akron
51. Melvin Ajinca | NON-COLLEGE
52. Quinten Post | Boston College
53. Cam Spencer | UConn
54. Anton Watson | Gonzaga
55. Bronny James | USC
56. Kevin McCullar | Kansas
57. Ulrich Chomche | NON-COLLEGE
58. Ariel Hukporti | NON-COLLEGE
""",
    2023: """
1. Victor Wembanyama | NON-COLLEGE
2. Brandon Miller | Alabama
3. Scoot Henderson | NON-COLLEGE
4. Amen Thompson | NON-COLLEGE
5. Ausar Thompson | NON-COLLEGE
6. Anthony Black | Arkansas
7. Bilal Coulibaly | NON-COLLEGE
8. Jarace Walker | Houston
9. Taylor Hendricks | UCF
10. Cason Wallace | Kentucky
11. Jett Howard | Michigan
12. Dereck Lively II | Duke
13. Gradey Dick | Kansas
14. Jordan Hawkins | UConn
15. Kobe Bufkin | Michigan
16. Keyonte George | Baylor
17. Jalen Hood-Schifino | Indiana
18. Jaime Jaquez Jr. | UCLA
19. Brandin Podziemski | Santa Clara
20. Cam Whitmore | Villanova
21. Noah Clowney | Alabama
22. Dariq Whitehead | Duke
23. Kris Murray | Iowa
24. Olivier-Maxence Prosper | Marquette
25. Marcus Sasser | Houston
26. Ben Sheppard | Belmont
27. Nick Smith Jr. | Arkansas
28. Brice Sensabaugh | Ohio State
29. Julian Strawther | Gonzaga
30. Kobe Brown | Missouri
31. James Nnaji | NON-COLLEGE
32. Jalen Pickett | Penn State
33. Leonard Miller | NON-COLLEGE
34. Colby Jones | Xavier
35. Julian Phillips | Tennessee
36. Andre Jackson Jr. | UConn
37. Hunter Tyson | Clemson
38. Jordan Walsh | Arkansas
39. Mouhamed Gueye | Washington State
40. Maxwell Lewis | Pepperdine
41. Amari Bailey | UCLA
42. Tristan Vukcevic | NON-COLLEGE
43. Rayan Rupert | NON-COLLEGE
44. Sidy Cissoko | NON-COLLEGE
45. GG Jackson | South Carolina
46. Seth Lundy | Penn State
47. Mojave King | NON-COLLEGE
48. Jordan Miller | Miami
49. Emoni Bates | Eastern Michigan
50. Keyontae Johnson | Kansas State
51. Jalen Wilson | Kansas
52. Toumani Camara | Dayton
53. Jaylen Clark | UCLA
54. Jalen Slawson | Furman
55. Isaiah Wong | Miami
56. Tarik Biberovic | NON-COLLEGE
57. Trayce Jackson-Davis | Indiana
58. Chris Livingston | Kentucky
""",
    2022: """
1. Paolo Banchero | Duke
2. Chet Holmgren | Gonzaga
3. Jabari Smith | Auburn
4. Keegan Murray | Iowa
5. Jaden Ivey | Purdue
6. Bennedict Mathurin | Arizona
7. Shaedon Sharpe | Kentucky
8. Dyson Daniels | NON-COLLEGE
9. Jeremy Sochan | Baylor
10. Johnny Davis | Wisconsin
11. Ousmane Dieng | NON-COLLEGE
12. Jalen Williams | Santa Clara
13. Jalen Duren | Memphis
14. Ochai Agbaji | Kansas
15. Mark Williams | Duke
16. AJ Griffin | Duke
17. Tari Eason | LSU
18. Dalen Terry | Arizona
19. Jake LaRavia | Wake Forest
20. Malaki Branham | Ohio State
21. Christian Braun | Kansas
22. Walker Kessler | Auburn
23. David Roddy | Colorado State
24. MarJon Beauchamp | NON-COLLEGE
25. Blake Wesley | Notre Dame
26. Wendell Moore | Duke
27. Nikola Jovic | NON-COLLEGE
28. Patrick Baldwin Jr. | Milwaukee
29. TyTy Washington | Kentucky
30. Peyton Watson | UCLA
31. Andrew Nembhard | Gonzaga
32. Caleb Houstan | Michigan
33. Christian Koloko | Arizona
34. Jaylin Williams | Arkansas
35. Max Christie | Michigan State
36. Gabriele Procida | NON-COLLEGE
37. Jaden Hardy | NON-COLLEGE
38. Kennedy Chandler | Tennessee
39. Khalifa Diop | NON-COLLEGE
40. Bryce McGowens | Nebraska
41. E.J. Liddell | Ohio State
42. Trevor Keels | Duke
43. Moussa Diabate | Michigan
44. Ryan Rollins | Toledo
45. Josh Minott | Memphis
46. Ismael Kamagate | NON-COLLEGE
47. Vince Williams Jr. | VCU
48. Kendall Brown | Baylor
49. Isaiah Mobley | USC
50. Matteo Spagnolo | NON-COLLEGE
51. Tyrese Martin | UConn
52. Karlo Matkovic | NON-COLLEGE
53. JD Davison | Alabama
54. Yannick Nzosa | NON-COLLEGE
55. Gui Santos | NON-COLLEGE
56. Luke Travers | NON-COLLEGE
57. Jabari Walker | Colorado
58. Hugo Besson | NON-COLLEGE
""",
    2021: """
1. Cade Cunningham | Oklahoma State
2. Jalen Green | NON-COLLEGE
3. Evan Mobley | USC
4. Scottie Barnes | Florida State
5. Jalen Suggs | Gonzaga
6. Josh Giddey | NON-COLLEGE
7. Jonathan Kuminga | NON-COLLEGE
8. Franz Wagner | Michigan
9. Davion Mitchell | Baylor
10. Ziaire Williams | Stanford
11. James Bouknight | UConn
12. Joshua Primo | Alabama
13. Chris Duarte | Oregon
14. Moses Moody | Arkansas
15. Corey Kispert | Gonzaga
16. Alperen Sengun | NON-COLLEGE
17. Trey Murphy III | Virginia
18. Tre Mann | Florida
19. Kai Jones | Texas
20. Jalen Johnson | Duke
21. Keon Johnson | Tennessee
22. Isaiah Jackson | Kentucky
23. Usman Garuba | NON-COLLEGE
24. Josh Christopher | Arizona State
25. Quentin Grimes | Houston
26. Nah'Shon Hyland | VCU
27. Cameron Thomas | LSU
28. Jaden Springer | Tennessee
29. Day'Ron Sharpe | North Carolina
30. Santi Aldama | Loyola Maryland
31. Isaiah Todd | NON-COLLEGE
32. Jeremiah Robinson-Earl | Villanova
33. Jason Preston | Ohio
34. Rokas Jokubaitis | NON-COLLEGE
35. Herbert Jones | Alabama
36. Miles McBride | West Virginia
37. JT Thor | Auburn
38. Ayo Dosunmu | Illinois
39. Neemias Queta | Utah State
40. Jared Butler | Baylor
41. Joe Wieskamp | Iowa
42. Isaiah Livers | Michigan
43. Greg Brown | Texas
44. Kessler Edwards | Pepperdine
45. Juhann Begarin | NON-COLLEGE
46. Dalano Banton | Nebraska
47. David Johnson | Louisville
48. Sharife Cooper | Auburn
49. Marcus Zegarowski | Creighton
50. Filip Petrusev | NON-COLLEGE
51. BJ Boston | Kentucky
52. Luka Garza | Iowa
53. Charles Bassey | Western Kentucky
54. Sandro Mamukelashvili | Seton Hall
55. Aaron Wiggins | Maryland
56. Scottie Lewis | Florida
57. Balsa Koprivica | Florida State
58. Jericho Sims | Texas
59. RaiQuan Gray | Florida State
60. Georgios Kalaitzakis | NON-COLLEGE
""",
    2020: """
1. Anthony Edwards | Georgia
2. James Wiseman | Memphis
3. LaMelo Ball | NON-COLLEGE
4. Patrick Williams | Florida State
5. Isaac Okoro | Auburn
6. Onyeka Okongwu | USC
7. Killian Hayes | NON-COLLEGE
8. Obi Toppin | Dayton
9. Deni Avdija | NON-COLLEGE
10. Jalen Smith | Maryland
11. Devin Vassell | Florida State
12. Tyrese Haliburton | Iowa State
13. Kira Lewis Jr. | Alabama
14. Aaron Nesmith | Vanderbilt
15. Cole Anthony | North Carolina
16. Isaiah Stewart | Washington
17. Aleksej Pokusevski | NON-COLLEGE
18. Josh Green | Arizona
19. Saddiq Bey | Villanova
20. Precious Achiuwa | Memphis
21. Tyrese Maxey | Kentucky
22. Zeke Nnaji | Arizona
23. Leandro Bolmaro | NON-COLLEGE
24. RJ Hampton | NON-COLLEGE
25. Immanuel Quickley | Kentucky
26. Payton Pritchard | Oregon
27. Udoka Azubuike | Kansas
28. Jaden McDaniels | Washington
29. Malachi Flynn | San Diego State
30. Desmond Bane | TCU
31. Tyrell Terry | Stanford
32. Vernon Carey Jr. | Duke
33. Daniel Oturu | Minnesota
34. Theo Maledon | NON-COLLEGE
35. Xavier Tillman | Michigan State
36. Tyler Bey | Colorado
37. Vit Krejci | NON-COLLEGE
38. Saben Lee | Vanderbilt
39. Elijah Hughes | Syracuse
40. Robert Woodard II | Mississippi State
41. Tre Jones | Duke
42. Nick Richards | Kentucky
43. Jahmi'us Ramsey | Texas Tech
44. Marko Simonovic | NON-COLLEGE
45. Jordan Nwora | Louisville
46. CJ Elleby | Washington State
47. Yam Madar | NON-COLLEGE
48. Nico Mannion | Arizona
49. Isaiah Joe | Arkansas
50. Skylar Mays | LSU
51. Justinian Jessup | Boise State
52. Kenyon Martin Jr. | NON-COLLEGE
53. Cassius Winston | Michigan State
54. Cassius Stanley | Duke
55. Jay Scrubb | NON-COLLEGE
56. Grant Riller | Charleston
57. Reggie Perry | Mississippi State
58. Paul Reed | DePaul
59. Jalen Harris | Nevada
60. Sam Merrill | Utah State
""",
    2019: """
1. Zion Williamson | Duke
2. Ja Morant | Murray State
3. R.J. Barrett | Duke
4. De'Andre Hunter | Virginia
5. Darius Garland | Vanderbilt
6. Jarrett Culver | Texas Tech
7. Coby White | North Carolina
8. Jaxson Hayes | Texas
9. Rui Hachimura | Gonzaga
10. Cam Reddish | Duke
11. Cameron Johnson | North Carolina
12. P.J. Washington | Kentucky
13. Tyler Herro | Kentucky
14. Romeo Langford | Indiana
15. Sekou Doumbouya | NON-COLLEGE
16. Chuma Okeke | Auburn
17. Nickeil Alexander-Walker | Virginia Tech
18. Goga Bitadze | NON-COLLEGE
19. Luka Samanic | NON-COLLEGE
20. Matisse Thybulle | Washington
21. Brandon Clarke | Gonzaga
22. Grant Williams | Tennessee
23. Darius Bazley | NON-COLLEGE
24. Ty Jerome | Virginia
25. Nassir Little | North Carolina
26. Dylan Windler | Belmont
27. Mfiondu Kabengele | Florida State
28. Jordan Poole | Michigan
29. Keldon Johnson | Kentucky
30. Kevin Porter Jr. | USC
31. Nicolas Claxton | Georgia
32. KZ Okpala | Stanford
33. Carsen Edwards | Purdue
34. Bruno Fernando | Maryland
35. Didi Louzada | NON-COLLEGE
36. Cody Martin | Nevada
37. Deividas Sirvydis | NON-COLLEGE
38. Daniel Gafford | Arkansas
39. Alen Smailagic | NON-COLLEGE
40. Justin James | Wyoming
41. Eric Paschall | Villanova
42. Admiral Schofield | Tennessee
43. Jaylen Nowell | Washington
44. Bol Bol | Oregon
45. Isaiah Roby | Nebraska
46. Talen Horton-Tucker | Iowa State
47. Iggy Brazdeikis | Michigan
48. Terance Mann | Florida State
49. Quinndary Weatherspoon | Mississippi State
50. Jarrell Brantley | Charleston
51. Tremont Waters | LSU
52. Jalen McDaniels | San Diego State
53. Justin Wright-Foreman | Hofstra
54. Marial Shayok | Iowa State
55. Kyle Guy | Virginia
56. Jaylen Hands | UCLA
57. Jordan Bone | Tennessee
58. Miye Oni | Yale
59. Dewan Hernandez | Miami
60. Vanja Marinkovic | NON-COLLEGE
""",
    2018: """
1. DeAndre Ayton | Arizona
2. Marvin Bagley | Duke
3. Luka Doncic | NON-COLLEGE
4. Jaren Jackson Jr. | Michigan State
5. Trae Young | Oklahoma
6. Mohamed Bamba | Texas
7. Wendell Carter | Duke
8. Collin Sexton | Alabama
9. Kevin Knox | Kentucky
10. Mikal Bridges | Villanova
11. Shai Gilgeous-Alexander | Kentucky
12. Miles Bridges | Michigan State
13. Jerome Robinson | Boston College
14. Michael Porter | Missouri
15. Troy Brown | Oregon
16. Zhaire Smith | Texas Tech
17. Donte DiVincenzo | Villanova
18. Lonnie Walker | Miami
19. Kevin Huerter | Maryland
20. Josh Okogie | Georgia Tech
21. Grayson Allen | Duke
22. Chandler Hutchison | Boise State
23. Aaron Holiday | UCLA
24. Anfernee Simons | NON-COLLEGE
25. Moritz Wagner | Michigan
26. Landry Shamet | Wichita State
27. Robert Williams | Texas A&M
28. Jacob Evans | Cincinnati
29. Dzanan Musa | NON-COLLEGE
30. Omari Spellman | Villanova
31. Elie Okobo | NON-COLLEGE
32. Jevon Carter | West Virginia
33. Jalen Brunson | Villanova
34. Devonte' Graham | Kansas
35. Melvin Frazier | Tulane
36. Mitchell Robinson | NON-COLLEGE
37. Gary Trent Jr. | Duke
38. Khyri Thomas | Creighton
39. Isaac Bonga | NON-COLLEGE
40. Rodions Kurucs | NON-COLLEGE
41. Jarred Vanderbilt | Kentucky
42. Bruce Brown | Miami
43. Justin Jackson | Maryland
44. Issuf Sanon | NON-COLLEGE
45. Hamidou Diallo | Kentucky
46. De'Anthony Melton | USC
47. Sviatoslav Mykhailiuk | Kansas
48. Keita Bates-Diop | Ohio State
49. Chimezie Metu | USC
50. Alize Johnson | Missouri State
51. Tony Carr | Penn State
52. Vince Edwards | Purdue
53. Devon Hall | Virginia
54. Shake Milton | SMU
55. Arnoldas Kulboka | NON-COLLEGE
56. Ray Spalding | Louisville
57. Kevin Hervey | UT Arlington
58. Thomas Welsh | UCLA
59. George King | Colorado
60. Kostas Antetokounmpo | Dayton
""",
    2017: """
1. Markelle Fultz | Washington
2. Lonzo Ball | UCLA
3. Jayson Tatum | Duke
4. Josh Jackson | Kansas
5. De'Aaron Fox | Kentucky
6. Jonathan Isaac | Florida State
7. Lauri Markkanen | Arizona
8. Frank Ntilikina | NON-COLLEGE
9. Dennis Smith | NC State
10. Zach Collins | Gonzaga
11. Malik Monk | Kentucky
12. Luke Kennard | Duke
13. Donovan Mitchell | Louisville
14. Bam Adebayo | Kentucky
15. Justin Jackson | North Carolina
16. Justin Patton | Creighton
17. D.J. Wilson | Michigan
18. TJ Leaf | UCLA
19. John Collins | Wake Forest
20. Harry Giles | Duke
21. Terrance Ferguson | NON-COLLEGE
22. Jarrett Allen | Texas
23. O.G. Anunoby | Indiana
24. Tyler Lydon | Syracuse
25. Anzejs Pasecniks | NON-COLLEGE
26. Caleb Swanigan | Purdue
27. Kyle Kuzma | Utah
28. Tony Bradley | North Carolina
29. Derrick White | Colorado
30. Josh Hart | Villanova
31. Frank Jackson | Duke
32. Davon Reed | Miami
33. Wesley Iwundu | Kansas State
34. Frank Mason | Kansas
35. Ivan Rabb | California
36. Jonah Bolden | NON-COLLEGE
37. Semi Ojeleye | SMU
38. Jordan Bell | Oregon
39. Jawun Evans | Oklahoma State
40. Dwayne Bacon | Florida State
41. Tyler Dorsey | Oregon
42. Thomas Bryant | Indiana
43. Isaiah Hartenstein | NON-COLLEGE
44. Damyean Dotson | Houston
45. Dillon Brooks | Oregon
46. Sterling Brown | SMU
47. Ike Anigbogu | UCLA
48. Sindarius Thornwell | South Carolina
49. Vlatko Cancar | NON-COLLEGE
50. Mathias Lessort | NON-COLLEGE
51. Monte Morris | Iowa State
52. Edmond Sumner | Xavier
53. Kadeem Allen | Arizona
54. Alec Peters | Valparaiso
55. Nigel Williams-Goss | Gonzaga
56. Jabari Bird | California
57. Sasha Vezenkov | NON-COLLEGE
58. Ognjen Jaramaz | NON-COLLEGE
59. Jaron Blossomgame | Clemson
60. Alpha Kaba | NON-COLLEGE
""",
    2016: """
1. Ben Simmons | LSU
2. Brandon Ingram | Duke
3. Jaylen Brown | California
4. Dragan Bender | NON-COLLEGE
5. Kris Dunn | Providence
6. Buddy Hield | Oklahoma
7. Jamal Murray | Kentucky
8. Marquese Chriss | Washington
9. Jakob Poeltl | Utah
10. Thon Maker | NON-COLLEGE
11. Domantas Sabonis | Gonzaga
12. Taurean Prince | Baylor
13. Georgios Papagiannis | NON-COLLEGE
14. Denzel Valentine | Michigan State
15. Juan Hernangomez | NON-COLLEGE
16. Guerschon Yabusele | NON-COLLEGE
17. Wade Baldwin IV | Vanderbilt
18. Henry Ellenson | Marquette
19. Malik Beasley | Florida State
20. Caris LeVert | Michigan
21. DeAndre' Bembry | Saint Joseph's
22. Malachi Richardson | Syracuse
23. Ante Zizic | NON-COLLEGE
24. Timothe Luwawu-Cabarrot | NON-COLLEGE
25. Brice Johnson | North Carolina
26. Furkan Korkmaz | NON-COLLEGE
27. Pascal Siakam | New Mexico State
28. Skal Labissiere | Kentucky
29. Dejounte Murray | Washington
30. Damian Jones | Vanderbilt
31. Deyonta Davis | Michigan State
32. Ivica Zubac | NON-COLLEGE
33. Cheick Diallo | Kansas
34. Tyler Ulis | Kentucky
35. Rade Zagorac | NON-COLLEGE
36. Malcolm Brogdon | Virginia
37. Chinanu Onuaku | Louisville
38. Patrick McCaw | UNLV
39. David Michineau | NON-COLLEGE
40. Diamond Stone | Maryland
41. Stephen Zimmerman | UNLV
42. Isaiah Whitehead | Seton Hall
43. Zhou Qi | NON-COLLEGE
44. Isaia Cordinier | NON-COLLEGE
45. Demetrius Jackson | Notre Dame
46. A.J. Hammons | Purdue
47. Jake Layman | Maryland
48. Paul Zipser | NON-COLLEGE
49. Michael Gbinije | Syracuse
50. Georges Niang | Iowa State
51. Ben Bentil | Providence
52. Joel Bolomboy | Weber State
53. Petr Cornelie | NON-COLLEGE
54. Kay Felder | Oakland
55. Marcus Paige | North Carolina
56. Daniel Hamilton | UConn
57. Wang Zhelin | NON-COLLEGE
58. Abdel Nader | Iowa State
59. Isaiah Cousins | Oklahoma
60. Tyrone Wallace | California
""",
    2015: """
1. Karl-Anthony Towns | Kentucky
2. D'Angelo Russell | Ohio State
3. Jahlil Okafor | Duke
4. Kristaps Porzingis | NON-COLLEGE
5. Mario Hezonja | NON-COLLEGE
6. Willie Cauley-Stein | Kentucky
7. Emmanuel Mudiay | NON-COLLEGE
8. Stanley Johnson | Arizona
9. Frank Kaminsky | Wisconsin
10. Justise Winslow | Duke
11. Myles Turner | Texas
12. Trey Lyles | Kentucky
13. Devin Booker | Kentucky
14. Cameron Payne | Murray State
15. Kelly Oubre | Kansas
16. Terry Rozier | Louisville
17. Rashad Vaughn | UNLV
18. Sam Dekker | Wisconsin
19. Jerian Grant | Notre Dame
20. Delon Wright | Utah
21. Justin Anderson | Virginia
22. Bobby Portis | Arkansas
23. Rondae Hollis-Jefferson | Arizona
24. Tyus Jones | Duke
25. Jarell Martin | LSU
26. Nikola Milutinov | NON-COLLEGE
27. Larry Nance Jr. | Wyoming
28. R.J. Hunter | Georgia State
29. Chris McCullough | Syracuse
30. Kevon Looney | UCLA
31. Cedi Osman | NON-COLLEGE
32. Montrezl Harrell | Louisville
33. Jordan Mickey | LSU
34. Anthony Brown | Stanford
35. Willy Hernangomez | NON-COLLEGE
36. Rakeem Christmas | Syracuse
37. Richaun Holmes | Bowling Green
38. Darrun Hilliard | Villanova
39. Juan Pablo Vaulet | NON-COLLEGE
40. Josh Richardson | Tennessee
41. Pat Connaughton | Notre Dame
42. Olivier Hanlan | Boston College
43. Joe Young | Oregon
44. Andrew Harrison | Kentucky
45. Marcus Thornton | William & Mary
46. Norman Powell | UCLA
47. Arturas Gudaitis | NON-COLLEGE
48. Dakari Johnson | Kentucky
49. Aaron White | Iowa
50. Marcus Eriksson | NON-COLLEGE
51. Tyler Harvey | Eastern Washington
52. Satnam Singh | NON-COLLEGE
53. Sir'Dominic Pointer | St. John's
54. Dani Diez | NON-COLLEGE
55. Cady Lalanne | Massachusetts
56. Branden Dawson | Michigan State
57. Nikola Radicevic | NON-COLLEGE
58. J.P. Tokoto | North Carolina
59. Dimitrios Agravanis | NON-COLLEGE
60. Luka Mitrovic | NON-COLLEGE
""",
}

ENUM = re.compile(r"^\s*\d+\s*[.|]\s*")


def parse(block: str) -> list[dict]:
    out = []
    for line in block.strip().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or line.startswith("*"):
            continue
        line = ENUM.sub("", line)
        if "|" not in line:
            continue
        parts = [p.strip() for p in line.split("|")]
        name, team = parts[0], parts[-1]
        if "NON-COLLEGE" in team.upper():
            continue
        out.append({"name": name, "current_team": team, "status": "gone"})
    return out


def main() -> None:
    for year, block in sorted(RAW.items()):
        rows = parse(block)
        path = OUT_DIR / f"{year}_early_entrants.json"
        path.write_text(json.dumps(rows, indent=2) + "\n")
        print(f"{year}: wrote {len(rows)} college entrants → {path.name}")


if __name__ == "__main__":
    main()
