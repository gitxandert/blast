#!/usr/bin/env bash
# A/B pair for a taste question (plans/detailed/SCN.md): renders two variants
# of a scenario and puts them in the blast server's assets folder, so they
# can be played from the assets page on a phone.
#
#   tools/ab.sh NAME                    blast/tests/scenarios/NAME.blast (A)
#                                         against NAME.b.blast beside it (B)
#   tools/ab.sh NAME A.blast B.blast    any two scripts
#
# Each script is rendered with the synthetic assets its `# scn:` line names
# (the same renderer the scenario tests use), into <assets>/ab/NAME-A.wav
# and NAME-B.wav. Prints the two /api/assets/raw URLs and the assets page.
#
# Where the server is comes from ~/.config/blast/server.toml (host, port,
# engine.assets_dir); override with BLAST_URL and BLAST_ASSETS_DIR. The URLs
# printed are, in order: BLAST_URL; else the https URL `tailscale serve`
# gives the server's host:port (a phone can open that one); else the local
# URL, which only this machine can open.
set -euo pipefail

if [ $# -ne 1 ] && [ $# -ne 3 ]; then
  sed -n '2,15p' "$0"
  exit 2
fi
root=$(git rev-parse --show-toplevel)
cd "$root"

name=$1
a=${2:-blast/tests/scenarios/$name.blast}
b=${3:-blast/tests/scenarios/$name.b.blast}
for f in "$a" "$b"; do
  [ -f "$f" ] || { echo "ab.sh: no such script: $f" >&2; exit 1; }
done

conf=${XDG_CONFIG_HOME:-$HOME/.config}/blast/server.toml
# first `key = value` in the config, quotes stripped
conf_get() {
  [ -f "$conf" ] || return 0
  sed -n "s/^$1 *= *\"\{0,1\}\([^\"]*\)\"\{0,1\} *\$/\1/p" "$conf" | head -1
}

assets=${BLAST_ASSETS_DIR:-$(conf_get assets_dir)}
assets=${assets:-$root/blast/assets}
host=$(conf_get host)
port=$(conf_get port)
local_url=http://${host:-127.0.0.1}:${port:-8800}

# the https URL `tailscale serve` maps to the server, or nothing
tailnet_url() {
  command -v tailscale >/dev/null 2>&1 || return 0
  local head name
  # the line above `proxy http://HOST:PORT` is the public URL
  head=$(tailscale serve status 2>/dev/null | awk -v t="${host:-127.0.0.1}:${port:-8800}" '
    /^https?:\/\// { url = $1 }
    /proxy/ && index($0, "//" t) { print url; exit }')
  [ -n "$head" ] || return 0
  # machine name from the status, the port from the mapping
  name=$(tailscale status --json 2>/dev/null | jq -r '.Self.DNSName // empty' 2>/dev/null | sed 's/\.$//')
  if [ -n "$name" ]; then
    local tail=${head#https://}
    case $tail in
      *:*) echo "https://$name:${tail##*:}" ;;
      *) echo "https://$name" ;;
    esac
  else
    echo "$head"
  fi
}

# percent-encodes everything but letters, digits and ._~-
urlencode() {
  local LC_ALL=C s=$1 i c
  for ((i = 0; i < ${#s}; i++)); do
    c=${s:i:1}
    case $c in
      [a-zA-Z0-9._~-]) printf '%s' "$c" ;;
      *) printf '%%%02X' "'$c" ;;
    esac
  done
}

if [ -n "${BLAST_URL:-}" ]; then
  url=$BLAST_URL
else
  url=$(tailnet_url)
  if [ -z "$url" ]; then
    url=$local_url
    echo "ab.sh: note: no tailscale serve mapping for the server; $url only opens on this machine" >&2
  fi
fi
enc=$(urlencode "$name")

mkdir -p "$assets/ab"
out_a="$assets/ab/$name-A.wav"
out_b="$assets/ab/$name-B.wav"

AB_A=$(realpath "$a") AB_B=$(realpath "$b") AB_OUT_A="$out_a" AB_OUT_B="$out_b" \
  cargo test -q -p blast --test scenarios -- --ignored ab_render >/dev/null \
  || { echo "ab.sh: the render failed; run it without -q to see why:" >&2
       echo "  AB_A=$a AB_B=$b AB_OUT_A=... cargo test -p blast --test scenarios -- --ignored ab_render" >&2
       exit 1; }

echo "A  $url/api/assets/raw?path=ab/$enc-A.wav"
echo "B  $url/api/assets/raw?path=ab/$enc-B.wav"
echo "assets page  $url/#assets"

# a running server answers for the files just written
if command -v curl >/dev/null 2>&1; then
  for side in A B; do
    code=$(curl -s -m 3 -o /dev/null -w '%{http_code}' "$local_url/api/assets/raw?path=ab/$enc-$side.wav" || true)
    [ "$code" = 200 ] || echo "ab.sh: warning: $side answered HTTP ${code:-none} (is blast server running at $local_url?)" >&2
  done
fi
