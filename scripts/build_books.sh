#!/usr/bin/env bash
#
# Build, check, and stage the shipped opening books for every mode.
#
# What this wraps is a five-step sequence per mode -- survey, deepen, compact,
# export, check -- that has to be run in that order, with tapers that interlock
# and with the same weights throughout. Doing it by hand is how a book ends up
# half-built under one set of weights and half under another.
#
# The build is resumable. `nodes.log` is append-only, so an interrupted run
# keeps everything it finished, and running it again extends the same book
# rather than restarting it. --budget is what a single run may spend, not a
# total the book remembers: run it twice at 80e9 and the book has had 160e9.
#
#   scripts/build_books.sh                       every mode, default budget
#   scripts/build_books.sh --budget 20e9 v5      one mode, quick pass
#   scripts/build_books.sh --publish             build, then PUT to the website
#
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "${HERE}")"
cd "${ROOT}"

# Twelve of fourteen cores. The last two are what keeps the machine usable, and
# the throughput curve is flat enough by then that they cost little: the tables
# are small on purpose (see run_build in src/bin/book.rs) so the workers are
# core-bound rather than bandwidth-bound.
DEFAULT_THREADS="$(( $(sysctl -n hw.ncpu 2>/dev/null || nproc) - 2 ))"
[ "${DEFAULT_THREADS}" -lt 1 ] && DEFAULT_THREADS=1

MODES=()
BUDGET=80000000000
SURVEY_BUDGET=12000000000
MAX_PLY=50
THREADS="${DEFAULT_THREADS}"
HASH_MB=64
# Empty means the book binary's own default, four times the thread count. A
# batch has to hold several positions per worker: it is dealt out longest-job-
# first and claimed rather than assigned, but a batch of exactly one position
# per worker has nothing to hide the expensive positions behind, and the whole
# batch then takes as long as its most expensive member. One position per worker
# measured 3.5 Mnps against a 42.5 Mnps peak.
BATCH=""
# Deep enough that nothing is truncated: the books themselves reach ply 25, and
# the graph export is bounded by the shortest line to a position rather than by
# a per-line depth. Nothing here is downloaded by a browser -- the website
# stores the graph and serves one position at a time -- so there is no reason to
# cap it below what the book actually knows.
PLIES=40
# The openings shipped with the website's first response, and how far each is
# followed. Everything else is fetched when somebody asks for it.
FEATURED=3
FEATURED_PLIES=12
# The certainty bar the export measures its `certified` list against. This is
# what the website leads with, and none of it is in score units: a margin in
# centipawns would need refitting every time the evaluation is re-tuned, since
# re-tuning moves every score in a mode at once. See `certified_openings` in
# src/book.rs.
CERTAIN_DEPTH=14
CERTAIN_PLIES=8
CERTAIN_MIN=2
CERTAIN_MAX=8
OUT_DIR="book/export"
PUBLISH=0
SKIP_SURVEY=0
# The shape of the deep pass. `openings.taper` is built for the website's
# opening explorer, which wants breadth; `play.taper` is built for an engine
# that is going to play from the result, which wants every band above what a
# clock can buy. See the header of either file.
DEEP_TAPER=scripts/openings.taper
FRESH=0

number() {
  # Accepts 80e9 and 80000000000 alike; the exponent form is what anyone
  # actually types for a node budget.
  printf '%.0f' "$(awk -v value="$1" 'BEGIN { printf "%.0f", value + 0 }')"
}

usage() {
   sed -n '3,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  cat <<'USAGE'
options:
  --budget <nodes>         deep pass, per mode          default: 80e9
  --survey-budget <nodes>  survey pass, per mode        default: 12e9
  --max-ply <n>            deepest line to build        default: 50
  --threads <n>            parallel searches            default: cores - 2
  --hash-mb <n>            transposition table each     default: 64
  --batch <n>              positions per dispatch       default: 4x --threads
  --plies <n>              export depth                 default: 40
  --featured <n>           openings shipped up front    default: 3
  --featured-plies <n>     how far each is followed     default: 12
  --certain-depth <n>      depth a certified claim needs default: 14
  --certain-plies <n>      longest certified line       default: 8
  --certain-min <n>        shortest certified line      default: 2
  --certain-max <n>        certified openings at most   default: 8
  --out-dir <path>         export destination           default: book/export
  --deep-taper <file>      shape of the deep pass, default scripts/openings.taper
                           (scripts/play.taper builds a book to play from)
  --skip-survey            deepen only, no root survey
  --fresh                  archive any existing book first
  --publish                PUT each export to $RPS_API_URL
  modes                    v3 / v5 / v6 / infiltration / total-war /
                           intransitive, default: all three
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --budget) BUDGET="$(number "$2")"; shift 2 ;;
    --survey-budget) SURVEY_BUDGET="$(number "$2")"; shift 2 ;;
    --max-ply) MAX_PLY="$2"; shift 2 ;;
    --threads) THREADS="$2"; shift 2 ;;
    --hash-mb) HASH_MB="$2"; shift 2 ;;
    --batch) BATCH="$2"; shift 2 ;;
    --plies) PLIES="$2"; shift 2 ;;
    --featured) FEATURED="$2"; shift 2 ;;
    --featured-plies) FEATURED_PLIES="$2"; shift 2 ;;
    --certain-depth) CERTAIN_DEPTH="$2"; shift 2 ;;
    --certain-plies) CERTAIN_PLIES="$2"; shift 2 ;;
    --certain-min) CERTAIN_MIN="$2"; shift 2 ;;
    --certain-max) CERTAIN_MAX="$2"; shift 2 ;;
    --out-dir) OUT_DIR="$2"; shift 2 ;;
    --deep-taper) DEEP_TAPER="$2"; shift 2 ;;
    --skip-survey) SKIP_SURVEY=1; shift ;;
    --fresh) FRESH=1; shift ;;
    --publish) PUBLISH=1; shift ;;
    -h|--help) usage; exit 0 ;;
    v3|V3|infiltration) MODES+=(infiltration); shift ;;
    v5|V5|total-war) MODES+=(total-war); shift ;;
    v6|V6|intransitive) MODES+=(intransitive); shift ;;
    *) echo "error: unknown argument $1" >&2; usage >&2; exit 2 ;;
  esac
