# ais-analytics

Paste a correlation id, see every step it touched. Point it at an Azure Log
Analytics workspace and it works out — from the data, not from a config file —
which column ties the steps of one flow together, then draws where that value
got to and where it didn't.

An empty lane is the point. Knowing a request never reached the invoicing
table is as useful as seeing that it did, so the view distinguishes *hasn't
arrived* from *was never on this path* rather than showing both as nothing.

It is the Log Analytics sibling of
[ais-tracing](https://github.com/bennekrouf/ais-tracing), which does the same
job against Cosmos DB.

## Install

Downloads are on the [latest release](https://github.com/bennekrouf/ais-analytics/releases/latest).

### macOS (Apple Silicon)

Download [`ais-analytics-macos-arm64.dmg`](https://github.com/bennekrouf/ais-analytics/releases/latest/download/ais-analytics-macos-arm64.dmg),
open it, drag **AIS Analytics** to Applications.

### Windows

Download [`ais-analytics-setup.exe`](https://github.com/bennekrouf/ais-analytics/releases/latest/download/ais-analytics-setup.exe)
and run it.

### Linux

```bash
curl -L https://github.com/bennekrouf/ais-analytics/releases/latest/download/ais-analytics-linux-x86_64.tar.gz | tar xz
cd ais-analytics-linux-x86_64
./setup-linux.sh
./ais-analytics
```

## First run

ais-analytics has no login of its own — it uses your Azure CLI session:

```bash
az login
```

Then start the app and pick a workspace. It reads the published table schema,
samples the tables holding data in the selected window, and proposes a
correlation key, a time column, and a step label. Every proposal shows its
evidence, and every one is a plain dropdown you can override.

If the scan comes back denied, you need **Log Analytics Reader** on the
workspace. The app offers to grant it to you, which only works if you can
already assign roles there.

## How it decides

Correlation keys are found by *value*, not by name. Two columns in different
tables carrying the same identifiers are treated as one key even when they're
spelled differently — `OperationId` in App Insights, `correlationId_g` in
`AzureDiagnostics`, `job_ref` in your own `_CL` table.

Names are a weak tie-breaker only. The standard Azure Monitor correlation
columns get a small prior, deliberately worth less than evidence of actually
linking, because the workspaces worth tracing are the ones where half the
tables follow no convention at all.

## Scope

Everything reads through a time window, chosen in the toolbar. That is not a
filter bolted on — every Log Analytics query is bounded, so the window decides
which tables even appear to hold data. A scan is cached per workspace *and per
window*, so switching range rescans rather than showing you a stale picture.

## Development

```bash
cargo test
cargo run
```

## License

See [LICENSE](LICENSE).
