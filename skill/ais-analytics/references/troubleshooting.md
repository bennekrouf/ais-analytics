# AIS Analytics troubleshooting

Known failures, grouped by where they show up. Each entry: what the user sees,
why, and what to do. Versions in brackets are where a fix arrived; if the
user is older, updating is often the answer.

## Contents

- [Starting and signing in](#starting-and-signing-in)
- [Scanning a workspace](#scanning-a-workspace)
- [Setup: the proposed key](#setup-the-proposed-key)
- [Signals, Logs and Dependencies](#signals-logs-and-dependencies)
- [Issues](#issues)
- [Tracing a value](#tracing-a-value)

## Starting and signing in

**"Azure CLI ('az') not found on PATH."**
Install the Azure CLI. An app started from Finder or the Start menu picks up
the login shell's PATH (0.1.16); restart the app after installing.

**"Not signed in" / "Your Azure session has expired. Sign in again and
rescan".**
Use **Sign in again** (or `az login`), then rescan.

**"No Log Analytics workspaces in any of your subscriptions."**
The signed-in user can't see one. Check `az account list` covers the right
tenant and subscriptions; with PIM, activate the role first.

**A second window fails to start on Windows.**
Fixed in 0.1.26: each window gets its own web view profile.

## Scanning a workspace

**The scan is denied ("access denied").**
The user needs **Log Analytics Reader** on the workspace. **Grant myself Log
Analytics Reader** works only for someone who can already assign roles there.
A new assignment can take a few minutes to apply.

**"No tables found in this workspace."**
No table holds data in the current window. Widen the window, or check the
workspace is the one the applications actually send to.

**"throttled by Azure — retry in Ns".**
Log Analytics limits query rates; wait and retry. Other dashboards or
scripts querying the same workspace share the limit.

**"the query timed out — narrow the time range and try again".**
Exactly that: a shorter window means less data to scan.

**The picture looks stale after changing the time range.**
Scans are cached per workspace and per window; switching the window rescans.
A corrupt cache file only costs a fresh scan (0.1.26).

## Setup: the proposed key

**"Nothing in the sample linked tables on its own."**
No column carries the same values across tables in the window. The tables
may not share an identifier, or the window holds too little traffic. Widen
the window or pick the key by hand.

**The proposed key is wrong.**
Change it in Setup ⚙: every proposal shows its evidence and is a dropdown.
Values that are too short, or appear in too many columns, are ignored as
identifiers, but an unusual schema can still mislead it.

**"No correlation key chosen yet — pick one in Setup ⚙ first."**
Open Setup ⚙ and pick one.

## Signals, Logs and Dependencies

**A query fails naming a column** (for example `Level`, `OperationId`, or a
lowercased name).
Fixed in 0.1.30 (column names are queried with each table's own spelling, and
a severity filter only queries tables that have it) and 0.1.31 (tables no
longer appear to have their neighbours' columns). Update; old scans are
cleaned when they load.

**Signals shows no failures line.**
No error rules are defined: what counts as a failure is the user's call. Add
them in Setup ⚙.

**Signals or Dependencies shows no latency percentiles.**
No column in the workspace reads as a duration.

**"No table here carries both the correlation key and rows to count. Pick a
different key in Setup."**
The chosen key isn't in the tables Signals counts from; choose another key.

**Dependencies: "Nothing here records what a call went out to".**
No column reads as a call target (`Target`, `RemoteHost` and similar). The
tab needs one alongside the correlation key. Before 0.1.31, a column that
only mentioned a host (`HostInstanceId`) could be picked by mistake.

**An error rule seems ignored for one table.**
A rule naming a column that table doesn't have is left out of that table's
count (0.1.30), rather than failing the whole query.

## Issues

**"This workspace has no AppExceptions table, so there is nothing to read."**
That table comes with a workspace-based Application Insights component. A
classic (non-workspace) component, or none, means no exceptions here.

**"Nothing repeated in this window."**
One-off exceptions are filtered out on purpose. Widen the window for
something rarer.

**"Application Insights sampling is on for this app."**
Counts are estimates, and a given request's telemetry may not have been
kept at all. A trace that stops early can be sampling, not a broken flow.

**"Unresolved Key Vault references".**
App settings that reference Key Vault but resolve to an empty value at
runtime: the app starts, reads a blank, and fails with an error that never
mentions Key Vault. The usual causes are the app's managed identity lacking
access to the vault, a wrong secret name or version, or network rules on the
vault. Fixing it is in the app's configuration, not in AIS Analytics.

## Tracing a value

**"No row anywhere carries this value, in full or in part. Check the value,
or the key."**
Check the value was pasted whole, that it belongs to the chosen key, and that
the window covers when the flow ran.

**A lane says "Not on this key's path".**
That table doesn't have the key column, so it can't hold the value. Not a
failure.

**A lane stays empty ("awaiting").**
The table has the key but no row with this value: the step hasn't happened,
or the flow doesn't go that way, or (with sampling on) the row wasn't kept.

**Steps from unrelated flows appear in a trace.**
Fixed in 0.1.21: a value is now matched only in the column it was found in.
