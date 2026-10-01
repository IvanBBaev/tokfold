#!/bin/sh
# Regenerates the JSONTestSuite seeds under seeds/<target>/jsonts-*.
#
# Usage, from this directory (fuzz/):
#
#   ./import-jsontestsuite.sh /path/to/JSONTestSuite
#
# The argument is a clone of https://github.com/nst/JSONTestSuite. The seeds in
# this repository were generated from the commit recorded in
# seeds/JSONTestSuite.NOTICE; check that clone out at that commit to reproduce them
# byte for byte, and update the NOTICE when importing a different commit.
#
# Every file in test_parsing/ is imported except the two listed in SKIP below (see
# README.md, "Seeds and dictionary", for why). Each target gets the case prefixed
# with the selector byte it needs:
#
#   mcp_json         no selector (the target reads the whole input as text)
#   tape_parse       0x1F: a depth limit of 31, so the bounded and the unbounded
#                    parse both run
#   roundtrip        0x00: the conservative profile, no overrides
#   mcp_handle_line  0x00: the 32 MiB frame limit; each line of the case is one
#                    message
set -eu

src="${1:?usage: $0 /path/to/JSONTestSuite}"
dir="$src/test_parsing"
test -d "$dir"
test -d seeds

SKIP="n_structure_100000_opening_arrays.json n_structure_open_array_object.json"

for t in mcp_json tape_parse roundtrip mcp_handle_line; do
  rm -f "seeds/$t"/jsonts-*
done

for f in "$dir"/*.json; do
  name="$(basename "$f")"
  case " $SKIP " in
    *" $name "*) continue ;;
  esac
  cp "$f" "seeds/mcp_json/jsonts-$name"
  { printf '\037'; cat "$f"; } > "seeds/tape_parse/jsonts-$name"
  { printf '\000'; cat "$f"; } > "seeds/roundtrip/jsonts-$name"
  { printf '\000'; cat "$f"; } > "seeds/mcp_handle_line/jsonts-$name"
done
