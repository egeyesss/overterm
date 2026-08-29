#!/bin/sh
#
# Installs oTerm into /Applications.
#
#   curl -fsSL https://raw.githubusercontent.com/egeyesss/overterm/main/install.sh | sh
#
# Set OVERTERM_VERSION to a tag to install that release instead of the
# newest one. Set OVERTERM_APPDIR to install somewhere other than
# /Applications.

set -eu

REPO="egeyesss/overterm"
APP_NAME="oTerm.app"
TARBALL="oTerm_universal.tar.gz"
APPS="${OVERTERM_APPDIR:-/Applications}"
DEST="$APPS/$APP_NAME"
VERSION="${OVERTERM_VERSION:-latest}"

say() { printf '%s\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || fail "oTerm only runs on macOS so far."
command -v curl >/dev/null 2>&1 || fail "curl is needed and is not installed."
command -v shasum >/dev/null 2>&1 || fail "shasum is needed and is not installed."

# A release is published under its tag, and the newest one also answers on
# a fixed URL. Release candidates are marked as prereleases so they never
# show up as the newest.
if [ "$VERSION" = latest ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

# /Applications is writable by an admin user on most Macs and not on all of
# them. Finding out now is better than finding out later, halfway through,
# with the old copy already deleted.
[ -d "$APPS" ] || fail "$APPS does not exist."
if [ ! -w "$APPS" ]; then
  fail "$APPS is not writable by $(whoami). Run it again with sudo:

  curl -fsSL https://raw.githubusercontent.com/$REPO/main/install.sh | sudo sh"
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

say "Downloading oTerm ($VERSION)"
curl -fsSL -o "$work/$TARBALL" "$base/$TARBALL" ||
  fail "could not download $base/$TARBALL"
curl -fsSL -o "$work/$TARBALL.sha256" "$base/$TARBALL.sha256" ||
  fail "could not download the checksum for $TARBALL"

# This catches a download that arrived damaged or truncated. It cannot tell
# you the release itself is honest, because the checksum comes from the
# same place the app does. The README says so plainly.
say "Checking the download"
(cd "$work" && shasum -a 256 -c "$TARBALL.sha256" >/dev/null) ||
  fail "the download does not match its checksum. Try again."

say "Unpacking"
tar -xzf "$work/$TARBALL" -C "$work"
[ -d "$work/$APP_NAME" ] || fail "the download did not contain $APP_NAME."

# Swapping the bundle underneath a running copy leaves it in a strange
# state, so ask it to go first. It not running is the normal case.
osascript -e 'quit app "oTerm"' >/dev/null 2>&1 || true

# Unpacking over the top of an old copy keeps whatever the old one had and
# the new one does not, so replace it outright. Everything above has
# already succeeded by this point, so the window where there is no app
# installed is one rename wide.
rm -rf "$DEST"
# The app was called OverTerm.app before it was called oTerm.app, so an
# upgrade would otherwise leave two copies in /Applications, one of them
# dead. Only removed once the new one is on disk and checked.
rm -rf "$APPS/OverTerm.app"
mv "$work/$APP_NAME" "$DEST"

say ""
say "Installed to $DEST"
say "Open it from Finder or Spotlight. Cmd+Shift+O summons and hides it."
say ""
say "On first launch it adds four entries to ~/.claude/settings.json so it"
say "can read Claude Code's own events. The settings sheet turns that off,"
say "and uninstall.sh takes the entries with it."
