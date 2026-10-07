# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Download growth no longer goes negative when a release leaves the repo.** The growth badges
  measured the download total's movement, and that total is GitHub's own sum, the figure a
  downloads badge shows. Moving a repo's resource releases to a separate repo, where the
  re-uploaded assets start again from zero, therefore took 1,608 downloads off the total, and every
  period that spanned the move showed its growth as −1,264 instead of the downloads actually
  gained. Growth now adds what each asset gained since its own previous reading, which is the
  arithmetic Recent changes already used. An asset's first reading is its baseline rather than
  growth, so a renamed asset counts nothing. An asset that leaves adds nothing, and a count that
  fell, from an asset deleted and uploaded again under the same name, adds nothing either. The
  total and its chart still follow GitHub and drop when a release leaves. Carrying a departed
  release's last sum in the total was the alternative, and it was tried and reverted: it set the
  total above the README badge. The cost: downloads a new asset collects before its first reading
  are in the total but never in growth.

## [1.4.0] - 2026-10-02

### Added

- **Event impact lines (repo page, Events).** Each event row carries one muted line under its title
  comparing views per day over the seven complete UTC days before the event with the days from the
  event to yesterday, plus the star change over each window: `Views/day 38 → 89 (+134%) · stars ±0
  → +6 (2 days so far)`. Whether a post moved anything is the question the event log exists to
  answer, and until now the reader had to hover the chart either side of a marker and do the
  division by eye.

  It is a rate per *observed* day rather than a sum, because the after-window is still filling and
  is shorter than the before-window by design; a gap shrinks the denominator instead of counting as
  a zero day. Today's partial bucket is left out of both windows, uniques are not used because they
  cannot be summed, and the line is omitted when either window has fewer than two observed days.
  The percentage is worked out from the two rounded rates printed beside it, so the line never
  contradicts itself — computing it from the unrounded rates printed `2 → 2 (+43%)` on real data —
  and it is shown only when the before-rate is at least three views a day, below which a percentage
  is noise. Another event inside either window is flagged `(overlaps another event)` rather than
  hidden: the reader can judge an overlap, the code cannot. The series are the dense ones the page
  already loads, and an add, edit or delete swap reads them over the page's own window, so a
  re-rendered row agrees with the one the page load drew.

- **Views change against the previous period (Analytics).** The repo table's Views cell carries a
  signed percentage: the last N complete UTC days against the N days before them. Unlike the impact
  line it compares two equal spans, so it needs every day of both windows observed and an earlier
  sum above zero — a gap would make one sum cover fewer days than the other — and otherwise renders
  nothing, never `NaN%` or `Infinity%`. All has no previous period and no figure. Every period's
  value ships behind the hidden-span pattern, so a period change costs no request and the page is
  right with JavaScript off. "Reading the numbers" in ARCHITECTURE.md now says why the two
  comparisons treat gaps differently, and why the Views figure, which includes today, and its
  change, which does not, are not read off the same days. It is the leaderboard rather than a KPI
  tile because the leaderboard is where repos are compared.

- **Repository switcher and previous/next links (repo page).** The header gains a crumb back to
  Repositories, a Switch repository menu listing every tracked repo with the current one marked,
  Previous and Next in name order, a GitHub link, and the ⚠ sync-failure glyph beside the name when
  the last sync failed. Moving from one repo to the next used to mean a trip back through the
  dashboard. The list is the dashboard's own `repo_overview` read, made inside the page's existing
  `Db::call`, so the page still renders from one call and no query was added. On a repo page the
  Repositories nav entry is marked as the current section (`aria-current="true"`, a section rather
  than a page).

- **Sync failures on Repositories and Analytics.** When any tracked repo's last sync failed, both
  overview pages open with one line — "1 repository failed its last sync — see Settings" — linking
  to the picker, and the repo's card or leaderboard row carries the ⚠ glyph with the stored error
  category. Failures were visible only in the Settings picker, which is not where anyone looks day
  to day. The data was already in each page's `Db::call`, and only the category reaches the page.

- **Period carried in links between pages.** A non-default period now travels as `?days=N` in the
  repo header's crumb, switcher and previous/next links, in the Analytics leaderboard and Recent
  changes rows, and in the nav's Analytics link on a repo page; All removes it. Choosing 30 days on
  Analytics and opening a repo used to land on its whole history. The hrefs are rewritten in the
  browser on load and on every period change, by the same rule as the address bar, so with
  JavaScript off they are plain links. Remembering the period across visits was left out: a stored
  period changes what every page opens on, which makes it an implicit preference.

