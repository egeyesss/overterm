#!/bin/sh
#
# Removes OverTerm and the entries it added to Claude Code's settings.
#
#   curl -fsSL https://raw.githubusercontent.com/egeyesss/overterm/main/uninstall.sh | sh
#
# Set OVERTERM_APPDIR if it was installed somewhere other than
# /Applications.

set -eu

APPS="${OVERTERM_APPDIR:-/Applications}"
DEST="$APPS/OverTerm.app"
# Lowercase, inside the capitalised bundle. The capitalised spelling works
# on an ordinary Mac and fails on a case sensitive filesystem.
BIN="$DEST/Contents/MacOS/overterm"

say() { printf '%s\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

if [ ! -d "$DEST" ]; then
  say "Nothing to do: there is no app at $DEST"
  exit 0
fi
[ -w "$APPS" ] || fail "$APPS is not writable by $(whoami). Run it again with sudo."

osascript -e 'quit app "OverTerm"' >/dev/null 2>&1 || true

# macOS runs none of the app's own code when its bundle is deleted, so the
# Claude Code entries would outlive the thing that reads them. Ask it to
# clean up while it is still here. A failure here must never be a reason
# somebody cannot remove the app.
if [ -x "$BIN" ]; then
  "$BIN" --uninstall-hooks || say "could not remove the Claude Code entries; remove any
whose command mentions overterm from ~/.claude/settings.json"
fi

rm -rf "$DEST"

say "Removed $DEST"
say "Settings are still at ~/.config/overterm if you want them gone too."