done
[ "${#MODES[@]}" -eq 0 ] && MODES=(infiltration total-war intransitive)

BOOK="${ROOT}/target/release/book"
echo "Building the book binary..."
cargo build --release --bin book
mkdir -p "${OUT_DIR}"

# The weight fingerprint of *this* binary, read from a throwaway book rather
# than hand-copied. A book refuses records made under different weights, which
# is right, but it refuses them with an error at the first command -- so the
# fingerprint has to be known before the run starts, not discovered by it.
PROBE="$(mktemp -d)"
trap 'rm -rf "${PROBE}"' EXIT
WEIGHTS="$("${BOOK}" stats --dir "${PROBE}/probe" --mode infiltration |
  awk '$1 == "weights" { print $2 }')"
[ -n "${WEIGHTS}" ] || { echo "error: could not read the weight fingerprint" >&2; exit 1; }

echo
echo "weights   ${WEIGHTS}"
echo "modes     ${MODES[*]}"
echo "threads   ${THREADS} x ${HASH_MB} MiB, batch ${BATCH:-4x threads}"
echo "budget    ${SURVEY_BUDGET} survey + ${BUDGET} deep nodes per mode"
echo "taper     ${DEEP_TAPER} for the deep pass"
echo "certainty depth ${CERTAIN_DEPTH}+, ${CERTAIN_MIN}-${CERTAIN_PLIES} plies, ${CERTAIN_MAX} openings"