- **Share bars behind traffic-source names (repo page).** Each referrer and path name cell is shaded
  in proportion to its views against the largest row shown. The width is one of 21 `wp-share-N`
  classes in 5% steps rather than an inline style, which the CSP forbids, and the bar is CSS alone,
  so it survives a sort swap and works with JavaScript off. The number stays the accessible value;
  the bar is decoration.

- **A notice when JavaScript is off.** Every page opens its content with a `<noscript>` notice:
  charts and editing need JavaScript, and the tables and totals are complete without it. Before, a
  reader with scripts blocked saw empty chart space and dead buttons with no reason given. A
  `noscript` element is not a script, so the CSP is unaffected.

### Changed

- **Traffic sources as two side-by-side tables (repo page).** The Views section was two full-width
  20-row tables that took most of the page. It is now Traffic sources: Referrers and Paths side by
  side from 64rem and stacked below it, ten rows each, with a Show all button for the rest. A muted
  line says what the tables cover — "All time since Aug 2, not affected by the period above.
  Uniques is a peak, never a total." Both were true before and stated nowhere, and the period
  selector just above made the tables look filtered. The column is headed Peak uniques for the same
  reason. Paths display relative to the repo, with the full path in the cell's title, and GitHub's
  page title shows only when it says something the path does not. Name sorting ignores case.

  Every row is still server-rendered. The collapse is a class JavaScript applies, so with
  JavaScript off every row shows, and the expanded state survives a sort swap. The date in the line
  is the first observed views day in the payload the page already ships, so there is no new query.

- **A shorter events list (repo page).** Event rows are one line each — Date, Kind, Event, Actions —
  with notes behind a disclosure under the title instead of a column of their own. Edit and Delete
  are quiet outlined buttons, Delete in danger ink, each with a visually hidden name such as "Edit
  r/ajatt, 2026-09-01" so a screen reader knows which row it acts on. Only the newest ten events
  show, with "Show N older events" for the rest. Pressing a kind chip expands the list first, so a
  filtered view is never partial; editing an older event keeps it in view; a click on a chart column
  opens the list before scrolling to the row. The collapse is a class rather than `hidden`, because
  the kind filter already owns `hidden`. `#events-data` and the chart markers keep the full list.

  Below 40rem each row is a small grid — date, kind and actions, then the title, then the notes —
  and the edit row follows the same grid, so editing on a phone never scrolls sideways.

- **Compact event form with confirmations (repo page).** "+ Add event" is a compact button on the
  chip row, and the open form puts Date, Title and Kind on one row. The form is `novalidate`, so
  every rejection comes from the server's one validation path and renders like the edit row's,
  rather than a browser bubble for some fields and a server message for others. An empty date says
  "Pick a date.", and kinds are capped at 40 characters with a matching `maxlength`. Add, save and
  delete each confirm with one line ("Event added.") that a screen reader hears once.

  Delete's dialog names the event — `Delete “r/ajatt” (2026-09-01)? This cannot be undone.` — with a
  danger-styled Delete button. Any `hx-confirm` trigger can now set the dialog's heading, button
  label and danger style through `data-confirm-title`, `data-confirm-label` and
  `data-confirm-danger`; a trigger without them gets the plain Confirm dialog as before. The form
  also carries `method="post"` and an action, so a submit with JavaScript off is a POST the CSRF
  check refuses with the styled page, never a GET that drops the entry.

- **KPI tile captions (repo page).** Every tile says what its figure covers: "+18 in 30 days" on a
  level tile, "in 30 days" on a rate tile, and "since Feb 12" at All, the first observed day being
  where an all-time figure honestly starts. A series with no observation gets no caption, never
  "+0". The captions ship per period in hidden spans, like the deltas, so they are right with
  JavaScript off and a period change writes no text. Below 40rem the tiles sit two to a row, and an
  odd last tile spans it.

- **One chart tooltip for figures and events.** Hovering a column shows its values and, under a
  hairline, the events in that bucket with their kind chips. A column with an event used to show the
  marker's own tip instead of the numbers, so the two things a reader wants side by side were never
  on screen together. Event markers sit in a lane of their own above the plot, clear of the top
  gridline and the line itself, and only that lane hit-tests as a marker. A click on a column with
  events still jumps to the event row, and the tooltip lists series in legend order.

  A week or month bucket that was only partly observed says so in its title — "Week of 2026-07-27 ·
  1 of 7 days observed" — and draws its bar faded. A sparse week summed into one bar otherwise reads
  as a quiet week. Every plotted value and the day zoom are unchanged.

