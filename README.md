# Yellowcode

Early alpha desktop viewer for 1C configurations, with a read-only query console for file databases.
This repository contains release notes and binary downloads. Application source code is maintained separately.

## Install on macOS Apple Silicon

```sh
brew install --cask belov38/yellowcode/yellowcode
open -a Yellowcode
```

Apple Silicon Macs (M1 and later, including M4) only. The binary targets macOS 11 or later;
older macOS versions have not been tested. Intel Macs, Windows and Linux are not supported by this release.

Alternatively, download the ZIP from [Releases](https://github.com/belov38/yellowcode/releases), unzip it and move
`Yellowcode.app` into Applications.

### First launch

This alpha is not Developer ID signed or notarized. macOS may block the downloaded application.
After attempting to open it, go to **System Settings → Privacy & Security → Open Anyway**, verify that the
application is Yellowcode, and confirm. Do not disable Gatekeeper globally.

## What to try

- Open a `.cf` or `.cfe` to browse metadata, object properties, modules, forms and templates.
- Inspect data composition schemas: datasets, query text, fields, links, resources and parameters.
- Open a copy of `1Cv8.1CD` to use the query console, result tables, parameters, history and query plans.
- Explore syntax help and role permissions.

No 1C installation or Rust toolchain is required. Supply your own configuration or database;
no business data or sample configuration is included in the download. Use an offline copy of the database.

## Alpha limitations

Read-only preview, not a replacement for the 1C platform. Configuration editing, XML-directory import,
the visual query builder and report generation through DCS are not part of this release. Query support
is incomplete; some constructs, including virtual register slices, are not supported. Large configurations
can take time and several GB of memory to open. Please include the app version and reproduction steps in bug reports,
without attaching business data or credentials.

## Updates

```sh
brew update
brew upgrade --cask yellowcode
```

Third-party notices and licenses are included in `Yellowcode.app/Contents/Resources/licenses/`.
