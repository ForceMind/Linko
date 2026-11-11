#!/usr/bin/env bash
set -euo pipefail

if [[ "${RUNNER_ENVIRONMENT:-}" != github-hosted || "${RUNNER_OS:-}" != Linux ]]; then
    echo 'This step requires a GitHub-hosted Linux runner.' >&2
    exit 1
fi

# Preserve the runner's Ubuntu suites and Signed-By configuration. Only replace
# its mirror list when the stock Azure and Canonical hosts cannot be resolved.
mirror_list=/etc/apt/apt-mirrors.txt
if [[ -r "$mirror_list" ]] && grep -qF azure.archive.ubuntu.com "$mirror_list" &&
    ! timeout 8s getent ahosts azure.archive.ubuntu.com >/dev/null &&
    ! timeout 8s getent ahosts archive.ubuntu.com >/dev/null &&
    ! timeout 8s getent ahosts security.ubuntu.com >/dev/null; then
    mirror=mirror.its.umich.edu
    echo "Default Ubuntu mirrors did not resolve. Checking Ubuntu archive mirror $mirror."
    timeout 10s getent ahosts "$mirror" >/dev/null
    printf 'https://%s/ubuntu\n' "$mirror" | sudo tee "$mirror_list" >/dev/null
    echo "Using Ubuntu archive mirror $mirror."
fi

sudo apt-get update
sudo apt-get install -y xvfb xdotool imagemagick libxkbcommon-x11-0 libegl1 libgl1-mesa-dri