- **One accent colour for every chart series.** Stars, views, clones, downloads and container pulls
  all draw in one blue with its gradient wash; the uniques line under views and clones is muted ink
  and thinner, and the legend tells the two apart. The per-metric hues were borrowed event-kind
  slots, so a Downloads line could share its red with reddit's markers, and red and green belong to
  delta direction. Event kinds now skip the accent's slot: a kind whose hash lands on slot 0
  (youtube, twitter, launch) is moved to one of the other seven, in the server's chip and the
  client's marker alike, so no dot or chip reads as part of the series. Only those kinds change
  colour. Hashing every kind over seven slots would have recoloured most existing badges, and a
  ninth colour would sit next to one of the eight already spread round the colour wheel.

- **Recent changes grouped by day (Analytics).** The heading reads "Recent changes · last 14 days",
  each UTC day appears once as a small label above its rows, and each repo's deltas sit right after
  its name instead of pushed to the far edge. The list keeps 20 rows, and when there were more a
  closing line says older changes are on each repository's page, rather than the list stopping
  without saying so. The labels are the stored UTC dates and are never relabelled "Today", which
  would be wrong for a reader east or west of UTC for part of every day.

- **Leaderboard columns labelled by period (Analytics).** The Growth and Views headers say which
  period they follow ("Growth · 30 days", "· all time" at All), and Downloads and Container pulls say
  "total", because those two ignore the period. The table fits all six columns at 768px, and below
  40rem the name column stays pinned while the figures scroll. Zero growth reads "±0", like the tile
  badges, through the one signed-number formatter the tiles, the leaderboard, the feed, the cards and
  the impact line now share.

