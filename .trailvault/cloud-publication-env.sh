#!/bin/sh
test "${GITHUB_REPOSITORY:-}" = "madsbolaris/trailvault-capture-test" || {
  echo "Trailvault fixture publication is restricted to its authorized repository." >&2
  exit 1
}
publication_origin="https://github.com/madsbolaris/trailvault-capture-test"
# The provider's transport proxy must not change the collector's approved destination.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_COUNT=6
export GIT_CONFIG_KEY_0=safe.directory
export GIT_CONFIG_VALUE_0="$PWD"
export GIT_CONFIG_KEY_1=remote.origin.url
export GIT_CONFIG_VALUE_1=
export GIT_CONFIG_KEY_2=remote.origin.url
export GIT_CONFIG_VALUE_2="$publication_origin"
export GIT_CONFIG_KEY_3=remote.origin.pushurl
export GIT_CONFIG_VALUE_3=
export GIT_CONFIG_KEY_4=remote.origin.pushurl
export GIT_CONFIG_VALUE_4="$publication_origin"
export GIT_CONFIG_KEY_5="url.$publication_origin.insteadOf"
export GIT_CONFIG_VALUE_5="$publication_origin"
if [ "$(git remote get-url origin)" != "$publication_origin" ] ||
   [ "$(git remote get-url --push origin)" != "$publication_origin" ]; then
  echo "Trailvault fixture could not pin its approved publication destination." >&2
  exit 1
fi
