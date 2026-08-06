#!/usr/bin/env bash
# PostToolUse(Write|Edit): block an edit that ADDS a banned token to a .rs or
# .md file. Counts are compared against the committed version, so a file that
# already carries a banned token stays editable and only a net increase blocks.
set -u
# NixOS-only: skip on any other machine.
[ -f /etc/NIXOS ] || exit 0
f=$(jq -r '.tool_input.file_path // .tool_response.filePath // empty' 2>/dev/null) || exit 0
case "$f" in *.rs|*.md) ;; *) exit 0 ;; esac
[ -f "$f" ] || exit 0

block() { printf '{"decision":"block","reason":%s}\n' "$(jq -Rn --arg r "$1" '$r')"; exit 0; }

# $1 = rg flags, $2 = pattern, $3 = message
check() {
  local now head root rel
  now=$(rg -o $1 -- "$2" "$f" 2>/dev/null | wc -l | tr -d ' '); now=${now:-0}
  [ "$now" -eq 0 ] && return 0
  head=0
  if root=$(git -C "$(dirname "$f")" rev-parse --show-toplevel 2>/dev/null); then
    rel=${f#"$root"/}
    head=$(git -C "$root" show "HEAD:$rel" 2>/dev/null | rg -o $1 -- "$2" 2>/dev/null | wc -l | tr -d ' ')
    head=${head:-0}
  fi
  [ "$now" -gt "$head" ] && block "$3 File: $f (was $head, now $now)."
  return 0
}

check -F $'\xe2\x80\x94' "This edit adds an em-dash. House style bans em-dashes: use an ASCII hyphen, a colon, or split the sentence."
# The ASCII alternate excludes a pipe on either side of the run, so a markdown
# table separator in a doc comment is not a banner.
check -N '^.*[─━═]{4,}|^\s*(//|#|<!--|/\*)[^|]*[-=*_]{5,}[^|]*$' "This edit adds a banner or section-divider comment. House style bans banners: a section is a module boundary, split the file or add nothing."
exit 0