- **Dashboard cards (Repositories).** Footers line up across a row, a hairline replaces Pico's
  shadow, and the whole card opens the repo, with the ⚠ glyph and the last-synced time raised above
  the stretched link so their tooltips still show. A card carries its star growth ("+18 stars · 30
  days") when growth was observed, a lone reading draws a dot instead of an invisible sparkline, and
  "0 events" is left out. A repo that has never synced shows "Waiting for first sync · in 42m"
  instead of an empty sparkline over dashes that read as zeroes.

- **First-run guidance from setup to the first sync.** With nothing tracked, Repositories and
  Analytics say why — "No repositories tracked yet — watchpost only collects the ones you pick." —
  and link straight to the picker. Setup says it is step one of two and, on a valid token, goes to
  the picker rather than to an empty dashboard. Saving the picker says what happens next: "Saved —
  tracking 6 repositories. New ones fill in on the next sync (in 42m), or press Sync now above."
  Starting a sync on Save was left out: it brings GitHub calls forward from the schedule.

- **Repository picker order and layout (Settings).** Tracked repos list first, and the rest sit in a
  collapsed "Not tracked (N)" group inside the same form, so a Save still submits them. The table
  fits a phone, the ⚠ glyph and the fork and archived tags sit after the name, an untracked row
  leaves Last synced blank rather than saying "never", and Save sits under the table beside a "14 of
  26 tracked" count. With no repos loaded, only Refresh from GitHub shows.

- **Settings and setup layout and wording.** Settings is one centred column with inputs capped at a
  readable width. The three save buttons say what they save — Save start page, Save interval, Save
  selection — and the copy keeps one vocabulary: "repositories" in sentences, "track" as the verb,
  every notice a full sentence. The setup page shows the brand without nav links, which could only
  ever redirect back to it.

- **Current section marked in the nav.** The current nav item is ink, bold and underlined, and the
  brand is ink rather than link blue. Before, every nav link looked the same on every page.

- **One type scale, section rhythm and table style.** `h2` is 1.25rem at weight 600, clearly below
  the 2rem `h1`; sections sit one 2rem gap apart; small text has one size. Every data table shares
  one numeric style — right-aligned tabular figures, names that wrap, sort links in ink — and a
  table wider than its wrapper shows a scroll-edge shadow on the cut-off side. The period select
  matches the height of the buttons beside it, and the Analytics portfolio chart drops its card frame
  to match the repo page's chart. On a touch screen sort links, chips, disclosures and row actions
  get larger hit areas; with a mouse the look is unchanged.

### Fixed

- **Release downloads count a renamed asset once.** The download total carried every
  `(release_tag, asset_name)` pair forward on its own, on the reasoning that an asset with no row on
  a given day had simply not been re-read. That never happens: `/releases` is paginated
  all-or-nothing and the collector writes every asset of every release whenever it answers, so a
  pair missing from a later day is one GitHub no longer lists. GitHub keeps an asset's count across
  a rename, which meant a renamed asset was counted once under each name, and a deleted asset
  stayed in the total for good. One repo read 156 where GitHub and its downloads badge said 139,
  the difference being two APKs renamed after upload. The total and the chart now sum the newest
  day's rows and carry that day total across days with no read. A deleted asset therefore leaves the
  total, as it does on GitHub. Keying rows by GitHub's asset id was the alternative: it survives a
  rename, but it still keeps deleted assets, needs a migration, and has no ids for the rows already
  written.

  The same double count happened within a single day. The rename landed between two polls, and the
  day-keyed upsert only ever adds or raises rows, so the day kept the asset under both names and the
  total stayed doubled until the next day's read. Each successful read now also deletes that day's
  rows for assets it no longer lists, which makes a day's rows its last reading, as the storage
  rules already said. The rows written before this release keep that day's double count, which
  shows as a one-day spike on the chart. A read that answers with no assets at all leaves the
  day empty, so the previous day's total carries. That only happens when every asset has been
  deleted.

- **Portfolio badges no longer count a newly tracked repo as growth (Analytics).** The badges took
  the period's movement of the summed star curve, and that curve steps up by a repo's whole level
  on the day watchpost first reads it. Tracking a 400-star repo therefore showed as +400 stars of
  growth, and the badge disagreed with the Growth column beneath it. Each badge is now the
  None-aware sum of the per-repo growth figures, which already measure from a repo's first observed
  value inside the window, so a badge equals its column added up. The curve keeps its documented
  step.

- **The sync panel no longer reports a skipped or rate-limited cycle as a success (Settings).** A
  cycle with every repo in backoff, or with nothing tracked, read as a green "Synced 0 repos", and a
  rate-limited cycle as a green success for however many repos it reached first. The status now carries how many repos were skipped and
  whether the cycle stopped on the rate limit or failed, set by the collector where it happens
  rather than parsed back out of the cycle report's strings. Each case gets its own line: "Synced 4
  repositories · 2m ago"; "GitHub rate limit reached — sync stopped after 2 of 6. Resumes in 38m";
  a muted "2 skipped after recent errors; retried after their backoff"; "Nothing to sync yet — pick
  repositories below."; and, when every repo came back partial, "No repository fully synced", since
  partial reads have already written rows. A failed cycle shows a fixed category and no detail.

- **A rejected setup token no longer nests a second page inside the form.** The error branch
  answered with the whole setup page, which htmx swapped into the form, so a bad token showed a
  second nav and heading inside the first. It now answers with the form alone, the notice above the
  field, the field marked invalid and described by the notice, and focus back in it. Every setup and
  settings form also carries `method="post"` and an action matching its `hx-post`, so a submit
  before htmx loads is a POST and a token never lands in the address bar.

- **Unknown addresses and malformed ids show the styled Not found page.** An unknown path answered
  an empty 404 and a wrong method a bare 405, and `/repos/abc` answered axum's plain-text "Cannot
  parse `abc` to a `i64`". Every one of them now renders the app's own page, inside the setup gate,
  CSRF check and security headers, and never echoes the path, the method or the parser's message. A
  4xx page is a calm status notice; a 5xx keeps its alert.

- **Pages no longer scroll sideways on a phone.** Long repo names break after the owner's slash, the
  nav wraps at 360px and 320px, an edit row's hidden labels are clipped by the table wrapper rather
  than widening the page, a long kind chip stops at its cell, and the toast keeps equal gutters.
  Wide tables scroll inside their own wrapper, never the page.

- **Muted text, deltas and borders meet AA contrast in both themes.** Delta figures used the chart
  mark colours, which reach 3:1 — enough for a mark, not for text — and now read Pico's text-grade
  insert and delete colours. Muted text in the dark theme is lifted to 4.5:1 on the card surface, a
  dark chart slot that was byte-identical to its light twin is re-stepped, and tooltips, toasts and
  the skip link share one border token defined in both themes.

- **Keyboard focus and chip state show without colour or hover.** Every focusable control draws one
  opaque 2px ring set off from its edge, which Pico's translucent ring did not reliably show; a
  focused KPI tile differs from an unfocused one, and the tiles no longer draw one group ring
  around the whole row. A switched-off kind chip shows a hollow dot and a muted label, so on and off
  differ in shape and not only in colour, and unchecked checkboxes have a border that reads.

- **Swapped confirmations are announced once.** A status that arrives together with its text in an
  htmx swap is not reliably spoken, and a poll that re-renders unchanged text can be spoken again.
  Every page now has one visually hidden live region outside `<main>`, and a settle hook copies the
  first `data-announce` text of a swap into it only when it changed; the event confirmations and the
  sync sentence use it. When the control that started a swap is gone afterwards, focus lands on the
  nearest focusable container, such as the sync panel, rather than on `<body>`.

### Security

- **rustls 0.23.45, past RUSTSEC-2026-0285.** The TLS stack under every GitHub API call accepted
  TLS 1.3 handshake messages across encryption-level boundaries; 0.23.45 rejects them. watchpost
  only speaks TLS to the configured API base and the GHCR package page, so the exposure was to a
  hostile or intercepting server on that path, but the advisory fails the audit job and the fix is
  a lockfile bump with no code change. `chacha20` moves from the yanked 0.10.1 to 0.10.2 in the
  same update. Both versions resolve under the 1.88 MSRV.

## [1.3.0] - 2026-09-08

### Added

- **A start page on the settings page.** Which page the bare root opens on is now a choice between
  the three nav destinations, stored as a slug in the `settings` table beside the token and the
  interval — the third runtime setting, and still no migration, which is what v3's generic table was
  for.

  Making the root honour it meant giving the repositories dashboard its own path, `/repos`. The
  cheaper-looking option — leave `/` rendering the dashboard and redirect away from it only when the
  setting points elsewhere — strands the dashboard: the nav's "Repositories" link points at `/`, so
  it would redirect straight back out again and the page would be unreachable from inside the app.
  With the split, `/` is a pure redirector and means "my start page" rather than "the dashboard".
  That is also why nothing else had to change: the desktop shortcut the installer writes, the setup
  gate's redirect off a completed wizard, and the wizard's own `hx-redirect` all point at `/` and now
  honour the setting for free.

  The hop is a 303 with `cache-control: no-store`. A permanent redirect would be cached
  indefinitely, so a later change to the setting would appear to do nothing — in the reporter's
  profile only, and irreproducible in a fresh one. The stored value is a slug rather than a path
  because a path could be hand-edited to `/`, which the redirector would follow into a loop; parsing
  through a closed enum makes that unrepresentable, and an unrecognised slug warns and takes the
  default rather than stranding the front page. The nav now renders from the same array the parser
  validates against, so a nav href and a landing path cannot drift apart.

  There is deliberately no environment variable and no `--doctor` line. A start page is a preference
  of whoever reads the pages rather than a statement the deployment makes, and `--doctor` reports
  what can be broken — this cannot be.

### Changed

- The per-period movement badge no longer renders on the "All" period, on the repo page's KPI tiles
  and the analytics portfolio totals. `growth` over the whole history is the last observed reading
  minus the first, which for any repo watchpost has watched since it had nothing is the level again
  — an unlabelled green number sitting directly under the number it restates, and indistinguishable
  from a total to anyone who has not read the code. Every real window keeps its badge, which is
  where a movement figure answers a question the reader asked.

  The spans for the other four periods stay in the markup when "All" is selected, all hidden. A
  period change is a client-side `hidden` flip with no request behind it, so removing them would
  mean a switch back to 30 days had no 30-day figure left to reveal. `.wp-kpi-delta` already
  reserves its row, so a tile keeps its height with the badge empty and switching period does not
  move the layout.

  The leaderboard's `Growth` column is unchanged. It sits under a heading rather than under the
  number it would restate, and blanking it would mean teaching `updatePeriodValues` about `<th>`
  elements for a redundancy the heading already explains.

- The settings page gathers everything it renders in one `Db::call` rather than two, restoring the
  one-call-per-page-render rule the new panel would otherwise have pushed to three. The schedule
  resolver grew a pure half (`resolved`) that takes an already-read value, so the panels' own
  database-reading helpers stay for the fragment swaps that want them.

## [1.2.0] - 2026-08-29

### Added

- **A sync interval on the settings page.** How often watchpost collects is now a field rather than
  a six-field cron expression in a file the container may not be able to write: `10m`, `6h`,
  `1h 30m` — terms add up, and the units are `m`, `h`, `d` and `w`. It is stored in the `settings`
  table beside the token, costing no migration (v3 was made generic for exactly this), and applied
  to the running scheduler, so a change takes effect without a restart. The panel names the next
  cycle, which is what makes the change verifiable rather than a claim.

  The bounds are rejections, not clamps, and both name a real limit. Below 5m a cycle can still be
  running when the next one is due — nothing breaks, because an overlapping tick is dropped, but the
  schedule stops describing what happens. Above 14d the gap is wider than GitHub's traffic
  retention, so days are lost permanently, which is the one thing watchpost exists to prevent.

  Interval schedules are `tokio-cron-scheduler` repeating jobs rather than a cron expression
  synthesized from the interval: an arbitrary duration has no honest six-field spelling, and the
  scheduler tracks a repeating job's next tick, which is where the countdown comes from. Cron stays
  the shape of the default, so an install that sets nothing keeps firing at five past the hour
  rather than at whatever minute the process booted.

  `WATCHPOST_CRON` is unchanged and still wins when set, on the same reasoning as the token: it is
  the deployment's own statement of intent, and a compose file that sets it would otherwise silently
  disagree with a value saved from a browser. In that state the panel is a statement rather than a
  form, and a POST from a stale page writes nothing. `--doctor` reports the effective schedule and
  which of the three sources it came from.

- **An Analytics page** (third nav entry, `/analytics`), answering how the tracked repos are doing
  where the dashboard answers which ones they are. Three sections, and one period selector in the
  header scoping the first two.

  The **portfolio** opens with the current totals — stars, forks, open issues, open PRs, each the
  sum of every tracked repo's latest observed row — over a combined star curve. The curve is built
  by summing each repo's dense carried-forward series in Rust rather than by a cross-repo query:
  `dense_series` is the single definition of what a gap and a carried-forward level mean, and a
  second reader would have been a second definition. It is also faster here, because migration v2
  deliberately dropped the index a `date`-leading scan of `repo_stats` would need. A repo watchpost
  had not started watching contributes nothing to the days before its first reading rather than a
  zero, so its arrival is a step up in the total and never a dip through it.

  The **repo table** ranks every tracked repo by stars, with columns for star growth over the
  selected period, views over it, release downloads to date and GHCR container pulls to date. One
  table with several columns rather than as many "top by X" lists: the portfolio is the same handful
  of repos the dashboard renders as cards, four rankings over five names carry no information, and
  one table answers the cross-question four cannot — that the repo with the most stars gets the
  fewest views. Growth measures from a repo's first *observed* value inside the window, not from the
  window's edge, so a repo first seen halfway through reports a real difference between two real
  readings instead of its whole star count. Downloads are the newest count per release asset summed,
  not a sum of daily rows, which would multiply the same cumulative counter by the number of days it
  was read; container pulls are the newest reading outright, one row already being the whole
  counter. The two distribution columns sit side by side because they answer the same question about
  different distributions — a project shipping an image publishes no release assets, and downloads
  alone said nothing about how far it had travelled. Both come from data the collector was already
  writing and the repo pages were already charting. A column nothing ever filled is not rendered at
  all.

  The **recent-changes feed** lists what moved, one row per repo per UTC day: stars, forks,
  watchers, open issues, open PRs, release downloads and container pulls, each as a signed delta.
  Four rules decide what counts as a change. The predecessor is the last *observed* value rather
  than the previous calendar day, so a sync gap produces one change on the day the next observation
  landed instead of a phantom pair; a first observation is dropped, because the first sync of a
  400-star repo is a reading and "+400 stars" would be a fiction; a zero delta produces no row at
  all; and views and clones are excluded, being per-day rates where the day's own value already is
  the change. Day resolution is what `repo_stats` stores, so the feed renders the entire existing
  history the moment it ships, backfilled star history included.

  No schema migration and no new configuration. The period selector is the repo page's, extracted
  into a module that now owns the allowlist, the parser and the control together — so an option can
  never be offered that the parser then rejects. The chart is the repo page's too: the page ships
  the portfolio total under the series name and canvas id that page already uses, so the bucketing,
  the zoom, the theme following, the tooltip and the gradient all arrive already written and the
  only new client code is a six-line `hidden` flip for the table's period columns.

- **CSV and JSON export per repo** (`Export: CSV · JSON` in the repo page header). Both span the
  whole history and take no period, because the history is the point. The CSV is the chart data
  flattened — one row per UTC day, built from the same dense readers the charts plot, so a cell and
  the same day on a chart are the same number by construction rather than by agreement. The JSON is
  the raw record: observed rows only, no carry-forward, plus the release assets, container pulls,
  referrers, paths and events that have no place in a daily grid, stamped with the schema version
  the file was written at. An unobserved counter is an empty field in the CSV and `null` in the
  JSON, never a `0` — a file that filled gaps with zeroes would re-introduce on the way out the lie
  watchpost refuses to tell on the way in. Sync errors, backoff state and the saved token are
  operational rather than history and appear in neither. Both are plain read-only GETs, and like
  every other page they are unauthenticated. The repo-name half of the filename is sanitised, since
  it is upstream-owned and would otherwise be able to write the response's own headers.

### Changed

- **The repo page leads with KPI tiles over one hero chart.** The grid of four 240px chart cards
  is gone: each observed metric is now a stat tile — its level, and for the carried-forward series
  a per-period delta — above a single full-width chart, and clicking a tile switches which metric
  that chart plots. The switch is an `aria-pressed` and `hidden` flip over canvases already built
  from the one payload the page always shipped, so it costs no request; the tile figures are
  server-rendered per period behind the leaderboard's hidden-span contract, so they are right with
  JS off and a zoom writes no text. The per-period arithmetic (`growth`, `sum_observed`,
  `per_period`, `add_into`) moved from the analytics handler to a shared `src/series.rs` to feed
  both pages from one definition.

- **Daily counts plot as bars.** Views and clones drew ninety spiky points of line into a
  card-width plot and read as scribble; as rounded columns (capped thickness, translucent at rest,
  solid on hover) they read as the discrete daily counts they are. Uniques stay a line over the
  bars, palette slots unchanged, and a null day is a missing bar rather than a zero-height one —
  the same gap discipline `spanGaps: false` kept for lines.

- **Chart chrome recedes.** Gridlines drop to a hairline one step off the card surface in both
  schemes (the changes feed's row borders, which shared the token, now use Pico's own separator);
  the area wash under cumulative lines fades to fully transparent; and the built-in canvas tooltip
  is replaced by an HTML tip styled from app.css — same face as the marker tip, value before
  label, "not observed" for a gap — positioned through the CSSOM, which the zero-inline-style CSP
  permits. A solid grey crosshair marks the hovered column; it is deliberately not dashed, because
  dashes now mean nothing else on the plot.

- **Event markers rest as dots.** The always-on dashed drop lines — the loudest thing on every
  chart — are gone; a marker is its coloured dot at the top of the plot, and the drop line draws
  solid at half strength only while the pointer is in that marker's column. Hover tips, click-to-
  row and kind filtering are unchanged.

- **Analytics totals wear delta badges.** Stars, forks, open issues and open PRs each carry their
  movement over the selected period, measured by `growth` over the same summed dense series the
  portfolio chart plots — an observed flat period reads ±0, an unobserved one an em dash. The
  portfolio chart adopts the hero height.

- **The dashboard is repo cards again.** The recent-changes feed briefly sat above them; at up to
  twenty full-width rows it pushed the repos the page is named after off the first screen, which is
  the wrong trade for a supporting figure. It now lives on the Analytics page, last, under the
  numbers it explains.

## [1.1.0] - 2026-08-20

### Added

- **GHCR container pull counts, auto-detected.** Every sync also fetches each tracked repo's
  public package page (`github.com/{owner}/{repo}/pkgs/container/{name}`) and charts the
  cumulative pull count as a "Container pulls" card on the repo page. Scraped, because no GitHub
  API exposes the number; unauthenticated, so it costs no token scope and no rate budget. A repo
  without a package named after it 404s and is skipped — zero configuration. A failed scrape is a
  partial sync like any failing endpoint, and deliberately does not count toward the total-failure
  verdict that backs a repo off. Schema migration v4 adds the `container_pulls` table (day-keyed,
  monotonic MAX, same rules as release assets).

### Changed

- **The repo charts are redrawn on a validated palette.** Each series is a gradient-filled line on
  a palette checked for colourblind separation and 3:1 contrast against the card surface. The axis
  borders and vertical gridlines are gone, dates read `Aug 19` rather than `2026-08-19`, counts past
  five digits read `12.3K`, and the tooltip takes the card's own surface and ink instead of the
  library default. Event markers drop to a half-strength dashed line under a ringed dot, so a
  marker sits behind the data it annotates rather than across it.

- **Event-kind chips carry their colour on a dot, not in the text.** The label is body ink at every
  size, which frees the palette from having to be readable as small text and lets its two lightest
  slots be used as marks.

- **Chart cards with no observed data are hidden.** A repo that ships only docker images no longer
  shows a blank Downloads pane, and repos without container packages don't get a blank pulls pane;
  the same rule covers views/clones cards where the token lacks traffic permissions. A repo with
  nothing observed at all keeps its existing empty state.

## [1.0.0] - 2026-08-18

First release. Everything below is relative to the pre-release state of the repository, which was
never tagged.

### Added

- One-line install: `curl … | bash` on Linux and macOS, `irm … | iex` on Windows. Both pull a
  published image, write `PUID`/`PGID` so the bind-mounted `data/` is writable whatever the host
  uid is, wait for `/health`, and open the setup page. `scripts/update.sh` updates in place.
- A first-run setup page. An install with no `WATCHPOST_GITHUB_TOKEN` now boots and redirects every
  page to `/setup`, where a pasted token is checked against GitHub before it is saved and
  collection starts immediately. The token can be replaced from the settings page afterwards; only
  its last four characters are ever rendered. An environment token still wins and hides the form.
- Published multi-arch images at `ghcr.io/0xzerolight/watchpost`, `linux/amd64` and `linux/arm64`,
  built on a tag push.
- `PUID`/`PGID`: the container entrypoint aligns the data directory with the host user and drops
  privileges, replacing the manual `chown -R <uid> data` the README used to ask for.
- `WATCHPOST_TZ`: displayed times — "last synced", the `--doctor` rate-limit reset, and the day a
  new event defaults to — render in the configured IANA zone instead of always UTC. Stored dates,
  chart day buckets and `WATCHPOST_CRON` stay UTC.
- Global error toast: a failed request says so instead of failing silently.
- Loading indicators and disabled states on every request that can be waited on.
- Delete confirmation in a real dialog rather than a bare button.
- Visible sort-direction indicators on the sortable tables.
- Skip link, current-page state in the nav, and focus that survives a fragment swap.
- Response compression (gzip).
- Content-hashed asset URLs, served `immutable` for a year and revalidated with a 304.
- Schema migration v2, adding the indexes the page queries actually use.
- Pre-migration backups: the database is copied to `data/watchpost.v{schema}.{timestamp}.bak`
  before a schema upgrade, and the newest three are kept.
- AGPL-3.0-or-later license.

### Changed

- `WATCHPOST_GITHUB_TOKEN` is optional. Missing is no longer a startup error; it is the state the
  setup page exists to resolve. Schema v3 adds a `settings` table to hold a token saved there.
- `--doctor` reports which token is in use and where it came from, and fails an install that has
  none with a pointer to the setup page rather than a request that was never made.
- The pages are rebuilt on one shared component set: real labels on every form field, consistent
  headings, empty states, and a restructured repo, settings and index page.
- Spacing, type and colour are design tokens from a single source; the event-kind chips meet WCAG
  AA contrast.
- A period change re-scales the existing charts instead of rebuilding them.
- Inline event handlers are gone, replaced by delegated listeners.
- The repo overview is one query rather than one per repo, and the delta recompute is bounded to
  the window that can have changed.
- Minimum supported Rust version is 1.88, declared in `Cargo.toml`.
- Docker base images are pinned to explicit versions and the build caches its dependency layer; CI
  runs a locked build, an MSRV check, a Docker build and an advisory audit.

### Fixed

- The Add event form pre-filled the UTC day, so between local midnight and the UTC rollover it
  defaulted to the wrong date.
- A line chart with only one or two observed days drew nothing at all — the Downloads card on a
  repo whose releases were first read this week was an empty plot area under a correctly scaled
  axis. Such points now get a marker.
- Rate-limit classification is limited to 403 and 429; a transient 5xx is retried instead of being
  counted as a rate limit.
- Daily stats record the last observation of the day rather than the intraday maximum.
- Repo writes are transactional, so a failure part-way through no longer leaves a half-written repo.
- A partial sync no longer inflates the error streak and the backoff that follows from it.
- A database written by a newer build is refused at open with the version pair and the fix, instead
  of being served against a schema the binary does not know.
- Backup pruning keys off the embedded timestamp, so a v10 database no longer discards its newest
  backups first.
- The health endpoint verifies the database and queries the live schema.
- Background polls no longer clear the error toast or clobber a pending focus target.
- Enter submits the picker and edit-row forms.
- Sort links carry the client-side zoom instead of resetting it.
- Chart tooltips are clamped to the canvas, reduced-motion preferences are honoured, and the axis
  labels are readable.
- A poisoned mutex is recovered and a panicking handler is caught, rather than taking down the
  worker behind it.
- Configuration is validated at startup: the token, the API base URL and the log filter.

### Security

- Security headers on every response: Content-Security-Policy, X-Content-Type-Options,
  X-Frame-Options, Referrer-Policy and Cross-Origin-Opener-Policy.
- The policy allows no inline script or style at all (`script-src 'self'`, `style-src 'self'`), so
  an injected `<script>` or `onerror=` does not execute even if it survives the escaping.
- The CSRF cookie is checked for the shape this server mints, and carries `Secure` when the request
  arrived over HTTPS plus a 30-day `Max-Age` so it outlives the browser session.
- Repo discovery is a CSRF-gated POST; it used to be a GET that spent API calls as a side effect.
- Error responses carry no internal detail — no paths, no SQL, no upstream error strings.
- There is no authentication, and the README now says so outright: anyone who can reach the port
  has full read and write. Bind it to the loopback or put it behind a proxy that authenticates.
- `compose.yml` publishes to `127.0.0.1:8080` rather than every interface, so the default deployment
  is not reachable from the network.

[1.4.0]: https://github.com/0xzerolight/watchpost/releases/tag/v1.4.0
[1.3.0]: https://github.com/0xzerolight/watchpost/releases/tag/v1.3.0
[1.2.0]: https://github.com/0xzerolight/watchpost/releases/tag/v1.2.0
[1.1.0]: https://github.com/0xzerolight/watchpost/releases/tag/v1.1.0
[1.0.0]: https://github.com/0xzerolight/watchpost/releases/tag/v1.0.0
