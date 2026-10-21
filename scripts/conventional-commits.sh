#!/usr/bin/env bash
# Conventional Commits rules (v0.2). Every merge to main is tagged with the
# version in Cargo.toml, so the PR, not CI, owns the bump. Each PR's commits and
# title must follow https://www.conventionalcommits.org, and the PR must move the
# version by exactly the largest bump those messages call for.
#
#   scripts/conventional-commits.sh lint    <base> <head> [pr-title]
#   scripts/conventional-commits.sh version <base> <head> [pr-title]
#   scripts/conventional-commits.sh level   <base> <head>  # none|patch|minor|major
#   scripts/conventional-commits.sh cargo-version <rev>
#
# <base>..<head> is the PR's own commits; merge commits are skipped.
set -euo pipefail

types='build|chore|ci|docs|feat|fix|perf|refactor|revert|style|test'
conventional="^($types)(\([^()]+\))?!?: [^ ]"
bang='^[a-z]+(\([^()]+\))?!: '
feature='^feat(\([^()]+\))?: '
fix='^(fix|perf)(\([^()]+\))?: '
levels=(none patch minor major)

fail() {
  if [[ -n ${GITHUB_ACTIONS-} ]]; then
    echo "::error::$*"
  else
    echo "error: $*" >&2
  fi
}

commits() { git rev-list --no-merges --reverse "$1..$2"; }

# Bump a message calls for, as an index into $levels.
level_of() {
  local subject=$1 body=$2
  if [[ $subject =~ $bang ]] || grep -qE '^BREAKING[ -]CHANGE: ' <<<"$body"; then
    echo 3
  elif [[ $subject =~ $feature ]]; then
    echo 2
  elif [[ $subject =~ $fix ]]; then
    echo 1
  else
    echo 0
  fi
}

# Largest bump among the PR's commits and its title (the merge commit and the
# release notes carry the title).
max_level() {
  local max=0 l sha
  if [[ -n ${3-} ]]; then
    max=$(level_of "$3" "")
  fi
  while read -r sha; do
    l=$(level_of "$(git log -1 --format=%s "$sha")" "$(git log -1 --format=%b "$sha")")
    if ((l > max)); then
      max=$l
    fi
  done < <(commits "$1" "$2")
  echo "$max"
}

cargo_version() {
  git show "$1:Cargo.toml" |
    awk '/^\[/ { pkg = ($0 == "[package]") }
         pkg && /^version *=/ { gsub(/^version *= *"|".*$/, ""); print; exit }'
}

bump() {
  local major minor patch
  IFS=. read -r major minor patch <<<"$1"
  case $2 in
    3) echo "$((major + 1)).0.0" ;;
    2) echo "$major.$((minor + 1)).0" ;;
    1) echo "$major.$minor.$((patch + 1))" ;;
    *) echo "$1" ;;
  esac
}

lint() {
  local base=$1 head=$2 title=${3-} bad=0 sha subject
  if [[ -n $title && ! $title =~ $conventional ]]; then
    fail "PR title is not a conventional commit: $title"
    bad=1
  fi
  while read -r sha; do
    subject=$(git log -1 --format=%s "$sha")
    if [[ ! $subject =~ $conventional ]]; then
      fail "commit ${sha:0:7} is not a conventional commit: $subject"
      bad=1
    fi
  done < <(commits "$base" "$head")
  if ((bad)); then
    echo "Use <type>[(scope)][!]: <description>, with type one of ${types//|/, }." >&2
    echo "Reword commits with git rebase -i; edit the PR title on GitHub." >&2
    return 1
  fi
  echo "conventional commits ok"
}

check_version() {
  local base=$1 head=$2 title=${3-}
  local from have level want
  from=$(cargo_version "$base")
  have=$(cargo_version "$head")
  level=$(max_level "$base" "$head" "$title")
  want=$(bump "$from" "$level")
  if [[ $have != "$want" ]]; then
    fail "Cargo.toml version is $have; this PR's largest change is a ${levels[level]} bump from $from, so set version = \"$want\" and run cargo update --workspace."
    return 1
  fi
  echo "version $have ok (${levels[level]} bump from $from)"
}

case ${1-} in
  lint) lint "${@:2}" ;;
  version) check_version "${@:2}" ;;
  level) echo "${levels[$(max_level "$2" "$3")]}" ;;
  cargo-version) cargo_version "$2" ;;
  *)
    echo "usage: $0 lint|version|level|cargo-version <args> (see the header)" >&2
    exit 2
    ;;
esac