for MODE in "${MODES[@]}"; do
  case "${MODE}" in
    infiltration) ID=V3 ;;
    total-war) ID=V5 ;;
    intransitive) ID=V6 ;;
  esac
  DIR="book/${ID}"
  LOG="${DIR}/build.log"

  echo
  echo "=============================================================="
  echo " ${ID} (${MODE})"
  echo "=============================================================="

  # A weights change invalidates every score in the book, so the old one cannot
  # be extended -- but it is also the only record of what the previous engine
  # thought, and the log is the authoritative artifact. Move it aside rather
  # than deleting it, and rather than stopping the whole run on an error the
  # operator can do nothing about.
  if [ -f "${DIR}/meta.txt" ]; then
    WAS="$(awk '$1 == "params" { print $2 }' "${DIR}/meta.txt")"
    if [ "${FRESH}" -eq 1 ] || { [ -n "${WAS}" ] && [ "${WAS}" != "${WEIGHTS}" ]; }; then
      ARCHIVE="book/archive/${ID}-${WAS:-unknown}-$(date +%Y%m%d-%H%M%S)"
      mkdir -p "$(dirname "${ARCHIVE}")"
      mv "${DIR}" "${ARCHIVE}"
      if [ "${WAS}" != "${WEIGHTS}" ]; then
        echo "archived  ${DIR} -> ${ARCHIVE} (built under weights ${WAS:-unknown})"
      else
        echo "archived  ${DIR} -> ${ARCHIVE} (--fresh)"
      fi
    fi
  fi
  mkdir -p "${DIR}"

  # Intransitive's goal is a corner, so reversing files is not a symmetry of
  # its rules and `Position::canonical_over` refuses to fold by it whatever the
  # book is configured with. Asking for `none` there only makes meta.txt say
  # what the book actually did; the default `files` would be equally correct
  # and would read as though it had merged something.
  SYMMETRY=files
  [ "${MODE}" = "intransitive" ] && SYMMETRY=none

  # Every command below reopens the book, and a book refuses to be opened under
  # a fold other than the one it was written with -- so all of them are passed
  # this, not just the two build passes. Leaving it off cost nothing while both
  # books were `files`, and stops a corner-goal mode dead the moment its build
  # finishes: "symmetry mismatch: book has none, this run has files", from
  # compact, after the whole budget has already been spent.

  COMMON=(--dir "${DIR}" --mode "${MODE}" --threads "${THREADS}"
          --hash-mb "${HASH_MB}" --symmetry "${SYMMETRY}")
  [ -n "${BATCH}" ] && COMMON+=(--batch "${BATCH}")

  if [ "${SKIP_SURVEY}" -eq 0 ]; then
    echo
    echo "--- pass 1: survey every opening move ---"
    "${BOOK}" build "${COMMON[@]}" --max-ply 1 \
      --taper scripts/survey.taper --budget "${SURVEY_BUDGET}" | tee -a "${LOG}"
  fi

  echo
  echo "--- pass 2: deepen toward ply ${MAX_PLY} ---"
  "${BOOK}" build "${COMMON[@]}" --max-ply "${MAX_PLY}" \
    --taper "${DEEP_TAPER}" --budget "${BUDGET}" | tee -a "${LOG}"

  echo
  echo "--- compact, and what the book now says ---"
  "${BOOK}" compact --dir "${DIR}" --mode "${MODE}" --symmetry "${SYMMETRY}" | tee -a "${LOG}"
  "${BOOK}" stats --dir "${DIR}" --mode "${MODE}" --symmetry "${SYMMETRY}" | tee -a "${LOG}"
  echo
  "${BOOK}" show --dir "${DIR}" --mode "${MODE}" --symmetry "${SYMMETRY}" --plies 24 --width 8 | tee -a "${LOG}"

  JSON="${OUT_DIR}/${ID}-openings.json"
  echo
  echo "--- export ---"
  "${BOOK}" export --dir "${DIR}" --mode "${MODE}" --symmetry "${SYMMETRY}" --format graph \
    --plies "${PLIES}" --featured "${FEATURED}" --featured-plies "${FEATURED_PLIES}" \
    --certain-depth "${CERTAIN_DEPTH}" --certain-plies "${CERTAIN_PLIES}" \
    --certain-min "${CERTAIN_MIN}" --certain-max "${CERTAIN_MAX}" \
    --output "${JSON}"
  node scripts/check_export.mjs "${JSON}" --mode "${ID}" --weights "${WEIGHTS}"

  # What the website will actually lead with. Printed after the export because
  # an empty list is the outcome worth noticing: it means this scan has not
  # separated the mode's openings from each other, and more budget rather than
  # a lower bar is the answer.
  echo
  "${BOOK}" certify --dir "${DIR}" --mode "${MODE}" --symmetry "${SYMMETRY}" \
    --certain-depth "${CERTAIN_DEPTH}" --certain-plies "${CERTAIN_PLIES}" \
    --certain-min "${CERTAIN_MIN}" --certain-max "${CERTAIN_MAX}" | tee -a "${LOG}"

  # The import is one PUT of the whole graph -- around ten megabytes, since it
  # carries every position the book knows rather than a capped slice. A reverse
  # proxy answers 413 on its own body limit long before the handler's 64 MiB.
  # The shipped nginx config raises it to 32m; a proxy that has not been
  # reloaded since is still on nginx's 1m default, and the failure reads like
  # the API being down rather than like a file being too large.
  BYTES="$(wc -c <"${JSON}" | tr -d ' ')"
  printf 'size      %s bytes (the website never downloads this; it stores it)\n' "${BYTES}"
  if [ "${BYTES}" -gt 33554432 ]; then
    echo "note      over the 32m the shipped nginx config allows. Raise" >&2
    echo "          client_max_body_size in backend/deploy/nginx-api-rps.conf." >&2
  elif [ "${BYTES}" -gt 1048576 ]; then
    echo "note      over nginx's 1m default. The deployed proxy needs the" >&2
    echo "          client_max_body_size from backend/deploy/nginx-api-rps.conf" >&2
    echo "          to have been reloaded, or this PUT comes back 413." >&2
  fi

  if [ "${PUBLISH}" -eq 1 ]; then
    : "${RPS_API_URL:?--publish needs RPS_API_URL}"
    : "${RPS_ADMIN_TOKEN:?--publish needs RPS_ADMIN_TOKEN}"
    echo
    echo "--- publish ---"
    curl --fail-with-body -sS -X PUT \
      -H 'Content-Type: application/json' \
      -H "Authorization: Bearer ${RPS_ADMIN_TOKEN}" \
      --data-binary "@${JSON}" \
      "${RPS_API_URL}/api/admin/openings/${ID}"
    echo
  fi
done

echo
echo "=============================================================="
ls -l "${OUT_DIR}"
if [ "${PUBLISH}" -eq 0 ]; then
  cat <<PUBLISH

Publish with:
  RPS_API_URL=... RPS_ADMIN_TOKEN=... scripts/build_books.sh --publish --skip-survey --budget 0
This is the only way in: the website curates names, not scans.
PUBLISH
fi
