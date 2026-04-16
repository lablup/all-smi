#!/bin/bash
# Script to determine the next PPA version number by checking existing versions

set -e

VERSION="$1"
DISTRO="$2"
PPA="$3"

if [ -z "$VERSION" ] || [ -z "$DISTRO" ] || [ -z "$PPA" ]; then
    echo "Usage: $0 <version> <distro> <ppa>"
    echo "Example: $0 0.7.2 noble lablup/backend-ai"
    exit 1
fi

get_highest_revision() {
    local base_version="$1"
    local distro="$2"
    local ppa="$3"
    local existing_versions=""
    local ppa_owner
    local ppa_name

    fetch_versions_for_status() {
        local status="$1"
        local api_url

        api_url="https://api.launchpad.net/1.0/~${ppa_owner}/+archive/ubuntu/${ppa_name}?ws.op=getPublishedSources&source_name=all-smi&distro_series=https://api.launchpad.net/1.0/ubuntu/${distro}&status=${status}"

        curl -s "$api_url" | \
            grep -o '"source_package_version": "[^"]*"' | \
            cut -d'"' -f4 | \
            grep -E "^${base_version}-[0-9]+~${distro}[0-9]+$" || true
    }

    ppa_owner=$(echo "$ppa" | cut -d'/' -f1)
    ppa_name=$(echo "$ppa" | cut -d'/' -f2)
    existing_versions=$(
        {
            fetch_versions_for_status "Published"
            fetch_versions_for_status "Pending"
        } | sort -u
    )

    if [ -z "$existing_versions" ]; then
        echo "1"
        return
    fi

    highest=0
    for ver in $existing_versions; do
        revision="${ver##*~${distro}}"
        if [ -n "$revision" ] && [ "$revision" -gt "$highest" ]; then
            highest=$revision
        fi
    done

    echo $((highest + 1))
}

next_revision=$(get_highest_revision "$VERSION" "$DISTRO" "$PPA")
echo "${VERSION}-1~${DISTRO}${next_revision}"
