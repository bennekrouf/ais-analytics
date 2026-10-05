---
name: ais-analytics
description: Help someone use AIS Analytics, the desktop app that follows one correlation id across the tables of an Azure Log Analytics workspace and shows rate, failures, logs, dependencies and recurring exceptions. Use this whenever the user mentions AIS Analytics or ais-analytics, or is tracing a correlation id or operation id through Log Analytics or Application Insights tables and asks about the proposed correlation key, empty lanes, Signals, Logs, Dependencies or Issues, KQL errors from the app, the time window, a denied scan (Log Analytics Reader), throttling, query timeouts, Application Insights sampling or unresolved Key Vault references. Also use it when they want to report an AIS Analytics bug or ask the author for help.
---

# AIS Analytics

AIS Analytics points at an Azure Log Analytics workspace and works out, from
the data rather than from configuration, which column ties the steps of one
flow together. Paste a correlation id and it draws where that value got to
and where it didn't. Around that it adds views for finding the id in the
first place: what's failing, the log stream, outbound calls, recurring
exceptions. It is the Log Analytics sibling of AIS Tracing, which does the
same against Cosmos DB.

It has no login of its own: it uses the Azure CLI session (`az login`) and
the signed-in user's access to the workspace. The person you are helping is
usually a developer or someone on support, looking at a real, often client's,
system.

## How it works

- **Pick a workspace.** The welcome screen lists the Log Analytics workspaces
  in all the user's subscriptions.
- **Time window** (toolbar, "How far back to read"). Every Log Analytics query
  is bounded, so the window decides which tables even appear to hold data. A
  scan is cached per workspace *and* per window: changing the window rescans.
- **Tabs**, in the order they're meant to be used:
  - **Signals**: request rate, failures and latency over the window, with
    operations ranked by failures; each row opens its run in Trace. Failures
    come from the user's error rules; with none, or no duration column, the
    tab says so next to the setting that fixes it. The app opens here.
  - **Logs**: the log stream, filtered by severity and text, every line
    leading to its run.
  - **Dependencies**: Signals grouped by call target: what the app calls out
    to, how often it fails, how long it takes.
  - **Issues**: what's failing repeatedly: recurring exceptions (one-offs are
    filtered out), **unresolved Key Vault references**, and a warning when
    Application Insights sampling is on.
  - **Trace**: paste a value and see each table (or step) as a lane.
  - **Setup ⚙**: what the app worked out, and where to disagree.
- **Setup ⚙.** The app reads the workspace's table schema, samples the tables
  with data in the window, and proposes the **correlation key** (decided from
  shared values, so `OperationId`, `correlationId_g` and `job_ref` in a
  custom `_CL` table are recognised as one key; standard Azure Monitor names
  are only a tie-breaker), the **order by** column, a **step label**, and
  **error rules** (rows matching them are drawn in red). Each one shows its
  evidence and is a plain dropdown the user can override.
- **Trace lanes** end up as: **reached** (the value is there), **awaiting**
  (the table has the key but not this value: it hasn't arrived), **not on
  this key's path** (the table doesn't have the key, so empty means nothing),
  or **failed** (the query couldn't run; the reason is shown). A value is
  matched only in the column it was found in.

## Permissions

Reading the workspace needs **Log Analytics Reader** on it. The app offers
**Grant myself Log Analytics Reader**, which only works if the user can
already assign roles there; otherwise whoever administers the workspace has
to grant it.

## When something fails

1. **Get the exact message** from the screen.
2. **Check the session**: "Your Azure session has expired. Sign in again and
   rescan" means exactly that. `az account show` says who is signed in.
3. **Check the window**: many "nothing found" results are a window that
   doesn't cover when the flow ran. Widen it.
4. **Look up the symptom** in `references/troubleshooting.md`. Many "the
   trace is wrong" reports are a key choice or the window, not a bug.
5. If it's still unexplained, check the version (window title or update
   banner): the release notes at
   <https://mayorana.ch/en/apps/ais-analytics/releases> say which version
   fixed what. Then offer to draft a report (below).

## Reporting a problem to the author

When the problem looks like an AIS Analytics bug, or the user wants to send
feedback, help them write a report they can send. Go through "When something
fails" first, even when the user asks straight for a report: a permission,
window or key-choice cause on their side wastes their time and the author's,
and finding it is more useful to them than a report. The user sends it, not
you: never create an issue, send an email or submit a form on their behalf.

Log rows contain workspace and resource names, host names, correlation ids,
URLs, user identifiers, exception messages and sometimes secrets logged by
mistake. Before showing the draft, replace anything like that with neutral
placeholders (`<workspace>`, `<table>`, `<host>`, `<id>`), and describe
columns and tables by name rather than pasting rows; table and column names
from Azure itself (`AppRequests`, `OperationId`) are fine to keep. Tell the
user what you replaced and ask them to check the rest.

Use this structure:

~~~markdown
**AIS Analytics version:** 0.1.33
**OS:** macOS 15.1 / Windows 11 / Ubuntu 24.04
**Time window:** last 24 hours / …

**Setup**
Correlation key: <column> · Order by: <column> · Step label: <column>

**Where**
Signals / Logs / Dependencies / Issues / Trace / Setup

**What I did**
1. …

**What I expected**
…

**What happened instead**
…

**Message shown**
```
…
```

**Workaround found, if any**
…
~~~

Then give them the two ways to send it:

- A GitHub issue at <https://github.com/bennekrouf/ais-analytics/issues/new>:
  they paste the title and body and submit it themselves. Issues there are
  public, which is one more reason the draft must be scrubbed.
- The contact form at <https://mayorana.ch/en/contact>, for anything they
  would rather not post publicly.
