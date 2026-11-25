#!/bin/sh
set -eu
folder=$(CDPATH= cd -P "$(dirname "$0")" && pwd -P)
application="$folder/Base Search.app"
if [ ! -x "$application/Contents/MacOS/base-search" ]; then
    printf '%s\n' 'Could not find Base Search. Extract the entire ZIP and try again.' >&2
    exit 1
fi
exec /usr/bin/open "$@" -a "$application"
