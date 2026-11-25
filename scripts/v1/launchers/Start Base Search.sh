#!/bin/sh
set -eu
folder=$(CDPATH= cd -P "$(dirname "$0")" && pwd -P)
program="$folder/app/base-search"
if [ ! -x "$program" ]; then
    printf '%s\n' 'Could not find Base Search. Extract the entire archive and try again.' >&2
    exit 1
fi
exec "$program" "$@"
